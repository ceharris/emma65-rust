//! A memory-mapped character/color-cell display device.
//!
//! See `doc/memory-mapped-display-device-spec.md` for the full behavioral specification and
//! `doc/memory-mapped-display-device-plan.md` for the design decisions this implementation
//! follows. Summary of the bus-addressable memory map (offsets relative to the device's base
//! address, `cells = columns * rows`):
//!
//! | Region           | Offset        | Size        | Access | Notes                          |
//! |------------------|---------------|-------------|--------|---------------------------------|
//! | Character RAM    | `0`           | `cells`     | R/W    | Glyph index per cell            |
//! | Color RAM        | `cells`       | `cells`     | R/W    | Palette index per cell          |
//! | Control register | `2*cells`     | 1           | R/W    | See [`CONTROL_SWAP_REQUEST`] etc |
//! | Status/data reg. | `2*cells + 1` | 1           | R/W    | Read: status (bit 0 vsync, bit 1 palette-update-accepted, both read-to-clear). Write: a data byte for an in-progress runtime palette update, ignored unless armed via control bit 3 (see [`CONTROL_PALETTE_ARM`]) |
//!
//! The register map above is not IRQ-capable on its own. This device gains IRQ capability only
//! when an optional keyboard data/latch sub-range is configured (`keyboard_address=`, a disjoint
//! 2-byte range elsewhere in the address space, mirroring `Console`'s input half) and a
//! configured break key is received on it -- see `doc/display-keyboard-integration-plan.md`.
//!
//! **Runtime palette updates**: writing [`CONTROL_PALETTE_ARM`] (bit 3) to the control register
//! arms a 4-byte write sequence to the status/data register: `index`, `red`, `green`, `blue`.
//! After the 4th byte, the addressed palette slot (`index` wrapped modulo the palette length, the
//! same rule [`compositing::resolve_palette_index`] applies on the read side) is updated and
//! status bit 1 is set. Writing control bit 3 again -- whether idle or mid-sequence -- (re)starts
//! the sequence from `index`; there is no way to disarm it once armed short of completing or
//! re-starting a sequence. A control write with bit 3 clear leaves the sequence state untouched,
//! so it's safe to write the other control bits (e.g. a swap request) mid-sequence.
//!
//! Compositing (turning character/color RAM plus a palette and glyph font into pixels) lives in
//! [`compositing`] and [`font`]. Configuration wiring (parsing `palette=`/`font=` file
//! attributes) lives in `emulator::config::display`; this module is bus-facing register and
//! buffer-swap behavior only, plus the compiled-in default font/palette fallbacks.
//!
//! **External protocol**: when run outside the debugger (plain `emma65` CLI), a device can
//! optionally stream its frame data to an external peripheral process instead of (or as well
//! as) the debugger's in-process [`DisplayFrame`] sink, via [`CharDisplay::attach_external_transport`].
//! See `doc/char-display-external-protocol.md` for the wire format; [`protocol`] implements it.

pub mod compositing;
pub mod font;
mod protocol;

use self::compositing::Rgb24;
use self::font::Font;
use super::input_buffer::InputBuffer;
use crate::emulator::transport::{Transport, TransportRelay};
use crate::emulator::{AddressRange, IoDevice, LogCategory, LogLevel, LogSender, log_msg};
use tokio::sync::mpsc;

/// A composited frame ready for display: an RGBA byte buffer (`columns * 8` by `rows * 8`
/// pixels, as produced by [`compositing::composite`]) plus the cell dimensions it was
/// composited from -- self-describing since a delivery client has no other way to learn the
/// device's configured grid size until a frame actually arrives.
///
/// Pushed to a device's frame sink once per vsync (design doc §6, §9) -- the device-driven push
/// channel the debugger's display panel bridge task consumes, via [`CharDisplay::attach_frame_sink`].
#[derive(Clone)]
pub struct DisplayFrame {
    /// RGBA bytes, row-major, top row first, 4 bytes per pixel (see [`compositing::composite`]).
    pub pixels: Vec<u8>,
    /// Grid width in cells this frame was composited from.
    pub columns: u32,
    /// Grid height in cells this frame was composited from.
    pub rows: u32,
}

/// Default grid width in cells, matching the spec's default.
pub const DEFAULT_COLUMNS: u32 = 40;
/// Default grid height in cells, matching the spec's default.
pub const DEFAULT_ROWS: u32 = 25;
/// Default vsync cadence in Hz.
pub const DEFAULT_FRAME_RATE_HZ: u32 = 60;
/// Reference clock used to derive vsync cadence when no wall-clock-correlated `clock_hz` is
/// available (i.e. the CPU runs at `ClockSpeed::unlimited()`). Matches the default profile's
/// WDC65C02 clock speed; see design doc §6 for why no cycle-based computation can be wall-clock
/// accurate in that mode regardless of what reference rate is chosen here.
pub const NOMINAL_CLOCK_HZ: u64 = 1_843_200;

/// Control register bit 0: write 1 to request a swap. Self-clearing; always reads 0.
const CONTROL_SWAP_REQUEST: u8 = 0b0000_0001;
/// Control register bit 1: swap-on-vsync enable. R/W, independent of bit 0.
const CONTROL_SWAP_ON_VSYNC: u8 = 0b0000_0010;
/// Control register bit 7: swap pending (read-only).
const CONTROL_SWAP_PENDING: u8 = 0b1000_0000;
/// Control register bit 3: write 1 to (re)start the 4-byte runtime palette-update sequence on
/// the status/data register. Always reads back 0; a write with this bit clear leaves the
/// sequence state untouched (see the module doc comment).
const CONTROL_PALETTE_ARM: u8 = 0b0000_1000;
/// Status register bit 0: vsync flag. Read-to-clear.
const STATUS_VSYNC: u8 = 0b0000_0001;
/// Status register bit 1: a runtime palette-update sequence was just applied. Read-to-clear.
const STATUS_PALETTE_ACCEPTED: u8 = 0b0000_0010;

