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
//! | Status register  | `2*cells + 1` | 1           | R      | Bit 0: vsync flag, read-to-clear |
//!
//! This device is not IRQ-capable; nothing in its register map asserts an interrupt.
//!
//! Compositing (turning character/color RAM plus a palette and glyph font into pixels) lives in
//! [`compositing`] and [`font`]. Configuration wiring lives in `emulator::config::display`, added
//! in a later work unit; this module is bus-facing register and buffer-swap behavior only.

pub mod compositing;
pub mod font;
pub mod palette;

use self::compositing::Rgb24;
use self::font::Font;
use crate::emulator::{AddressRange, IoDevice};

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
/// Status register bit 0: vsync flag. Read-to-clear.
const STATUS_VSYNC: u8 = 0b0000_0001;

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
            cycles_per_frame,
            cycle_accumulator: 0,
            font,
            palette,
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

    /// The vsync-equivalent tick (spec §5.3): sets the vsync status flag and performs a pending
    /// swap, if any.
    fn on_vsync(&mut self) {
        self.status |= STATUS_VSYNC;
        if self.swap_pending {
            self.perform_swap();
            self.swap_pending = false;
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
            self.status &= !STATUS_VSYNC;
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
        }
        // Status register writes are ignored.
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
        self.cycle_accumulator = 0;
    }

    fn name(&self) -> &str {
        self.name
    }

    fn identity_address(&self) -> u16 {
        self.address_range.start
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
            palette::default_palette(),
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
            palette::default_palette(),
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
}
