//! A memory-mapped HD44780-compatible character LCD display device.
//!
//! See `plan/memory-mapped-lcd-display-device-spec.md` for the full behavioral specification and
//! `plan/memory-mapped-lcd-display-device-plan.md` for the design decisions this implementation
//! follows. Unlike [`super::display::CharDisplay`] and [`super::led_matrix::LedMatrix`], this
//! device does not map its display memory directly into the bus address space -- it reproduces a
//! real HD44780's two-register interface (spec §2), with all of DDRAM, CGRAM, and the address
//! counter reached only indirectly through those two registers:
//!
//! | Register    | Offset | Access | Notes                                              |
//! |-------------|--------|--------|-----------------------------------------------------|
//! | Instruction | `0`    | R/W    | W: issue instruction (spec §4.2). R: busy + address counter (spec §4.3) |
//! | Data        | `1`    | R/W    | R/W: DDRAM/CGRAM at the current address (spec §4.4) |
//!
//! Register access, instruction decode, busy timing, and DDRAM/CGRAM storage are all observable
//! through direct `read`/`write`/`peek` calls (Work Unit 1). [`cgrom`] and [`compositing`] add
//! the CGROM table and the pure pixel-compositing function (Work Unit 2); the config module
//! (`emulator::config::lcd_display`) resolves a real `Geometry`/`CgRom`/colors (Work Unit 3); and
//! [`LcdDisplay::attach_frame_sink`] wires a debugger-owned push channel that every render-
//! affecting register write composites into and sends an [`LcdDisplayFrame`] through (Work Unit
//! 4, design doc §7) -- frontend rendering itself is Work Unit 5.

pub mod cgrom;
pub mod compositing;
mod protocol;

use self::cgrom::CgRom;
use self::compositing::{CursorState, Rgb24};
use crate::emulator::transport::Transport;
use crate::emulator::{AddressRange, IoDevice, LogCategory, LogLevel, LogSender, log_msg};
use tokio::sync::mpsc;

/// The `NOMINAL_CLOCK_HZ` fallback used when the CPU runs unthrottled (`ClockSpeed::unlimited()`)
/// -- reused directly from `display` rather than duplicated (design doc §4).
use super::display::NOMINAL_CLOCK_HZ;

/// A composited frame ready for display: an RGBA byte buffer (`columns * 5` by `rows * (8 or 10)`
/// pixels, depending on the active font -- see [`compositing::composite`]) plus the cell
/// dimensions it was composited from -- self-describing since a delivery client has no other way
/// to learn the device's configured grid size until a frame actually arrives.
///
/// Pushed to a device's frame sink on every register write that can change what's rendered
/// (design doc §7) -- unlike [`super::display::DisplayFrame`]'s vsync cadence or
/// [`super::led_matrix::LedMatrixFrame`]'s per-swap push, this device has no periodic redraw
/// concept at all (spec §2.1's timing is about busy/instruction latency, not a periodic vsync).
#[derive(Clone)]
pub struct LcdDisplayFrame {
    /// RGBA bytes, row-major, top row first, 4 bytes per pixel.
    pub pixels: Vec<u8>,
    /// Grid width in cells this frame was composited from.
    pub columns: u8,
    /// Grid height in cells this frame was composited from.
    pub rows: u8,
}

/// A supported physical character grid: visible rows, each composed of one or more DDRAM
/// segments (`(start, count)`, raw HD44780 address and visible width -- spec §7.1). `columns` is
/// the widest row's visible width, used by the panel to size itself once compositing exists.
///
/// Instances are `'static` (looked up from a fixed table once config wiring exists -- design doc
/// §2); this work unit's tests hand-construct `Geometry` values directly, the same way
/// `CharDisplay`'s tests hand-construct dimensions before its own config module existed.
pub struct Geometry {
    pub rows: u8,
    pub columns: u8,
    pub segments: &'static [&'static [(u8, u8)]],
}

impl Geometry {
    /// True for every supported geometry except the two single-80-byte-line ones (`8x1`,
    /// `40x1`), whose only segments stay within the first 40-byte DDRAM half. Every other
    /// geometry's segments reach into the second half (a raw address of `0x40` or more -- spec
    /// §7.1's addressing-style column), which is what this device's real-hardware-accurate DDRAM
    /// addressing (`fold_ddram_address` below) and `line_shift`'s sizing/modulus (spec §7.4) both
    /// key off. `pub(crate)` since [`compositing::composite`] needs the same line-bucketing logic
    /// to apply `line_shift` correctly and is handed only a `&Geometry`, not this device's own
    /// precomputed `dual_line` field.
    pub(crate) fn is_dual_line(&self) -> bool {
        self.segments.iter().any(|row| row.iter().any(|&(start, _)| start >= 0x40))
    }
}

/// Short-instruction busy duration (spec §5): every instruction except `Clear Display`/`Return
/// Home`, plus data register reads and writes.
const SHORT_INSTRUCTION_US: u64 = 37;
/// Long-instruction busy duration (spec §5): `Clear Display` and `Return Home` only.
const LONG_INSTRUCTION_US: u64 = 1_520;

/// Per-register nibble-pairing state (spec §6). Tracked independently for the instruction and
/// data registers so interleaved accesses to one don't disturb an in-progress pairing on the
/// other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NibbleState {
    /// 8-bit mode, or 4-bit mode awaiting the first (high) nibble.
    Idle,
    /// 4-bit mode: high nibble already received (already shifted into position), awaiting the
    /// low nibble.
    HighReceived(u8),
}

impl NibbleState {
    /// Feeds one raw register write through the current interface width (spec §6), returning the
    /// assembled byte once a full value is available: immediately in 8-bit mode, or after the
    /// second nibble in 4-bit mode. Reads never go through this -- they always return a full byte
    /// in one access, in both interface widths (spec §6).
    fn feed(&mut self, raw: u8, eight_bit: bool) -> Option<u8> {
        if eight_bit {
            *self = NibbleState::Idle;
            return Some(raw);
        }
        match *self {
            NibbleState::Idle => {
                *self = NibbleState::HighReceived(raw & 0xF0);
                None
            }
            NibbleState::HighReceived(hi) => {
                *self = NibbleState::Idle;
                Some(hi | (raw >> 4))
            }
        }
    }
}

/// The address counter, bundled with which RAM it currently targets so an increment/decrement
/// can never be applied against the wrong RAM's modulus (spec §4.4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressCounterTarget {
    /// Physical index into the flat 80-byte DDRAM store (design doc §5) -- already folded from
    /// whatever raw address a `Set DDRAM Address` instruction supplied; see
    /// `LcdDisplay::fold_ddram_address`.
    Ddram(u8),
    /// Index into the 64-byte CGRAM store (0..64).
    Cgram(u8),
}