/// State machine for the in-progress runtime palette-update sequence (module doc comment):
/// accumulates the `index`, `red`, `green` bytes before applying the color on the 4th (`blue`)
/// byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteUpdateState {
    Idle,
    ExpectIndex,
    ExpectRed(u8),
    ExpectGreen(u8, u8),
    ExpectBlue(u8, u8, u8),
}

/// The optional keyboard sub-range's range and buffered input state, bundled together so the
/// two can't drift apart (design doc §1).
struct KeyboardInput {
    range: AddressRange,
    input: InputBuffer,
}

/// A memory-mapped character/color-cell display device.
pub struct CharDisplay {
    name: &'static str,
    address_range: AddressRange,
    columns: u32,
    rows: u32,
    double_buffered: bool,

    /// CPU-addressable buffers. Fixed identity for the device's lifetime — the CPU always
    /// reads/writes these, regardless of swap state (spec §5.1).
    char_ram: Vec<u8>,
    color_ram: Vec<u8>,

    /// Scanout buffers, populated by `perform_swap`. Only meaningful when `double_buffered`;
    /// left empty otherwise since `frame_source()` reads straight from the CPU-addressable
    /// buffers in single-buffered mode.
    scanout_char_ram: Vec<u8>,
    scanout_color_ram: Vec<u8>,

    swap_on_vsync: bool,
    swap_pending: bool,
    status: u8,

    /// In-progress runtime palette-update sequence state (module doc comment).
    palette_update: PaletteUpdateState,

    /// Cycle-accounted vsync cadence (design §6): the fixed number of CPU cycles per frame,
    /// derived once at construction from `clock_hz` (or [`NOMINAL_CLOCK_HZ`] as a fallback) and
    /// `frame_rate_hz`.
    cycles_per_frame: u64,
    cycle_accumulator: u64,
    /// The configured vsync cadence, kept alongside the derived `cycles_per_frame` because the
    /// external protocol header (`protocol::encode_header`) reports it directly.
    frame_rate_hz: u32,

    /// Glyph font and color palette fixed at configuration time (spec §3, §7). Not yet consumed
    /// here -- compositing a frame and pushing it to a debugger-owned sink is added in a later
    /// work unit -- but held now so the config module's loaded values actually take effect on
    /// the instantiated device rather than being parsed and discarded.
    font: Font,
    palette: Vec<Rgb24>,

    /// Push channel for composited frames (design doc §9), set post-construction via
    /// [`Self::attach_frame_sink`] -- `None` when run outside the debugger (plain `emma65`
    /// CLI), in which case vsync never composites anything.
    frame_sink: Option<mpsc::Sender<DisplayFrame>>,

    /// Outbound-only transport for the external display protocol (`doc/char-display-external-
    /// protocol.md`), set post-construction via [`Self::attach_external_transport`] -- `None`
    /// unless a `transport=` attribute is configured (config wiring is a later work unit of the
    /// SDL2 display peripheral plan).
    external_transport: Option<Box<dyn Transport>>,

    /// Optional keyboard data/latch sub-range (design doc §1), set post-construction via
    /// [`Self::with_keyboard_range`] -- `None` unless a `keyboard_address=` attribute is
    /// configured.
    keyboard: Option<KeyboardInput>,

    /// Relay for the keyboard sub-range's inbound byte stream, drained unconditionally every
    /// `tick()` regardless of whether `keyboard` is set. This is a correctness requirement, not
    /// just a capability: the CLI path's relay rides the same [`PipeTransport`] as
    /// `external_transport`, and an undrained relay's channel closing tears down *that entire
    /// transport* -- frames included -- the instant the peripheral sends its first keystroke (see
    /// `doc/display-keyboard-integration-plan.md`'s Context section). Set alongside
    /// `external_transport` (via [`Self::attach_external_transport`]) or alongside
    /// `keyboard_transport` (via [`Self::attach_keyboard_transport`]).
    keyboard_relay: Option<TransportRelay>,

    /// Debugger-path-only transport for the keyboard sub-range, set post-construction via
    /// [`Self::attach_keyboard_transport`]. The CLI path's inbound keyboard stream instead rides
    /// `external_transport`, whose `shutdown()` already tears down both directions of that one
    /// child's stdio.
    keyboard_transport: Option<Box<dyn Transport>>,

    /// Sender for structured diagnostic messages (e.g. `reset()`).
    log_sender: LogSender,
}

