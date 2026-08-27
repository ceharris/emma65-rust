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
//! This device is not IRQ-capable; nothing in its register map asserts an interrupt.
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

pub mod compositing;
pub mod font;

use self::compositing::Rgb24;
use self::font::Font;
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
            font,
            palette,
            frame_sink: None,
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
    /// scanout buffers and pushes the frame.
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
    }
}

impl IoDevice for CharDisplay {
    fn read(&mut self, address: u16) -> u8 {
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
    }

    fn tick(&mut self, cycles: u32) {
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
        log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} reset", self.identity());
    }

    fn name(&self) -> &str {
        self.name
    }

    fn identity_address(&self) -> u16 {
        self.address_range.start
    }

    /// Drops the frame sink, closing the channel from this end -- the channel equivalent of
    /// the terminal bridge seeing EOF on its pipe (design doc §10), ending the debugger's
    /// display bridge task's `recv()` loop.
    fn shutdown(&mut self) {
        self.frame_sink = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE_NAME: &str = "display/char";
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
}