impl AddressCounterTarget {
    /// Advances the address counter by one position in the direction `forward` specifies,
    /// wrapping within whichever RAM it targets (spec §4.4.1): 80 for DDRAM, 64 for CGRAM,
    /// regardless of configured geometry.
    fn advance(&mut self, forward: bool) {
        match self {
            AddressCounterTarget::Ddram(addr) => {
                *addr = if forward { (*addr + 1) % 80 } else { (*addr + 80 - 1) % 80 };
            }
            AddressCounterTarget::Cgram(addr) => {
                *addr = if forward { (*addr + 1) % 64 } else { (*addr + 64 - 1) % 64 };
            }
        }
    }

    /// The raw numeric value, as reported by the instruction register's read side (spec §4.3):
    /// the DDRAM or CGRAM address last set, without regard to which RAM it targets.
    fn value(&self) -> u8 {
        match *self {
            AddressCounterTarget::Ddram(addr) => addr,
            AddressCounterTarget::Cgram(addr) => addr,
        }
    }
}

/// A memory-mapped HD44780-compatible character LCD display device.
pub struct LcdDisplay {
    name: &'static str,
    address_range: AddressRange,
    geometry: &'static Geometry,
    dual_line: bool,
    cgrom: CgRom,
    background: Rgb24,
    foreground: Rgb24,

    instruction_nibble: NibbleState,
    data_nibble: NibbleState,
    interface_width_8bit: bool, // DL, from Function Set; 8-bit at reset (spec §2.2)
    font_5x10: bool,            // F, from Function Set

    entry_id: bool,    // ID: true = increment
    entry_shift: bool, // S: accompany DDRAM writes with a display shift

    display_on: bool,
    cursor_on: bool,
    cursor_blink: bool,

    ac: AddressCounterTarget,
    ddram: [u8; 80],
    cgram: [u8; 64],

    /// One shift offset per 40-byte DDRAM line (spec §7.4); single-line geometries
    /// (`Geometry::is_dual_line() == false`) use only `line_shift[0]`, modulo 80 instead of 40.
    line_shift: [u8; 2],

    /// Effective clock, resolved once at construction from the configured `clock_hz` or
    /// [`NOMINAL_CLOCK_HZ`] as a fallback (spec §5). Instruction/data timing is computed from
    /// this at execution time rather than cached per-instruction-type, since it's cheap
    /// arithmetic run at most once per instruction.
    clock_hz: u64,
    busy_cycles_remaining: u64,

    /// Push channel for composited frames (design doc §7), set post-construction via
    /// [`Self::attach_frame_sink`] -- `None` when run outside the debugger (plain `emma65` CLI),
    /// in which case no register write ever composites anything.
    frame_sink: Option<mpsc::Sender<LcdDisplayFrame>>,

    /// Outbound-only transport to an external peripheral process (`plan/lcd-display-external-
    /// protocol.md`), set post-construction via [`Self::attach_external_transport`] -- `None` when
    /// run without one configured (the debugger, or a plain `emma65` CLI with no `transport=`
    /// attribute), in which case [`Self::push_frame`] never sends anything over it. Unlike
    /// [`super::display::CharDisplay`]'s `external_transport`, this device has no inbound
    /// direction to pair with a relay -- like [`super::led_matrix::LedMatrix`], it has no input
    /// capability at all.
    external_transport: Option<Box<dyn Transport>>,

    /// A frame message [`Self::push_frame`] couldn't hand off to `external_transport` immediately
    /// (its ring was still full of an earlier, not-yet-drained message) -- retried on every
    /// [`Self::tick`] until it succeeds, always holding the *latest* composited state rather than
    /// a backlog (a later `push_frame` overwrites it outright, discarding the stale one). Without
    /// this, a companion peripheral that briefly falls behind the write rate (issue #581) can miss
    /// a frame permanently: unlike `CharDisplay`/`LedMatrix`, this device has no periodic vsync
    /// tick to naturally re-send the current state on, so a dropped frame with no writes following
    /// it would otherwise leave the peripheral showing stale content forever.
    pending_frame: Option<Vec<u8>>,

    log_sender: LogSender,
}