impl CharDisplay {
    /// Creates a new device. `address_range` must span exactly `2 * columns * rows + 2` bytes;
    /// callers (the config module) are responsible for computing it from `columns`/`rows`.
    ///
    /// `clock_hz` is the CPU's configured clock speed in Hz, or `None` if the CPU runs
    /// unthrottled (`ClockSpeed::unlimited()`); see [`NOMINAL_CLOCK_HZ`].
    ///
    /// `font` and `palette` are the glyph bitmap and color list fixed at configuration time
    /// (spec §3, §7); `palette` must be non-empty, per spec §3 -- the config module validates
    /// this before constructing the device.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &'static str,
        address_range: AddressRange,
        columns: u32,
        rows: u32,
        double_buffered: bool,
        clock_hz: Option<u64>,
        frame_rate_hz: u32,
        font: Font,
        palette: Vec<Rgb24>,
    ) -> Self {
        debug_assert!(!palette.is_empty(), "palette must be non-empty (validated by the config module)");
        let cells = (columns as usize) * (rows as usize);
        let effective_clock_hz = clock_hz.unwrap_or(NOMINAL_CLOCK_HZ);
        let cycles_per_frame = (effective_clock_hz / frame_rate_hz.max(1) as u64).max(1);
        Self {
            name,
            address_range,
            columns,
            rows,
            double_buffered,
            char_ram: vec![0; cells],
            color_ram: vec![0; cells],
            scanout_char_ram: if double_buffered { vec![0; cells] } else { Vec::new() },
            scanout_color_ram: if double_buffered { vec![0; cells] } else { Vec::new() },
            swap_on_vsync: false,
            swap_pending: false,
            status: 0,
            palette_update: PaletteUpdateState::Idle,
            cycles_per_frame,
            cycle_accumulator: 0,
            frame_rate_hz,
            font,
            palette,
            frame_sink: None,
            external_transport: None,
            keyboard: None,
            keyboard_relay: None,
            keyboard_transport: None,
            log_sender: LogSender::default(),
        }
    }

    /// Grid width in cells.
    pub fn columns(&self) -> u32 {
        self.columns
    }

    /// Grid height in cells.
    pub fn rows(&self) -> u32 {
        self.rows
    }

    /// Total number of cells (`columns * rows`).
    pub fn cells(&self) -> usize {
        self.char_ram.len()
    }

    /// The glyph font fixed at configuration time (spec §7).
    pub fn font(&self) -> &Font {
        &self.font
    }

    /// The color palette fixed at configuration time (spec §3).
    pub fn palette(&self) -> &[Rgb24] {
        &self.palette
    }

    /// Attaches a push channel for composited frames (design doc §9). Once set, every vsync
    /// (design §6) composites the current scanout buffers and sends the result with
    /// [`mpsc::Sender::try_send`] -- never blocking `tick()`; if the consumer isn't keeping up,
    /// the frame is silently dropped rather than stalling CPU execution, the same never-blocks
    /// contract `LogSender` upholds for its own bounded channel.
    pub fn attach_frame_sink(&mut self, sink: mpsc::Sender<DisplayFrame>) {
        self.frame_sink = Some(sink);
    }

    /// Attaches a transport for the external display protocol (`doc/char-display-external-
    /// protocol.md`), now bidirectional: `transport` remains outbound-only for frames, but
    /// `relay` is the same transport's inbound relay, held and drained unconditionally every
    /// `tick()` (see the `keyboard_relay` field doc comment). Immediately sends the one-time
    /// header over `transport`; unlike [`Self::attach_frame_sink`], nothing else is sent on
    /// individual register writes -- only the header now, then one bulk frame per vsync (see
    /// [`Self::on_vsync`]).
    pub fn attach_external_transport(&mut self, mut transport: Box<dyn Transport>, relay: TransportRelay) {
        let header = protocol::encode_header(self.columns, self.rows, self.frame_rate_hz, self.palette.len() as u16, &self.font);
        transport.send_bytes(&header);
        self.external_transport = Some(transport);
        self.keyboard_relay = Some(relay);
    }

    /// Claims an additional, disjoint 2-byte address range (data/latch registers, mirroring
    /// `Console`'s input half) for keyboard input, paired with a `BusConfig::extend_device` call
    /// by the caller (the config module) at the same range/`DeviceId`. Must be called before the
    /// device is boxed onto the bus.
    pub fn with_keyboard_range(mut self, range: AddressRange) -> Self {
        self.keyboard = Some(KeyboardInput { range, input: InputBuffer::new() });
        self
    }

    /// Sets the break key to recognize on the keyboard sub-range's inbound stream. A no-op if no
    /// keyboard range is configured.
    pub fn set_break_key(&mut self, break_key: u8) {
        if let Some(keyboard) = self.keyboard.as_mut() {
            keyboard.input.set_break_key(break_key);
        }
    }

    /// Attaches a transport for the keyboard sub-range's inbound byte stream, along with its
    /// paired relay -- the debugger path, mirroring the deleted `Keyboard::attach_transport`. The
    /// CLI path instead rides `external_transport`/`relay` via
    /// [`Self::attach_external_transport`], since both directions share one child process's
    /// stdio.
    pub fn attach_keyboard_transport(&mut self, transport: Box<dyn Transport>, relay: TransportRelay) {
        self.keyboard_transport = Some(transport);
        self.keyboard_relay = Some(relay);
    }

    /// Installs a log sender for diagnostic messages (e.g. `reset()`).
    pub fn set_log_sender(&mut self, sender: LogSender) {
        self.log_sender = sender;
    }

    /// Returns the (character, color) buffer pair currently intended for scanout (spec §6):
    /// the scanout buffers in double-buffered mode, or the CPU-addressable buffers directly in
    /// single-buffered mode.
    pub fn frame_source(&self) -> (&[u8], &[u8]) {
        if self.double_buffered {
            (&self.scanout_char_ram, &self.scanout_color_ram)
        } else {
            (&self.char_ram, &self.color_ram)
        }
    }

    fn control_register(&self) -> u8 {
        let mut value = 0;
        if self.swap_on_vsync {
            value |= CONTROL_SWAP_ON_VSYNC;
        }
        if self.swap_pending {
            value |= CONTROL_SWAP_PENDING;
        }
        value
    }

    fn write_control(&mut self, value: u8) {
        self.swap_on_vsync = value & CONTROL_SWAP_ON_VSYNC != 0;
        if value & CONTROL_SWAP_REQUEST != 0 {
            self.request_swap();
        }
        if value & CONTROL_PALETTE_ARM != 0 {
            self.palette_update = PaletteUpdateState::ExpectIndex;
        }
    }

    /// Advances the runtime palette-update state machine by one data byte (module doc comment).
    /// A no-op while idle, preserving the register's default "writes ignored" behavior.
    fn write_palette_data(&mut self, value: u8) {
        self.palette_update = match self.palette_update {
            PaletteUpdateState::Idle => return,
            PaletteUpdateState::ExpectIndex => PaletteUpdateState::ExpectRed(value),
            PaletteUpdateState::ExpectRed(index) => PaletteUpdateState::ExpectGreen(index, value),
            PaletteUpdateState::ExpectGreen(index, red) => PaletteUpdateState::ExpectBlue(index, red, value),
            PaletteUpdateState::ExpectBlue(index, red, green) => {
                let slot = compositing::resolve_palette_index(index, self.palette.len());
                self.palette[slot] = Rgb24::new(red, green, value);
                self.status |= STATUS_PALETTE_ACCEPTED;
                PaletteUpdateState::Idle
            }
        };
    }

    /// Handles a swap request (spec §5.2): a no-op in single-buffered mode, an immediate copy
    /// when swap-on-vsync is disabled, or deferred (idempotently, if one is already pending)
    /// until the next vsync otherwise.
    fn request_swap(&mut self) {
        if !self.double_buffered {
            return;
        }
        if self.swap_on_vsync {
            self.swap_pending = true;
        } else {
            self.perform_swap();
        }
    }

    fn perform_swap(&mut self) {
        self.scanout_char_ram.copy_from_slice(&self.char_ram);
        self.scanout_color_ram.copy_from_slice(&self.color_ram);
    }

    /// The vsync-equivalent tick (spec §5.3): sets the vsync status flag, performs a pending
    /// swap if any, and -- if a frame sink is attached (design §9) -- composites the resulting
    /// scanout buffers and pushes the frame. If an external transport is attached (`doc/char-
    /// display-external-protocol.md`), also bulk-sends this vsync's char/color RAM and palette
    /// as one frame message.
    fn on_vsync(&mut self) {
        self.status |= STATUS_VSYNC;
        if self.swap_pending {
            self.perform_swap();
            self.swap_pending = false;
        }
        if let Some(sink) = &self.frame_sink {
            let (char_ram, color_ram) = self.frame_source();
            let pixels = compositing::composite(char_ram, color_ram, self.columns, self.rows, &self.palette, &self.font);
            let _ = sink.try_send(DisplayFrame { pixels, columns: self.columns, rows: self.rows });
        }
        let mut external_transport = self.external_transport.take();
        if let Some(transport) = external_transport.as_mut() {
            let (char_ram, color_ram) = self.frame_source();
            let frame = protocol::encode_frame(char_ram, color_ram, &self.palette);
            transport.send_bytes(&frame);
        }
        self.external_transport = external_transport;
    }
}