impl LcdDisplay {
    /// Creates a new device. `address_range` must span exactly 2 bytes (spec §4.1) -- callers
    /// (the config module) are responsible for this.
    ///
    /// `clock_hz` is the CPU's configured clock speed in Hz, or `None` if the CPU runs
    /// unthrottled (`ClockSpeed::unlimited()`); see [`NOMINAL_CLOCK_HZ`].
    ///
    /// `cgrom`, `background`, and `foreground` are fixed at configuration time (spec §3) and
    /// consumed by [`compositing::composite`] once a frame sink is attached (design doc §7) via
    /// [`Self::attach_frame_sink`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &'static str,
        address_range: AddressRange,
        geometry: &'static Geometry,
        clock_hz: Option<u64>,
        cgrom: CgRom,
        background: Rgb24,
        foreground: Rgb24,
    ) -> Self {
        debug_assert_eq!(address_range.len(), 2, "LcdDisplay's bus footprint is always 2 bytes (spec §4.1)");
        Self {
            name,
            address_range,
            geometry,
            dual_line: geometry.is_dual_line(),
            cgrom,
            background,
            foreground,
            instruction_nibble: NibbleState::Idle,
            data_nibble: NibbleState::Idle,
            interface_width_8bit: true,
            font_5x10: false,
            entry_id: true,
            entry_shift: false,
            display_on: false,
            cursor_on: false,
            cursor_blink: false,
            ac: AddressCounterTarget::Ddram(0),
            ddram: [0; 80],
            cgram: [0; 64],
            line_shift: [0; 2],
            clock_hz: clock_hz.unwrap_or(NOMINAL_CLOCK_HZ),
            busy_cycles_remaining: 0,
            frame_sink: None,
            external_transport: None,
            pending_frame: None,
            log_sender: LogSender::default(),
        }
    }

    /// The physical character grid fixed at configuration time (spec §3).
    pub fn geometry(&self) -> &'static Geometry {
        self.geometry
    }

    /// The character generator ROM table fixed at configuration time (spec §3, §8.1).
    pub fn cgrom(&self) -> &CgRom {
        &self.cgrom
    }

    /// The cosmetic-only background color fixed at configuration time (spec §3, §8.3).
    pub fn background(&self) -> Rgb24 {
        self.background
    }

    /// The cosmetic-only foreground color fixed at configuration time (spec §3, §8.3).
    pub fn foreground(&self) -> Rgb24 {
        self.foreground
    }

    /// Installs a log sender for diagnostic messages (e.g. `reset()`, busy-discarded accesses).
    pub fn set_log_sender(&mut self, sender: LogSender) {
        self.log_sender = sender;
    }

    /// Attaches a push channel for composited frames (design doc §7). Once set, every register
    /// write that changes what's rendered composites the current state and sends the result with
    /// [`mpsc::Sender::try_send`] -- never blocking; if the consumer isn't keeping up, the frame
    /// is silently dropped rather than stalling CPU execution, the same never-blocks contract
    /// `CharDisplay::attach_frame_sink`/`LedMatrix::attach_frame_sink` uphold.
    pub fn attach_frame_sink(&mut self, sink: mpsc::Sender<LcdDisplayFrame>) {
        self.frame_sink = Some(sink);
    }

    /// Attaches an outbound-only transport to an external peripheral process
    /// (`plan/lcd-display-external-protocol.md`), immediately sending the one-time header over
    /// `transport`; thereafter [`Self::push_frame`] also sends a frame message over it whenever it
    /// sends one to the frame sink (if attached).
    pub fn attach_external_transport(&mut self, mut transport: Box<dyn Transport>) {
        let header = protocol::encode_header(self.geometry.columns, self.geometry.rows, self.background, self.foreground);
        transport.send_bytes(&header);
        self.external_transport = Some(transport);
    }

    /// Translates the address counter's current position into a cursor state for
    /// [`compositing::composite`] (design doc §8, spec §8.3): `None` when the address counter
    /// targets CGRAM, or a DDRAM address that `line_shift` has scrolled outside every segment's
    /// visible window; otherwise the visible `(row, column)` cell it currently occupies.
    /// `visible`/`blinking` are read directly from `cursor_on`/`cursor_blink` -- unlike a real
    /// panel's time-based blink cadence, this device has no periodic tick to alternate on (design
    /// doc §6, §7), so `cursor_blink` just selects a static solid-block-vs-underline style.
    fn compositing_cursor(&self) -> CursorState {
        let position = match self.ac {
            AddressCounterTarget::Ddram(addr) => compositing::ddram_cursor_position(addr, self.geometry, &self.line_shift),
            AddressCounterTarget::Cgram(_) => None,
        };
        CursorState { position, visible: self.cursor_on, blinking: self.cursor_blink }
    }

    /// Composites the current display state and pushes it to the frame sink, if attached (design
    /// doc §7). Called after every register write that actually took effect -- an executed
    /// instruction or a completed data write, never a busy-discarded one, and never a read (spec
    /// §8.3 only lists writes as render-affecting).
    fn push_frame(&mut self) {
        if self.frame_sink.is_none() && self.external_transport.is_none() {
            return;
        }
        let cursor = self.compositing_cursor();
        let pixels = compositing::composite(
            &self.ddram,
            &self.cgram,
            self.geometry,
            &self.line_shift,
            cursor,
            self.display_on,
            self.font_5x10,
            &self.cgrom,
            self.background,
            self.foreground,
        );
        if let Some(transport) = self.external_transport.as_mut() {
            // `width_px` is fixed by `geometry`; `height_px` is derived from the buffer's own
            // length (rather than duplicating `compositing`'s private cell-height constants here)
            // since it depends on `font_5x10`, which can change at runtime (spec §8.2) -- the same
            // technique `LcdDisplayPanel.tsx`'s `decodeFrame` uses on the receiving end.
            let width_px = self.geometry.columns as u16 * 5;
            let height_px = (pixels.len() / 4 / width_px as usize) as u16;
            let message = protocol::encode_frame(width_px, height_px, &pixels);
            // If the ring is still full of an earlier, not-yet-drained frame, sending would fail
            // outright (never partially -- see `PipeTransport::send_bytes`'s atomicity guarantee).
            // Checking capacity first, rather than attempting the send and reacting to failure,
            // means a still-congested channel is never reported as a dropped-bytes event (issue
            // #587) -- it's stashed as `pending_frame` for `tick` to keep retrying instead, and a
            // still-pending older frame is implicitly replaced here, since only the latest
            // composited state is ever worth delivering (see `pending_frame`'s doc comment, issue
            // #581).
            self.pending_frame = if transport.has_outbound_capacity(message.len()) && transport.send_bytes(&message) {
                None
            } else {
                Some(message)
            };
        }
        if let Some(sink) = &self.frame_sink {
            let _ = sink.try_send(LcdDisplayFrame { pixels, columns: self.geometry.columns, rows: self.geometry.rows });
        }
    }

    fn busy(&self) -> bool {
        self.busy_cycles_remaining > 0
    }

    /// Sets `busy_cycles_remaining` from a duration in microseconds, converted to whole CPU
    /// cycles at the currently effective clock speed (spec §5).
    fn set_busy(&mut self, duration_us: u64) {
        self.busy_cycles_remaining = ((self.clock_hz as u128 * duration_us as u128) / 1_000_000).max(1) as u64;
    }

    /// Folds a raw HD44780 DDRAM address (as encoded in a `Set DDRAM Address` instruction, or as
    /// documented in spec §7.1's segment tables) into this device's flat 80-byte physical store.
    ///
    /// On real hardware, and on every *dual-line* geometry here, DDRAM address bit 6 (`0x40`)
    /// selects which of the two internal 40-byte lines an access targets, with the low 6 bits
    /// giving the position within it -- the well-known reason a real second line "starts at
    /// 0x40" even though only 80 bytes of DDRAM physically exist. Folding on the line-select bit
    /// (rather than storing the raw 0..128 value, or reducing it modulo 80) is what keeps a wide
    /// geometry's second line -- e.g. `40x2`'s `(0x40, 40)` segment -- inside the physical array
    /// without colliding with line one, while still matching the exact addresses spec §7.1
    /// documents. *Single*-line geometries (`8x1`, `40x1`) have no such split -- their whole
    /// 80-byte DDRAM is one contiguous line addressed 0x00..0x4F, so the raw value is used
    /// directly (reduced modulo 80 only as a safety clamp for out-of-range input).
    fn fold_ddram_address(&self, raw: u8) -> u8 {
        if self.dual_line {
            let line = (raw >> 6) & 1;
            let position = raw & 0x3F;
            line * 40 + (position % 40)
        } else {
            raw % 80
        }
    }

    fn execute_instruction(&mut self, byte: u8) {
        match byte {
            0x01 => {
                // Clear Display (spec §4.2): DDRAM <- 0x20, AC (DDRAM) <- 0, ID forced to
                // increment. Shift offsets are left untouched -- unlike Return Home, the spec's
                // own effect column for this instruction doesn't mention them.
                self.ddram = [0x20; 80];
                self.ac = AddressCounterTarget::Ddram(0);
                self.entry_id = true;
                self.set_busy(LONG_INSTRUCTION_US);
            }
            0x02..=0x03 => {
                // Return Home: AC (DDRAM) <- 0, shift offsets <- 0, DDRAM contents unchanged.
                self.ac = AddressCounterTarget::Ddram(0);
                self.line_shift = [0; 2];
                self.set_busy(LONG_INSTRUCTION_US);
            }
            0x04..=0x07 => {
                // Entry Mode Set: 0000 01 ID S
                self.entry_id = byte & 0x02 != 0;
                self.entry_shift = byte & 0x01 != 0;
                self.set_busy(SHORT_INSTRUCTION_US);
            }
            0x08..=0x0F => {
                // Display On/Off Control: 0000 1 D C B
                self.display_on = byte & 0x04 != 0;
                self.cursor_on = byte & 0x02 != 0;
                self.cursor_blink = byte & 0x01 != 0;
                self.set_busy(SHORT_INSTRUCTION_US);
            }
            0x10..=0x1F => {
                // Cursor or Display Shift: 0001 SC RL --
                let sc = byte & 0x08 != 0;
                let rl = byte & 0x04 != 0;
                if sc {
                    self.shift_display(rl);
                } else {
                    self.ac.advance(rl);
                }
                self.set_busy(SHORT_INSTRUCTION_US);
            }
            0x20..=0x3F => {
                // Function Set: 001 DL N F -- (N is accepted but has no observable effect here,
                // spec §7.3, and nothing reads it back, so it isn't stored).
                self.interface_width_8bit = byte & 0x10 != 0;
                self.font_5x10 = byte & 0x04 != 0;
                self.set_busy(SHORT_INSTRUCTION_US);
            }
            0x40..=0x7F => {
                // Set CGRAM Address: 01 AAAAAA
                self.ac = AddressCounterTarget::Cgram(byte & 0x3F);
                self.set_busy(SHORT_INSTRUCTION_US);
            }
            0x80..=0xFF => {
                // Set DDRAM Address: 1 AAAAAAA
                self.ac = AddressCounterTarget::Ddram(self.fold_ddram_address(byte & 0x7F));
                self.set_busy(SHORT_INSTRUCTION_US);
            }
            _ => {
                // 0x00 is not a defined HD44780 instruction; treated as a no-op, consuming no
                // busy time.
            }
        }
    }

    /// Shifts every DDRAM line's offset simultaneously by one position in the direction
    /// `forward` specifies (spec §7.4) -- matching real hardware's single shared shift mechanism,
    /// which is why `16x4`/`20x4`'s paired rows shift together once compositing exists.
    fn shift_display(&mut self, forward: bool) {
        let (count, modulus): (usize, u8) = if self.dual_line { (2, 40) } else { (1, 80) };
        for offset in self.line_shift.iter_mut().take(count) {
            *offset = if forward { (*offset + 1) % modulus } else { (*offset + modulus - 1) % modulus };
        }
    }

    fn write_data(&mut self, byte: u8) {
        let targeted_ddram = matches!(self.ac, AddressCounterTarget::Ddram(_));
        match self.ac {
            AddressCounterTarget::Ddram(addr) => self.ddram[addr as usize] = byte,
            AddressCounterTarget::Cgram(addr) => self.cgram[addr as usize] = byte,
        }
        self.ac.advance(self.entry_id);
        if targeted_ddram && self.entry_shift {
            self.shift_display(self.entry_id);
        }
        self.set_busy(SHORT_INSTRUCTION_US);
    }

    fn write_instruction_register(&mut self, raw: u8) {
        if let Some(byte) = self.instruction_nibble.feed(raw, self.interface_width_8bit) {
            if self.busy() {
                log_msg!(
                    self.log_sender,
                    LogLevel::Warn,
                    LogCategory::Device,
                    "{} discarded instruction 0x{byte:02x} write while busy",
                    self.identity()
                );
                return;
            }
            self.execute_instruction(byte);
            self.push_frame();
        }
    }

    fn write_data_register(&mut self, raw: u8) {
        if let Some(byte) = self.data_nibble.feed(raw, self.interface_width_8bit) {
            if self.busy() {
                log_msg!(
                    self.log_sender,
                    LogLevel::Warn,
                    LogCategory::Device,
                    "{} discarded data 0x{byte:02x} write while busy",
                    self.identity()
                );
                return;
            }
            self.write_data(byte);
            self.push_frame();
        }
    }

    /// Busy flag (bit 7) plus the current address counter value (bits 6:0) -- spec §4.3. Always
    /// permitted, including while busy, and never itself a source of busy time.
    fn read_instruction(&self) -> u8 {
        let busy_bit = if self.busy() { 0x80 } else { 0x00 };
        busy_bit | (self.ac.value() & 0x7F)
    }

    fn read_data(&mut self) -> u8 {
        if self.busy() {
            log_msg!(
                self.log_sender,
                LogLevel::Warn,
                LogCategory::Device,
                "{} discarded data read while busy",
                self.identity()
            );
            return 0;
        }
        let value = match self.ac {
            AddressCounterTarget::Ddram(addr) => self.ddram[addr as usize],
            AddressCounterTarget::Cgram(addr) => self.cgram[addr as usize],
        };
        // Unlike a write, a read never accompanies a display shift even when entry mode's S bit
        // is set (spec §4.4), matching real hardware exactly.
        self.ac.advance(self.entry_id);
        self.set_busy(SHORT_INSTRUCTION_US);
        value
    }

    fn peek_data(&self) -> u8 {
        match self.ac {
            AddressCounterTarget::Ddram(addr) => self.ddram[addr as usize],
            AddressCounterTarget::Cgram(addr) => self.cgram[addr as usize],
        }
    }
}

impl IoDevice for LcdDisplay {
    fn read(&mut self, address: u16) -> u8 {
        match address - self.address_range.start {
            0 => self.read_instruction(),
            1 => self.read_data(),
            _ => 0,
        }
    }

    fn write(&mut self, address: u16, value: u8) {
        match address - self.address_range.start {
            0 => self.write_instruction_register(value),
            1 => self.write_data_register(value),
            _ => {}
        }
    }

    fn peek(&self, address: u16) -> u8 {
        match address - self.address_range.start {
            0 => self.read_instruction(),
            1 => self.peek_data(),
            _ => 0,
        }
    }

    fn claims(&self, address: u16) -> bool {
        self.address_range.contains(address)
    }

    fn tick(&mut self, cycles: u32) {
        self.busy_cycles_remaining = self.busy_cycles_remaining.saturating_sub(cycles as u64);
        // Retry a frame `push_frame` couldn't deliver last time (issue #581) -- see
        // `pending_frame`'s doc comment for why this can't just be left dropped. Checked via
        // `has_outbound_capacity` first, same as `push_frame`, so a channel that's still busy
        // isn't re-reported as a fresh dropped-bytes event on every single tick it stays that way
        // (issue #587).
        if let Some(message) = &self.pending_frame
            && let Some(transport) = self.external_transport.as_mut()
            && transport.has_outbound_capacity(message.len())
            && transport.send_bytes(message)
        {
            self.pending_frame = None;
        }
    }