impl IoDevice for CharDisplay {
    fn read(&mut self, address: u16) -> u8 {
        if let Some(keyboard) = self.keyboard.as_mut()
            && keyboard.range.contains(address) {
            return match address - keyboard.range.start {
                0 => keyboard.input.read_data(),
                1 => keyboard.input.read_latch(),
                _ => 0,
            };
        }
        let offset = (address - self.address_range.start) as u32;
        let cells = self.cells() as u32;
        if offset < cells {
            self.char_ram[offset as usize]
        } else if offset < 2 * cells {
            self.color_ram[(offset - cells) as usize]
        } else if offset == 2 * cells {
            self.control_register()
        } else if offset == 2 * cells + 1 {
            let value = self.status;
            self.status &= !(STATUS_VSYNC | STATUS_PALETTE_ACCEPTED);
            value
        } else {
            0
        }
    }

    fn write(&mut self, address: u16, value: u8) {
        if let Some(keyboard) = self.keyboard.as_mut()
            && keyboard.range.contains(address) {
            // Data register (offset 0) is input-only, matching the deleted `Keyboard`'s own
            // no-op write semantics: this device has no outbound byte stream to send to.
            if address - keyboard.range.start == 1 {
                keyboard.input.write_latch(value);
            }
            return;
        }
        let offset = (address - self.address_range.start) as u32;
        let cells = self.cells() as u32;
        if offset < cells {
            self.char_ram[offset as usize] = value;
        } else if offset < 2 * cells {
            self.color_ram[(offset - cells) as usize] = value;
        } else if offset == 2 * cells {
            self.write_control(value);
        } else if offset == 2 * cells + 1 {
            self.write_palette_data(value);
        }
    }

    fn peek(&self, address: u16) -> u8 {
        if let Some(keyboard) = self.keyboard.as_ref()
            && keyboard.range.contains(address) {
            return match address - keyboard.range.start {
                0 => keyboard.input.peek_data(),
                1 => keyboard.input.peek_latch(),
                _ => 0,
            };
        }
        let offset = (address - self.address_range.start) as u32;
        let cells = self.cells() as u32;
        if offset < cells {
            self.char_ram[offset as usize]
        } else if offset < 2 * cells {
            self.color_ram[(offset - cells) as usize]
        } else if offset == 2 * cells {
            self.control_register()
        } else if offset == 2 * cells + 1 {
            self.status
        } else {
            0
        }
    }

    fn claims(&self, address: u16) -> bool {
        self.address_range.contains(address)
            || self.keyboard.as_ref().is_some_and(|keyboard| keyboard.range.contains(address))
    }

    fn tick(&mut self, cycles: u32) {
        // Drained unconditionally, regardless of whether `keyboard` is configured -- see the
        // `keyboard_relay` field doc comment for why this is a correctness requirement, not an
        // optimization to skip when there's no keyboard sub-range.
        if let Some(relay) = self.keyboard_relay.as_mut() {
            let mut keyboard = self.keyboard.as_mut();
            relay.drain_bytes_into(|b| {
                if let Some(keyboard) = keyboard.as_mut() {
                    keyboard.input.push(b);
                }
            });
        }
        self.cycle_accumulator += cycles as u64;
        while self.cycle_accumulator >= self.cycles_per_frame {
            self.cycle_accumulator -= self.cycles_per_frame;
            self.on_vsync();
        }
    }