    fn reset(&mut self) {
        // A hardware reset re-establishes controller mode state (spec §2.2: always 8-bit, not
        // mid-nibble, immediately after reset) but -- like every other display device here --
        // leaves DDRAM/CGRAM contents untouched; only `Clear Display` clears DDRAM.
        self.instruction_nibble = NibbleState::Idle;
        self.data_nibble = NibbleState::Idle;
        self.interface_width_8bit = true;
        self.font_5x10 = false;
        self.entry_id = true;
        self.entry_shift = false;
        self.display_on = false;
        self.cursor_on = false;
        self.cursor_blink = false;
        self.ac = AddressCounterTarget::Ddram(0);
        self.line_shift = [0; 2];
        self.busy_cycles_remaining = 0;
        log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} reset", self.identity());
    }

    fn name(&self) -> &str {
        self.name
    }

    fn identity_address(&self) -> u16 {
        self.address_range.start
    }

    /// Drops the frame sink, closing the channel from this end -- the channel equivalent of the
    /// terminal bridge seeing EOF on its pipe, ending the debugger's LCD display bridge task's
    /// `recv()` loop -- and shuts down the external transport, if attached. Mirrors
    /// `CharDisplay::shutdown`/`LedMatrix::shutdown`.
    fn shutdown(&mut self) {
        self.frame_sink = None;
        self.pending_frame = None;
        if let Some(transport) = self.external_transport.as_mut() {
            transport.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE_NAME: &str = "lcd_display";
    const BASE_ADDRESS: u16 = 0xD000;

    const DUAL_LINE_GEOMETRY: Geometry =
        Geometry { rows: 2, columns: 16, segments: &[&[(0x00, 16)], &[(0x40, 16)]] };
    const SINGLE_LINE_GEOMETRY: Geometry =
        Geometry { rows: 1, columns: 40, segments: &[&[(0x00, 40)]] };

    fn address_range() -> AddressRange {
        AddressRange::new(BASE_ADDRESS, BASE_ADDRESS + 1)
    }

    fn device() -> LcdDisplay {
        LcdDisplay::new(DEVICE_NAME, address_range(), &DUAL_LINE_GEOMETRY, Some(1_000_000),
            CgRom::default(), Rgb24::new(0, 0, 0), Rgb24::new(255, 255, 255))
    }

    fn single_line_device() -> LcdDisplay {
        LcdDisplay::new(DEVICE_NAME, address_range(), &SINGLE_LINE_GEOMETRY, Some(1_000_000),
            CgRom::default(), Rgb24::new(0, 0, 0), Rgb24::new(255, 255, 255))
    }

    fn instruction_addr() -> u16 {
        BASE_ADDRESS
    }

    fn data_addr() -> u16 {
        BASE_ADDRESS + 1
    }

    /// Ticks past whatever busy period is currently in effect (short or long), at the 1 MHz
    /// clock every test device is constructed with.
    fn tick_past_busy(device: &mut LcdDisplay) {
        device.tick(2_000);
    }

    #[test]
    fn claims_only_within_configured_range() {
        let device = device();
        assert!(device.claims(BASE_ADDRESS));
        assert!(device.claims(BASE_ADDRESS + 1));
        assert!(!device.claims(BASE_ADDRESS - 1));
        assert!(!device.claims(BASE_ADDRESS + 2));
    }

    #[test]
    fn identity_address_and_name() {
        let device = device();
        assert_eq!(device.name(), DEVICE_NAME);
        assert_eq!(device.identity_address(), BASE_ADDRESS);
        assert_eq!(device.identity(), format!("{DEVICE_NAME}@{BASE_ADDRESS:#06x}"));
    }

    #[test]
    fn reset_state_is_eight_bit_and_not_mid_nibble() {
        let device = device();
        assert_eq!(device.peek(instruction_addr()), 0);
    }

    #[test]
    fn eight_bit_mode_data_write_is_a_single_access() {
        let mut device = device();
        device.write(data_addr(), 0x41);
        assert_eq!(device.peek(data_addr()), 0); // AC already advanced to 1 after the write
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0x41);
    }

    #[test]
    fn four_bit_mode_nibble_pair_write_assembles_full_instruction_byte() {
        let mut device = device();
        device.write(instruction_addr(), 0x20); // Function Set, DL=0 (4-bit), N=0, F=0
        tick_past_busy(&mut device);

        // Set DDRAM Address 0x05, as two nibbles: high nibble 0x8_, low nibble 0x5_.
        device.write(instruction_addr(), 0x80);
        device.write(instruction_addr(), 0x50);
        tick_past_busy(&mut device);

        assert_eq!(device.peek(instruction_addr()) & 0x7F, 5);
    }

    #[test]
    fn four_bit_mode_data_nibble_pair_produces_correct_assembled_byte() {
        let mut device = device();
        device.write(instruction_addr(), 0x20); // Function Set, DL=0 (4-bit)
        tick_past_busy(&mut device);

        // Assemble 0x41 ('A') from a high nibble of 0x4_ and a low nibble of 0x1_.
        device.write(data_addr(), 0x40);
        device.write(data_addr(), 0x10);
        tick_past_busy(&mut device);

        // AC has already advanced past the written cell; rewind it (also via 4-bit nibbles) to
        // read the byte back.
        device.write(instruction_addr(), 0x80);
        device.write(instruction_addr(), 0x00);
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0x41);
    }

    #[test]
    fn four_bit_mode_nibble_pairing_is_independent_per_register() {
        let mut device = device();
        device.write(instruction_addr(), 0x20); // Function Set, DL=0 (4-bit)
        tick_past_busy(&mut device);

        // Begin a data-register pairing (high nibble only)...
        device.write(data_addr(), 0x40);
        // ...and interleave a complete, unrelated instruction-register pairing (Entry Mode Set
        // 0x06: ID=1, S=0): high nibble 0x0_, low nibble 0x6_.
        device.write(instruction_addr(), 0x00);
        device.write(instruction_addr(), 0x60);
        tick_past_busy(&mut device);
        // Finish the data-register pairing with its low nibble.
        device.write(data_addr(), 0x10);
        tick_past_busy(&mut device);

        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        device.write(instruction_addr(), 0x00);
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0x41, "the interleaved instruction must not disturb the in-progress data nibble pairing");
    }

    #[test]
    fn busy_gates_a_too_early_access_without_extending_the_busy_period() {
        let mut device = device();
        device.write(instruction_addr(), 0x01); // Clear Display (long)
        assert_ne!(device.peek(instruction_addr()) & 0x80, 0, "expected busy immediately after Clear Display");

        // A write arriving while busy is discarded outright.
        device.write(data_addr(), 0xFF);
        device.tick(1); // not remotely enough to clear a long busy period
        assert_ne!(device.peek(instruction_addr()) & 0x80, 0, "still busy");
        assert_eq!(device.peek(data_addr()), 0x20, "the discarded write must not have reached DDRAM");

        tick_past_busy(&mut device);
        assert_eq!(device.peek(instruction_addr()) & 0x80, 0, "busy must clear once its period elapses");

        device.write(data_addr(), 0x41);
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0x41, "a write after busy clears must succeed");
    }

    #[test]
    fn busy_discard_does_not_disturb_an_in_progress_nibble_pairing() {
        let mut device = device();
        device.write(instruction_addr(), 0x20); // Function Set, DL=0 (4-bit)
        tick_past_busy(&mut device);

        // Clear Display (0x01), sent as two nibbles: high nibble 0x0_, low nibble 0x1_.
        device.write(instruction_addr(), 0x01);
        device.write(instruction_addr(), 0x10);
        assert_ne!(device.peek(instruction_addr()) & 0x80, 0, "expected busy after Clear Display");

        // A second instruction, sent as two nibbles, arrives while busy: both nibbles are still
        // consumed by the nibble-pairing state machine, but the assembled instruction is discarded.
        device.write(instruction_addr(), 0x00);
        device.write(instruction_addr(), 0x60);
        assert_ne!(device.peek(instruction_addr()) & 0x80, 0, "still busy; the discarded instruction must not have reset busy");

        tick_past_busy(&mut device);
        // Nibble state must be back to Idle (not stuck mid-pair) -- a fresh instruction assembles
        // correctly: Entry Mode Set (0x06), ID=1, S=0.
        device.write(instruction_addr(), 0x00);
        device.write(instruction_addr(), 0x60);
        tick_past_busy(&mut device);
        // If nibble state were corrupted, this Clear Display (0x01) would not take effect as a
        // clean single instruction; confirm DDRAM was actually cleared.
        device.write(instruction_addr(), 0x01);
        device.write(instruction_addr(), 0x10);
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80);
        device.write(instruction_addr(), 0x00);
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0x20);
    }

    #[test]
    fn data_read_while_busy_is_ignored_and_returns_zero() {
        let mut device = device();
        device.write(data_addr(), 0x41);
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0x41);

        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0 (short busy)
        // Immediately read while busy, before ticking.
        assert_eq!(device.read(data_addr()), 0);
    }

    #[test]
    fn ddram_address_counter_wraps_at_eighty_on_increment() {
        // Single-line geometry: raw address maps directly to the physical index (no line fold),
        // so 0x4F (79) is unambiguously the last valid DDRAM position.
        let mut device = single_line_device();
        device.write(instruction_addr(), 0x80 | 0x4F); // Set DDRAM Address 79
        tick_past_busy(&mut device);
        assert_eq!(device.peek(instruction_addr()) & 0x7F, 79);

        device.write(data_addr(), 0x2A);
        tick_past_busy(&mut device);
        assert_eq!(device.peek(instruction_addr()) & 0x7F, 0, "increment past 79 must wrap to 0");
    }

    #[test]
    fn ddram_address_counter_wraps_at_eighty_on_decrement() {
        let mut device = single_line_device();
        device.write(instruction_addr(), 0x04); // Entry Mode Set, ID=0 (decrement), S=0
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);

        device.write(data_addr(), 0x2A);
        tick_past_busy(&mut device);
        assert_eq!(device.peek(instruction_addr()) & 0x7F, 79, "decrement past 0 must wrap to 79");
    }

    #[test]
    fn cgram_address_counter_wraps_at_sixty_four() {
        let mut device = device();
        device.write(instruction_addr(), 0x7F); // Set CGRAM Address 63
        tick_past_busy(&mut device);
        assert_eq!(device.peek(instruction_addr()) & 0x7F, 63);

        device.write(data_addr(), 0x1F);
        tick_past_busy(&mut device);
        assert_eq!(device.peek(instruction_addr()) & 0x7F, 0, "increment past 63 must wrap to 0");
    }

    #[test]
    fn dual_line_geometry_folds_second_line_address_into_upper_half() {
        let mut device = device(); // DUAL_LINE_GEOMETRY
        device.write(instruction_addr(), 0xC0); // Set DDRAM Address 0x40 (raw): line 2, position 0
        tick_past_busy(&mut device);
        assert_eq!(device.peek(instruction_addr()) & 0x7F, 40, "0x40 must fold to physical index 40, not 64");

        device.write(data_addr(), b'A');
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0 (line 1)
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0, "line 1's own address 0 must be untouched by the line-2 write");
    }

    #[test]
    fn single_line_geometry_uses_raw_address_directly() {
        let mut device = single_line_device();
        device.write(instruction_addr(), 0x80 | 0x28); // Set DDRAM Address 40 (no line-2 concept here)
        tick_past_busy(&mut device);
        assert_eq!(device.peek(instruction_addr()) & 0x7F, 40);
    }

    #[test]
    fn clear_display_fills_ddram_with_spaces_and_resets_address_counter() {
        let mut device = device();
        device.write(data_addr(), 0x41);
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x04); // Entry Mode Set, ID=0 (decrement)
        tick_past_busy(&mut device);

        device.write(instruction_addr(), 0x01); // Clear Display
        tick_past_busy(&mut device);

        assert_eq!(device.peek(instruction_addr()) & 0x7F, 0);
        assert_eq!(device.peek(data_addr()), 0x20);
        device.write(instruction_addr(), 0x7F); // Set CGRAM Address 63, just to move AC off 0...
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // ...then back to DDRAM 0, to scan every DDRAM cell
        tick_past_busy(&mut device);
        for _ in 0..80 {
            assert_eq!(device.read(data_addr()), 0x20);
            tick_past_busy(&mut device);
        }
    }

    #[test]
    fn return_home_resets_shift_offsets_but_not_ddram_contents() {
        let mut device = device();
        device.write(data_addr(), 0x41);
        tick_past_busy(&mut device);

        device.write(instruction_addr(), 0x10 | 0x08 | 0x04); // Cursor or Display Shift, SC=1, RL=1
        tick_past_busy(&mut device);
        assert_eq!(device.line_shift, [1, 1]);

        device.write(instruction_addr(), 0x02); // Return Home
        tick_past_busy(&mut device);
        assert_eq!(device.line_shift, [0, 0]);

        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0x41, "Return Home must not touch DDRAM contents");
    }

    #[test]
    fn display_shift_moves_every_line_together() {
        let mut device = device(); // DUAL_LINE_GEOMETRY: two independent 40-byte lines
        device.write(instruction_addr(), 0x10 | 0x08 | 0x04); // SC=1, RL=1 (shift right)
        tick_past_busy(&mut device);
        assert_eq!(device.line_shift, [1, 1], "a shared shift must move both lines, not just the one AC targets");
    }

    #[test]
    fn cursor_only_shift_moves_address_counter_without_touching_shift_offsets_or_ddram() {
        let mut device = device();
        device.write(data_addr(), 0x41);
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);

        device.write(instruction_addr(), 0x10 | 0x04); // SC=0, RL=1 (cursor right)
        tick_past_busy(&mut device);

        assert_eq!(device.peek(instruction_addr()) & 0x7F, 1, "cursor-only shift must move the address counter");
        assert_eq!(device.line_shift, [0, 0], "cursor-only shift must not touch shift offsets");
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0x41, "cursor-only shift must not touch DDRAM contents");
    }

    #[test]
    fn entry_mode_shift_moves_display_in_the_same_direction_as_id() {
        let mut device = device();
        device.write(instruction_addr(), 0x04 | 0x01); // Entry Mode Set, ID=0 (decrement), S=1
        tick_past_busy(&mut device);

        device.write(data_addr(), b'A');
        tick_past_busy(&mut device);

        assert_eq!(device.line_shift, [39, 39], "S accompanying a decrementing write must shift backward");
    }

    #[test]
    fn function_set_selects_font_height() {
        let mut device = device();
        device.write(instruction_addr(), 0x20 | 0x10 | 0x04); // Function Set, DL=1 (8-bit), F=1
        tick_past_busy(&mut device);
        assert!(device.font_5x10);
    }

    #[test]
    fn reset_returns_to_eight_bit_mode_and_clears_pending_nibble() {
        let mut device = device();
        device.write(instruction_addr(), 0x20); // Function Set, DL=0 (4-bit)
        tick_past_busy(&mut device);
        device.write(data_addr(), 0x40); // begin a nibble pair, leave it pending

        device.reset();

        assert_eq!(device.data_nibble, NibbleState::Idle);
        assert!(device.interface_width_8bit);
        // A plain 8-bit write now takes effect as a single, complete access.
        device.write(data_addr(), 0x41);
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);
        assert_eq!(device.peek(data_addr()), 0x41);
    }

    #[test]
    fn reset_preserves_ddram_contents() {
        let mut device = device();
        device.write(data_addr(), 0x41);
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);

        device.reset();

        assert_eq!(device.peek(data_addr()), 0x41);
    }

    #[test]
    fn reset_logs_device_message() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let mut device = device();
        device.set_log_sender(sender);
        device.reset();
        let received = rx.recv().unwrap();
        assert_eq!(received.category, LogCategory::Device);
        assert_eq!(received.message, format!("{DEVICE_NAME}@0x{BASE_ADDRESS:04x} reset"));
    }

    #[test]
    fn a_completed_data_write_pushes_a_composited_frame_to_an_attached_sink() {
        let mut device = device(); // DUAL_LINE_GEOMETRY: 16x2
        let (tx, mut rx) = mpsc::channel(4);
        device.attach_frame_sink(tx);

        device.write(data_addr(), 0x41);

        let frame = rx.try_recv().expect("expected a composited frame after a completed data write");
        assert_eq!(frame.columns, 16);
        assert_eq!(frame.rows, 2);
        assert_eq!(frame.pixels.len(), 16 * 5 * 2 * 8 * 4);
    }

    #[test]
    fn a_completed_instruction_pushes_a_composited_frame_to_an_attached_sink() {
        let mut device = device();
        let (tx, mut rx) = mpsc::channel(4);
        device.attach_frame_sink(tx);

        device.write(instruction_addr(), 0x08 | 0x04); // Display On/Off Control, D=1
        assert!(rx.try_recv().is_ok(), "expected a composited frame after a completed instruction");
    }

    #[test]
    fn a_busy_discarded_write_does_not_push_a_frame() {
        let mut device = device();
        let (tx, mut rx) = mpsc::channel(4);
        device.attach_frame_sink(tx);

        device.write(instruction_addr(), 0x01); // Clear Display (long busy)
        rx.try_recv().expect("expected a frame from the Clear Display instruction itself");

        device.write(data_addr(), 0xFF); // discarded while busy
        assert!(rx.try_recv().is_err(), "a busy-discarded write must not push a frame");
    }

    #[test]
    fn a_data_read_does_not_push_a_frame() {
        let mut device = device();
        device.write(data_addr(), 0x41);
        tick_past_busy(&mut device);
        device.write(instruction_addr(), 0x80); // Set DDRAM Address 0
        tick_past_busy(&mut device);

        let (tx, mut rx) = mpsc::channel(4);
        device.attach_frame_sink(tx);
        device.read(data_addr());
        assert!(rx.try_recv().is_err(), "a read must never push a frame (spec §8.3 lists only writes)");
    }

    #[test]
    fn shutdown_drops_the_frame_sink_closing_the_channel() {
        let mut device = device();
        let (tx, mut rx) = mpsc::channel(4);
        device.attach_frame_sink(tx);

        device.shutdown();

        device.write(data_addr(), 0x41);
        assert!(rx.try_recv().is_err(), "channel should be closed once the sink is dropped");
    }

    // -- External transport (`plan/lcd-display-external-protocol.md`) --

    use crate::emulator::transport::InternalPipeTransport;

    /// `remote` is the peripheral's end of the pipe: everything the device sends over the
    /// external transport lands here, byte by byte, and can be collected with [`collect_bytes`].
    /// Like `LedMatrix`'s equivalent helper, no relay is needed -- this device's transport is
    /// outbound-only, so `pair_direct()`'s bare, unrelayed ends suffice.
    fn device_with_external_transport() -> (LcdDisplay, InternalPipeTransport) {
        let (local, remote) = InternalPipeTransport::pair_direct().unwrap();
        let mut device = device(); // DUAL_LINE_GEOMETRY: 16x2
        device.attach_external_transport(Box::new(local));
        (device, remote)
    }

    fn collect_bytes(remote: &mut InternalPipeTransport) -> Vec<u8> {
        let mut buf = Vec::new();
        while let Some(b) = remote.try_recv() {
            buf.push(b);
        }
        buf
    }

    #[test]
    fn attach_external_transport_sends_header_immediately() {
        let (_device, mut remote) = device_with_external_transport();
        let bytes = collect_bytes(&mut remote);

        assert_eq!(&bytes[0..4], b"E65L");
        assert_eq!(bytes[4], 1); // version
        assert_eq!(bytes[5], 16); // columns
        assert_eq!(bytes[6], 2); // rows
        assert_eq!(&bytes[7..10], &[0, 0, 0]); // background, from device()
        assert_eq!(&bytes[10..13], &[255, 255, 255]); // foreground, from device()
        assert_eq!(bytes.len(), 13);
    }

    #[test]
    fn a_completed_data_write_sends_a_frame_message_over_external_transport() {
        let (mut device, mut remote) = device_with_external_transport();
        collect_bytes(&mut remote); // drain the header

        device.write(data_addr(), 0x41);

        let bytes = collect_bytes(&mut remote);
        let width_px = 16u16 * 5;
        let height_px = 2u16 * 8; // 5x8 font by default
        assert_eq!(&bytes[0..2], &width_px.to_le_bytes());
        assert_eq!(&bytes[2..4], &height_px.to_le_bytes());
        assert_eq!(bytes.len(), 4 + width_px as usize * height_px as usize * 4);
    }

    #[test]
    fn switching_to_5x10_font_changes_the_next_frame_message_height() {
        let (mut device, mut remote) = device_with_external_transport();
        collect_bytes(&mut remote); // drain the header

        device.write(instruction_addr(), 0x20 | 0x10 | 0x04); // Function Set, DL=1 (8-bit), F=1
        tick_past_busy(&mut device);

        let bytes = collect_bytes(&mut remote);
        let width_px = 16u16 * 5;
        let height_px = 2u16 * 10;
        assert_eq!(&bytes[0..2], &width_px.to_le_bytes());
        assert_eq!(&bytes[2..4], &height_px.to_le_bytes());
        assert_eq!(bytes.len(), 4 + width_px as usize * height_px as usize * 4);
    }

    #[test]
    fn a_busy_discarded_write_sends_no_frame_message_over_external_transport() {
        let (mut device, mut remote) = device_with_external_transport();
        collect_bytes(&mut remote); // drain the header

        device.write(instruction_addr(), 0x01); // Clear Display (long busy)
        collect_bytes(&mut remote); // drain the frame from the Clear Display instruction itself

        device.write(data_addr(), 0xFF); // discarded while busy
        assert!(collect_bytes(&mut remote).is_empty(), "a busy-discarded write must not send a frame message");
    }

    #[test]
    fn a_data_read_sends_no_frame_message_over_external_transport() {
        let (mut device, mut remote) = device_with_external_transport();
        collect_bytes(&mut remote); // drain the header
        device.read(data_addr());
        assert!(collect_bytes(&mut remote).is_empty(), "a read must never send a frame message (spec §8.3 lists only writes)");
    }

    #[test]
    fn shutdown_stops_further_sends_over_external_transport() {
        let (mut device, mut remote) = device_with_external_transport();
        collect_bytes(&mut remote); // drain the header

        device.shutdown();
        device.write(data_addr(), 0x41);

        assert!(collect_bytes(&mut remote).is_empty());
    }

    /// A [`Transport`] double whose `send_bytes` can be switched between always-succeeding and
    /// always-failing (as `PipeTransport`'s does when its ring is full), via a cloned handle kept
    /// by the test, so `pending_frame`'s retry behavior (issue #581) can be exercised
    /// deterministically -- `InternalPipeTransport` can't stand in for this, since its
    /// `send_bytes` (the `Transport` trait's default byte-by-byte loop) always returns `true`
    /// regardless of whether the underlying OS pipe accepted the write.
    #[derive(Clone)]
    struct ControllableTransportHandle {
        accepting: std::sync::Arc<std::sync::atomic::AtomicBool>,
        sent: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
        /// Counts every `send_bytes` call attempted against this double, regardless of whether
        /// `accepting` made it succeed or fail -- lets a test distinguish "no attempt was made"
        /// (because `has_outbound_capacity` said not to bother, issue #587) from "an attempt was
        /// made and failed" (because `accepting` was false at the time).
        send_attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ControllableTransportHandle {
        fn set_accepting(&self, accepting: bool) {
            self.accepting.store(accepting, std::sync::atomic::Ordering::SeqCst);
        }

        fn sent(&self) -> Vec<Vec<u8>> {
            self.sent.lock().unwrap().clone()
        }

        fn send_attempts(&self) -> usize {
            self.send_attempts.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    struct ControllableTransport {
        handle: ControllableTransportHandle,
    }

    impl ControllableTransport {
        fn new() -> (Self, ControllableTransportHandle) {
            let handle = ControllableTransportHandle {
                accepting: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                send_attempts: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                sent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            };
            (Self { handle: handle.clone() }, handle)
        }
    }

    impl Transport for ControllableTransport {
        fn send(&mut self, byte: u8) {
            self.send_bytes(&[byte]);
        }

        fn send_bytes(&mut self, bytes: &[u8]) -> bool {
            self.handle.send_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.handle.accepting.load(std::sync::atomic::Ordering::SeqCst) {
                self.handle.sent.lock().unwrap().push(bytes.to_vec());
                true
            } else {
                false
            }
        }

        fn has_outbound_capacity(&self, _len: usize) -> bool {
            self.handle.accepting.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn is_connected(&self) -> bool {
            true
        }

        fn shutdown(&mut self) {}
    }

    #[test]
    fn a_frame_that_cannot_be_sent_immediately_is_retried_on_tick_until_it_succeeds() {
        let mut device = device();
        let (transport, handle) = ControllableTransport::new();
        handle.set_accepting(false);
        device.attach_external_transport(Box::new(transport)); // header itself is dropped here

        device.write(data_addr(), 0x41);
        assert!(device.pending_frame.is_some(), "a failed send must be stashed for retry");

        tick_past_busy(&mut device); // still failing: must remain pending, not be lost
        assert!(device.pending_frame.is_some());
        assert!(handle.sent().is_empty());

        handle.set_accepting(true);
        tick_past_busy(&mut device);
        assert!(device.pending_frame.is_none(), "a retry that succeeds must clear the pending frame");
        assert_eq!(handle.sent().len(), 1);
    }

    #[test]
    fn a_retry_against_a_still_congested_channel_never_attempts_the_send() {
        // Regression test for issue #587: previously, `tick` retried a still-pending frame by
        // calling `send_bytes` unconditionally, so every tick the channel stayed congested
        // re-triggered `PipeTransport`'s outbound-drop counting for the *same* undelivered frame
        // -- turning one transient backpressure episode into a "bytes dropped" count in the
        // millions, despite the frame always eventually getting through intact. Checking
        // `has_outbound_capacity` first means a known-still-full channel is never even attempted,
        // so no drop is ever reported for it.
        let mut device = device();
        let (transport, handle) = ControllableTransport::new();
        handle.set_accepting(false);
        device.attach_external_transport(Box::new(transport));
        handle.send_attempts.store(0, std::sync::atomic::Ordering::SeqCst); // discount the header send

        device.write(data_addr(), 0x41);
        assert!(device.pending_frame.is_some());
        assert_eq!(handle.send_attempts(), 0, "a known-congested channel must not even be attempted");

        for _ in 0..1000 {
            tick_past_busy(&mut device);
        }
        assert_eq!(
            handle.send_attempts(), 0,
            "retrying against a still-congested channel must never attempt the send, no matter how many ticks pass"
        );
        assert!(device.pending_frame.is_some());

        handle.set_accepting(true);
        tick_past_busy(&mut device);
        assert_eq!(handle.send_attempts(), 1, "capacity becoming available must trigger exactly one attempt");
        assert!(device.pending_frame.is_none());
    }

    #[test]
    fn a_second_write_before_the_first_frame_is_delivered_replaces_the_pending_frame() {
        let mut device = device();
        let (transport, handle) = ControllableTransport::new();
        handle.set_accepting(false); // header itself is dropped here, so `sent` below counts frames only
        device.attach_external_transport(Box::new(transport));

        device.write(instruction_addr(), 0x0C); // Display On, cursor off, blink off
        tick_past_busy(&mut device); // display is off at reset, so composited frames are all-blank until this

        device.write(data_addr(), 0x41);
        let first_pending = device.pending_frame.clone();
        tick_past_busy(&mut device); // clears busy without changing DDRAM content

        device.write(data_addr(), 0x42); // different DDRAM content -> a different composited frame
        let second_pending = device.pending_frame.clone();

        assert_ne!(first_pending, second_pending, "the newer frame must replace the older one, not queue behind it");

        handle.set_accepting(true);
        tick_past_busy(&mut device);
        let sent = handle.sent();
        assert_eq!(sent.len(), 1, "only the latest frame should ever reach the transport, never a backlog");
        assert_eq!(Some(sent[0].clone()), second_pending);
    }
}