    fn reset(&mut self) {
        self.swap_on_vsync = false;
        self.swap_pending = false;
        self.status = 0;
        self.palette_update = PaletteUpdateState::Idle;
        self.cycle_accumulator = 0;
        if let Some(keyboard) = self.keyboard.as_mut() {
            keyboard.input.reset();
        }
        log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} reset", self.identity());
    }

    fn irq_active(&self) -> bool {
        self.keyboard.as_ref().is_some_and(|keyboard| keyboard.input.irq_active())
    }

    fn name(&self) -> &str {
        self.name
    }

    fn identity_address(&self) -> u16 {
        self.address_range.start
    }

    /// Drops the frame sink, closing the channel from this end -- the channel equivalent of
    /// the terminal bridge seeing EOF on its pipe (design doc §10), ending the debugger's
    /// display bridge task's `recv()` loop. Also shuts down the external transport and the
    /// debugger-path keyboard transport, if either is present (`doc/char-display-external-
    /// protocol.md`; the CLI path's keyboard stream rides `external_transport`, already covered).
    fn shutdown(&mut self) {
        self.frame_sink = None;
        if let Some(transport) = self.external_transport.as_mut() {
            transport.shutdown();
        }
        if let Some(transport) = self.keyboard_transport.as_mut() {
            transport.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE_NAME: &str = "display";
    const COLUMNS: u32 = 4;
    const ROWS: u32 = 2;
    const CELLS: u32 = COLUMNS * ROWS;
    const BASE_ADDRESS: u16 = 0xD000;

    fn address_range() -> AddressRange {
        AddressRange::new(BASE_ADDRESS, BASE_ADDRESS + (2 * CELLS + 2 - 1) as u16)
    }

    fn device(double_buffered: bool) -> CharDisplay {
        CharDisplay::new(
            DEVICE_NAME,
            address_range(),
            COLUMNS,
            ROWS,
            double_buffered,
            Some(1_000_000),
            100,
            Font::default(),
            compositing::default_palette(),
        )
    }

    fn char_ram_addr(offset: u16) -> u16 {
        BASE_ADDRESS + offset
    }

    fn color_ram_addr(offset: u16) -> u16 {
        BASE_ADDRESS + CELLS as u16 + offset
    }

    fn control_addr() -> u16 {
        BASE_ADDRESS + (2 * CELLS) as u16
    }

    fn status_addr() -> u16 {
        BASE_ADDRESS + (2 * CELLS) as u16 + 1
    }

    #[test]
    fn char_ram_read_write_round_trip() {
        let mut device = device(true);
        device.write(char_ram_addr(0), 0x41);
        device.write(char_ram_addr(CELLS as u16 - 1), 0x42);
        assert_eq!(device.read(char_ram_addr(0)), 0x41);
        assert_eq!(device.read(char_ram_addr(CELLS as u16 - 1)), 0x42);
        assert_eq!(device.peek(char_ram_addr(0)), 0x41);
    }

    #[test]
    fn color_ram_read_write_round_trip() {
        let mut device = device(true);
        device.write(color_ram_addr(0), 0x05);
        device.write(color_ram_addr(CELLS as u16 - 1), 0x0F);
        assert_eq!(device.read(color_ram_addr(0)), 0x05);
        assert_eq!(device.read(color_ram_addr(CELLS as u16 - 1)), 0x0F);
        assert_eq!(device.peek(color_ram_addr(0)), 0x05);
    }

    #[test]
    fn claims_only_within_configured_range() {
        let device = device(true);
        assert!(device.claims(BASE_ADDRESS));
        assert!(device.claims(status_addr()));
        assert!(!device.claims(BASE_ADDRESS - 1));
        assert!(!device.claims(status_addr() + 1));
    }

    #[test]
    fn identity_address_and_name() {
        let device = device(true);
        assert_eq!(device.identity_address(), BASE_ADDRESS);
        assert_eq!(device.name(), DEVICE_NAME);
    }

    #[test]
    fn swap_immediate_when_swap_on_vsync_disabled() {
        let mut device = device(true);
        device.write(char_ram_addr(0), 0x41);
        // Before any swap, frame_source (scanout) must not reflect the CPU write yet.
        assert_eq!(device.frame_source().0[0], 0);
        device.write(control_addr(), CONTROL_SWAP_REQUEST);
        assert_eq!(device.frame_source().0[0], 0x41);
        // Swap pending bit must not be set for an immediate swap.
        assert_eq!(device.read(control_addr()) & CONTROL_SWAP_PENDING, 0);
    }

    #[test]
    fn swap_deferred_until_vsync_when_swap_on_vsync_enabled() {
        let mut device = device(true);
        device.write(control_addr(), CONTROL_SWAP_ON_VSYNC);
        device.write(char_ram_addr(0), 0x41);
        device.write(control_addr(), CONTROL_SWAP_ON_VSYNC | CONTROL_SWAP_REQUEST);
        // Swap must be deferred: scanout unaffected, pending bit set.
        assert_eq!(device.frame_source().0[0], 0);
        assert_ne!(device.read(control_addr()) & CONTROL_SWAP_PENDING, 0);
        // Drive vsync: cycles_per_frame = 1_000_000 / 100 = 10_000.
        device.tick(10_000);
        assert_eq!(device.frame_source().0[0], 0x41);
        assert_eq!(device.read(control_addr()) & CONTROL_SWAP_PENDING, 0);
        assert_ne!(device.read(status_addr()) & STATUS_VSYNC, 0);
    }

    #[test]
    fn second_swap_request_while_pending_is_idempotent() {
        let mut device = device(true);
        device.write(control_addr(), CONTROL_SWAP_ON_VSYNC | CONTROL_SWAP_REQUEST);
        device.write(control_addr(), CONTROL_SWAP_ON_VSYNC | CONTROL_SWAP_REQUEST);
        assert_ne!(device.read(control_addr()) & CONTROL_SWAP_PENDING, 0);
        device.tick(10_000);
        // A single deferred swap should have been performed, not queued twice; the pending
        // bit is clear and a further tick with no new request changes nothing.
        assert_eq!(device.read(control_addr()) & CONTROL_SWAP_PENDING, 0);
    }

    #[test]
    fn single_buffered_mode_swap_is_a_no_op() {
        let mut device = device(false);
        device.write(char_ram_addr(0), 0x41);
        // frame_source reads directly from the CPU-addressable buffer; no swap needed.
        assert_eq!(device.frame_source().0[0], 0x41);
        device.write(control_addr(), CONTROL_SWAP_REQUEST);
        assert_eq!(device.frame_source().0[0], 0x41);
        assert_eq!(device.read(control_addr()) & CONTROL_SWAP_PENDING, 0);
    }

    #[test]
    fn status_register_is_read_to_clear() {
        let mut device = device(true);
        device.tick(10_000);
        assert_ne!(device.peek(status_addr()) & STATUS_VSYNC, 0);
        assert_ne!(device.read(status_addr()) & STATUS_VSYNC, 0);
        assert_eq!(device.read(status_addr()), 0);
        assert_eq!(device.peek(status_addr()), 0);
    }

    #[test]
    fn status_register_writes_are_ignored() {
        let mut device = device(true);
        device.write(status_addr(), 0xFF);
        assert_eq!(device.peek(status_addr()), 0);
    }

    #[test]
    fn full_palette_sequence_applies_color_and_sets_accepted_status() {
        let mut device = device(true);
        device.write(control_addr(), CONTROL_PALETTE_ARM);
        device.write(status_addr(), 2); // index
        device.write(status_addr(), 10); // red
        device.write(status_addr(), 20); // green
        device.write(status_addr(), 30); // blue

        assert_eq!(device.palette()[2], Rgb24::new(10, 20, 30));
        assert_ne!(device.peek(status_addr()) & STATUS_PALETTE_ACCEPTED, 0);
        assert_eq!(device.read(status_addr()) & STATUS_PALETTE_ACCEPTED, STATUS_PALETTE_ACCEPTED);
        assert_eq!(device.peek(status_addr()) & STATUS_PALETTE_ACCEPTED, 0, "read-to-clear");
    }

    #[test]
    fn palette_update_out_of_range_index_wraps_via_modulo() {
        let mut device = device(true);
        assert_eq!(device.palette().len(), 16);
        device.write(control_addr(), CONTROL_PALETTE_ARM);
        device.write(status_addr(), 200); // 200 % 16 == 8
        device.write(status_addr(), 1);
        device.write(status_addr(), 2);
        device.write(status_addr(), 3);

        assert_eq!(device.palette()[8], Rgb24::new(1, 2, 3));
    }

    #[test]
    fn palette_data_writes_are_ignored_when_not_armed() {
        let mut device = device(true);
        let original = device.palette().to_vec();
        device.write(status_addr(), 0);
        device.write(status_addr(), 10);
        device.write(status_addr(), 20);
        device.write(status_addr(), 30);

        assert_eq!(device.palette(), original.as_slice());
        assert_eq!(device.peek(status_addr()) & STATUS_PALETTE_ACCEPTED, 0);
    }

    #[test]
    fn rearming_mid_sequence_discards_partial_sequence_and_starts_fresh() {
        let mut device = device(true);
        device.write(control_addr(), CONTROL_PALETTE_ARM);
        device.write(status_addr(), 0); // index (would-be first sequence)
        device.write(status_addr(), 0xFF); // red (would-be first sequence, discarded)

        device.write(control_addr(), CONTROL_PALETTE_ARM); // re-arm resets to ExpectIndex
        device.write(status_addr(), 3); // index
        device.write(status_addr(), 11);
        device.write(status_addr(), 22);
        device.write(status_addr(), 33);

        assert_eq!(device.palette()[3], Rgb24::new(11, 22, 33));
        assert_eq!(device.palette()[0], Rgb24::new(0, 0, 0), "first slot untouched by discarded sequence");
    }

    #[test]
    fn unrelated_control_write_without_arm_bit_does_not_disturb_in_progress_sequence() {
        let mut device = device(true);
        device.write(control_addr(), CONTROL_PALETTE_ARM);
        device.write(status_addr(), 5); // index
        device.write(status_addr(), 11); // red

        // A plain swap request, with bit 3 clear, does not disarm the in-progress sequence --
        // there is no way to disarm it short of re-arming or completing it.
        device.write(control_addr(), CONTROL_SWAP_REQUEST);
        device.write(status_addr(), 22); // green
        device.write(status_addr(), 33); // blue

        assert_eq!(device.palette()[5], Rgb24::new(11, 22, 33));
    }

    #[test]
    fn reset_clears_in_progress_palette_update_state() {
        let mut device = device(true);
        let original = device.palette().to_vec();
        device.write(control_addr(), CONTROL_PALETTE_ARM);
        device.write(status_addr(), 0); // index
        device.write(status_addr(), 0xFF); // red

        device.reset();
        // Resuming with the remaining bytes of the pre-reset sequence must not spuriously
        // apply a color built from a mix of pre- and post-reset bytes.
        device.write(status_addr(), 1);
        device.write(status_addr(), 2);

        assert_eq!(device.palette(), original.as_slice());
    }

    #[test]
    fn vsync_cadence_derived_from_clock_and_frame_rate() {
        let mut device = device(true);
        // cycles_per_frame = 1_000_000 / 100 = 10_000; one cycle short must not fire vsync yet.
        device.tick(9_999);
        assert_eq!(device.peek(status_addr()) & STATUS_VSYNC, 0);
        device.tick(1);
        assert_ne!(device.peek(status_addr()) & STATUS_VSYNC, 0);
    }

    #[test]
    fn nominal_clock_used_when_clock_hz_unavailable() {
        let mut device = CharDisplay::new(
            DEVICE_NAME,
            address_range(),
            COLUMNS,
            ROWS,
            true,
            None,
            DEFAULT_FRAME_RATE_HZ,
            Font::default(),
            compositing::default_palette(),
        );
        let cycles_per_frame = NOMINAL_CLOCK_HZ / DEFAULT_FRAME_RATE_HZ as u64;
        device.tick(cycles_per_frame as u32 - 1);
        assert_eq!(device.peek(status_addr()) & STATUS_VSYNC, 0);
        device.tick(1);
        assert_ne!(device.peek(status_addr()) & STATUS_VSYNC, 0);
    }

    #[test]
    fn reset_clears_control_and_status_but_preserves_ram() {
        let mut device = device(true);
        device.write(char_ram_addr(0), 0x41);
        device.write(control_addr(), CONTROL_SWAP_ON_VSYNC | CONTROL_SWAP_REQUEST);
        device.tick(10_000);
        device.tick(10_000);
        device.reset();
        assert_eq!(device.peek(control_addr()), 0);
        assert_eq!(device.peek(status_addr()), 0);
        assert_eq!(device.peek(char_ram_addr(0)), 0x41);
    }

    #[test]
    fn reset_logs_device_message() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let mut device = device(true);
        device.set_log_sender(sender);
        device.reset();
        let received = rx.recv().unwrap();
        assert_eq!(received.category, LogCategory::Device);
        assert_eq!(received.message, format!("{DEVICE_NAME}@0x{BASE_ADDRESS:04x} reset"));
    }

    #[test]
    fn font_and_palette_accessors_reflect_constructor_arguments() {
        let font = Font::default();
        let palette = vec![Rgb24::new(1, 2, 3), Rgb24::new(4, 5, 6)];
        let device = CharDisplay::new(
            DEVICE_NAME,
            address_range(),
            COLUMNS,
            ROWS,
            true,
            Some(1_000_000),
            100,
            font.clone(),
            palette.clone(),
        );
        assert_eq!(device.font(), &font);
        assert_eq!(device.palette(), palette.as_slice());
    }

    #[test]
    fn vsync_pushes_a_composited_frame_to_an_attached_sink() {
        let mut device = device(true);
        let (tx, mut rx) = mpsc::channel(1);
        device.attach_frame_sink(tx);
        device.write(char_ram_addr(0), 0x41);
        device.write(color_ram_addr(0), 1);
        device.tick(10_000); // one vsync at this device's cycles_per_frame (1_000_000 / 100)

        let frame = rx.try_recv().expect("expected a composited frame after vsync");
        assert_eq!(frame.columns, COLUMNS);
        assert_eq!(frame.rows, ROWS);
        assert_eq!(frame.pixels.len(), (COLUMNS * 8 * ROWS * 8 * 4) as usize);
    }

    #[test]
    fn vsync_with_no_attached_sink_still_sets_the_status_flag() {
        let mut device = device(true);
        device.tick(10_000);
        assert_ne!(device.peek(status_addr()) & STATUS_VSYNC, 0);
    }

    #[test]
    fn shutdown_drops_the_frame_sink_closing_the_channel() {
        let mut device = device(true);
        let (tx, mut rx) = mpsc::channel(1);
        device.attach_frame_sink(tx);
        device.shutdown();
        device.tick(10_000);
        assert!(rx.try_recv().is_err(), "channel should be closed once the sink is dropped");
    }

    /// `remote` is the peripheral's end of the pipe: everything the device sends over the
    /// external transport lands here, byte by byte, and can be collected with
    /// [`collect_bytes`]. Mirrors `LedMatrix`'s own `device_with_pipe` test helper.
    fn device_with_external_transport(double_buffered: bool) -> (CharDisplay, crate::emulator::transport::InternalPipeTransport) {
        let reporter = crate::emulator::transport::TransportReporter::pending(None);
        let ((local, relay), remote) = crate::emulator::transport::InternalPipeTransport::pair(reporter).unwrap();
        let mut device = device(double_buffered);
        device.attach_external_transport(Box::new(local), TransportRelay::Byte(relay));
        (device, remote)
    }

    fn collect_bytes(remote: &mut crate::emulator::transport::InternalPipeTransport) -> Vec<u8> {
        let mut buf = Vec::new();
        while let Some(b) = remote.try_recv() {
            buf.push(b);
        }
        buf
    }

    #[test]
    fn attach_external_transport_sends_header_immediately() {
        let (_device, mut remote) = device_with_external_transport(true);
        let bytes = collect_bytes(&mut remote);

        assert_eq!(&bytes[0..4], b"E65D");
        assert_eq!(bytes[4], 1); // version
        assert_eq!(&bytes[5..9], &COLUMNS.to_le_bytes());
        assert_eq!(&bytes[9..13], &ROWS.to_le_bytes());
        assert_eq!(&bytes[13..17], &100u32.to_le_bytes()); // frame_rate_hz from device()
        assert_eq!(&bytes[17..19], &16u16.to_le_bytes()); // default_palette() length
        assert_eq!(bytes.len(), 19 + font::FONT_BYTES);
    }

    #[test]
    fn vsync_sends_one_frame_over_external_transport() {
        let (mut device, mut remote) = device_with_external_transport(true);
        collect_bytes(&mut remote); // drain the header

        device.write(char_ram_addr(0), 0x41);
        device.write(color_ram_addr(0), 1);
        device.write(control_addr(), CONTROL_SWAP_REQUEST);
        device.tick(10_000); // one vsync at this device's cycles_per_frame

        let frame = collect_bytes(&mut remote);
        let expected_len = 2 * CELLS as usize + compositing::default_palette().len() * 3;
        assert_eq!(frame.len(), expected_len);
        assert_eq!(frame[0], 0x41); // char RAM cell 0
        assert_eq!(frame[CELLS as usize], 1); // color RAM cell 0
    }

    #[test]
    fn shutdown_stops_further_sends_over_external_transport() {
        let (mut device, mut remote) = device_with_external_transport(true);
        collect_bytes(&mut remote); // drain the header

        device.shutdown();
        device.tick(10_000); // one vsync -- must not send after shutdown

        assert!(collect_bytes(&mut remote).is_empty());
    }

    /// **The correctness fix** (design doc Context section): the keyboard relay must be drained
    /// unconditionally every `tick()`, even with no keyboard range configured -- otherwise the
    /// first byte a peripheral ever writes to its own stdout kills the *entire* transport, frames
    /// included, because an undrained relay's channel closing is treated as a broken pipe.
    #[test]
    fn tick_drains_keyboard_relay_even_without_keyboard_range_configured() {
        let (mut device, mut remote) = device_with_external_transport(true);
        collect_bytes(&mut remote); // drain the header

        remote.send(0x42); // simulates emma65-display's first-ever stdout write
        std::thread::sleep(std::time::Duration::from_millis(5));
        device.tick(10_000); // must not tear down the transport merely because this arrived

        let frame = collect_bytes(&mut remote);
        assert!(!frame.is_empty(), "expected the transport to remain alive and still send a frame");
    }

    // -- Keyboard sub-range: behavior re-homed from the deleted `Keyboard` device's own test
    // module, now exercised through `CharDisplay`'s keyboard sub-range instead. --

    const KEYBOARD_BASE: u16 = 0xE000;

    fn keyboard_range() -> AddressRange {
        AddressRange::new(KEYBOARD_BASE, KEYBOARD_BASE + 1)
    }

    fn keyboard_data_addr() -> u16 {
        KEYBOARD_BASE
    }

    fn keyboard_latch_addr() -> u16 {
        KEYBOARD_BASE + 1
    }

    fn device_with_keyboard(double_buffered: bool) -> CharDisplay {
        device(double_buffered).with_keyboard_range(keyboard_range())
    }

    /// See `Console`'s test module for the rationale behind this hand-fed relay harness.
    fn spawn_byte_relay(capacity: usize) -> (crossbeam_channel::Sender<u8>, crate::emulator::transport::ChannelRelay<u8>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (tx, crate::emulator::transport::ChannelRelay::spawn(rx, capacity))
    }

    fn device_with_keyboard_pipe(relay_capacity: usize) -> (CharDisplay, crate::emulator::transport::InternalPipeTransport, crossbeam_channel::Sender<u8>) {
        let (local, remote) = crate::emulator::transport::InternalPipeTransport::pair_direct().unwrap();
        let (tx, relay) = spawn_byte_relay(relay_capacity);
        let mut device = device_with_keyboard(true);
        device.attach_keyboard_transport(Box::new(local), TransportRelay::Byte(relay));
        (device, remote, tx)
    }

    #[test]
    fn keyboard_read_data_register_delegates_to_input_buffer() {
        let mut device = device_with_keyboard(true);
        device.write(keyboard_latch_addr(), 0x42);
        assert_eq!(device.read(keyboard_data_addr()), 0x42);
    }

    #[test]
    fn keyboard_read_latch_register_delegates_to_input_buffer() {
        let mut device = device_with_keyboard(true);
        device.keyboard.as_mut().unwrap().input.push(0x42);
        assert_eq!(device.read(keyboard_latch_addr()), 0x42);
    }

    #[test]
    fn keyboard_write_data_register_is_noop() {
        let (mut device, mut remote, _tx) = device_with_keyboard_pipe(256);
        device.write(keyboard_data_addr(), 0x42);
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(remote.try_recv(), None, "expected no outbound byte");
        assert_eq!(device.peek(keyboard_data_addr()), 0, "expected no state change");
    }

    #[test]
    fn keyboard_write_latch_register_delegates_to_input_buffer() {
        let mut device = device_with_keyboard(true);
        device.write(keyboard_latch_addr(), 0x42);
        assert_eq!(device.peek(keyboard_latch_addr()), 0x42);
    }

    #[test]
    fn keyboard_peek_delegates_to_input_buffer_without_side_effects() {
        let mut device = device_with_keyboard(true);
        device.keyboard.as_mut().unwrap().input.push(0x42);
        assert_eq!(device.peek(keyboard_data_addr()), 0x42);
        assert_eq!(device.peek(keyboard_data_addr()), 0x42, "peek must not consume the buffered byte");
    }

    #[test]
    fn keyboard_tick_buffers_input_from_transport() {
        let (mut device, _remote, tx) = device_with_keyboard_pipe(256);
        tx.send(0x42).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        device.tick(1);
        assert_eq!(device.peek(keyboard_data_addr()), 0x42);
    }

    #[test]
    fn keyboard_tick_latches_break_key_and_sets_interrupt_flag() {
        let (mut device, _remote, tx) = device_with_keyboard_pipe(256);
        device.set_break_key(0x3);
        tx.send(0x3).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        device.tick(1);
        assert_eq!(device.peek(keyboard_latch_addr()), 0x3);
        assert!(device.irq_active(), "expected interrupt flag set");
    }

    #[test]
    fn set_break_key_is_a_noop_without_a_keyboard_range() {
        // Must not panic when no keyboard range is configured.
        let mut device = device(true);
        device.set_break_key(0x3);
        assert!(!device.irq_active());
    }

    #[test]
    fn keyboard_reset_clears_buffered_input() {
        let mut device = device_with_keyboard(true);
        device.keyboard.as_mut().unwrap().input.push(0x42);
        device.reset();
        assert_eq!(device.peek(keyboard_data_addr()), 0, "reset must clear buffered keyboard input");
    }

    #[test]
    fn claims_covers_both_framebuffer_and_keyboard_ranges() {
        let device = device_with_keyboard(true);
        assert!(device.claims(BASE_ADDRESS));
        assert!(device.claims(keyboard_data_addr()));
        assert!(device.claims(keyboard_latch_addr()));
        assert!(!device.claims(keyboard_latch_addr() + 1));
    }

    #[test]
    fn keyboard_range_below_framebuffer_base_does_not_underflow() {
        // The keyboard range's early check must run before the framebuffer offset arithmetic --
        // otherwise a keyboard address below the framebuffer's base underflows that subtraction.
        let low_range = AddressRange::new(0x0010, 0x0011);
        let mut device = device(true).with_keyboard_range(low_range);
        device.write(0x0011, 0x42);
        assert_eq!(device.read(0x0011), 0x42);
    }

    #[test]
    fn no_irq_capability_without_a_keyboard_range() {
        let device = device(true);
        assert!(!device.irq_active());
    }
}
