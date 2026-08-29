//! A memory-mapped RGB LED matrix display device.
//!
//! See `doc/memory-mapped-led-matrix-device-spec.md` for the full behavioral specification and
//! `doc/memory-mapped-led-matrix-device-plan.md` for the design decisions this implementation
//! follows. Summary of the bus-addressable memory map (offsets relative to the device's base
//! address, `pixel_bytes = matrices * 1024`):
//!
//! | Region           | Offset            | Size          | Access | Notes                     |
//! |------------------|-------------------|---------------|--------|-----------------------------|
//! | Pixel memory     | `0`               | `pixel_bytes` | R/W    | Matrix *n* at `[n*1024, n*1024+1023]`, palette index per pixel, row-major |
//! | Command register | `pixel_bytes`     | 1             | W      | Selects/arms an operation (spec §4.2). Always reads `0` |
//! | Data register    | `pixel_bytes + 1` | 1             | R/W    | Argument byte(s) for the armed operation (spec §4.3)    |
//!
//! Unlike `CharDisplay`, this device has no control/status registers and no IRQ capability --
//! swaps are always synchronous (spec §5.2) -- and a single uniform command/data register pair
//! drives every operation (swap, auto-refresh, power, brightness, palette read/write) instead of
//! dedicated bitfield registers (design doc §1).
//!
//! **Command/data state machine** (design doc §5): writing the command register always replaces
//! [`PendingOp`] wholesale, discarding whatever partial sequence was in progress, and arms a
//! byte-sequence state machine on the data register. Write-sequence commands (`CMD_SWAP`,
//! `CMD_SET_AUTOREFRESH`, `CMD_SET_POWER`, `CMD_SET_BRIGHTNESS`, `CMD_PALETTE_WRITE`) apply their
//! effect once their full argument sequence has been written; `CMD_PALETTE_READ`'s one write byte
//! (the palette index) instead resolves the addressed entry immediately and arms a 3-byte *read*
//! sequence, popped one byte per subsequent data-register read.
//!
//! Palette entries are stored as [`compositing::Rgb565`], not full RGB24 -- `CMD_PALETTE_WRITE`/
//! `CMD_PALETTE_READ` still exchange 8-bit-per-channel bytes with the CPU, masked down on write
//! and scaled back up on read (spec §4.2.1).
//!
//! This device is not yet reachable from configuration (`emulator::config`) or compositing/frame
//! delivery -- those are later work units of the same plan.

pub mod compositing;

use tokio::sync::mpsc;

use self::compositing::Rgb565;
use crate::emulator::{AddressRange, IoDevice, LogCategory, LogLevel, LogSender, log_msg};

/// Pixels per matrix: a fixed 32x32 grid (spec §2), one palette-index byte per pixel.
pub const PIXELS_PER_MATRIX: usize = 32 * 32;

/// Number of palette entries (spec §2.1): fixed, not derived from a configurable palette length
/// like `CharDisplay`'s.
pub const PALETTE_LEN: usize = 256;

const CMD_SWAP: u8 = 0;
const CMD_SET_AUTOREFRESH: u8 = 1;
const CMD_SET_POWER: u8 = 2;
const CMD_SET_BRIGHTNESS: u8 = 3;
const CMD_PALETTE_WRITE: u8 = 4;
const CMD_PALETTE_READ: u8 = 5;

/// The operation a just-armed command register write selects (design doc §5). Distinct from
/// [`PendingOp`], which additionally tracks how many argument bytes have been collected so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Swap,
    SetAutorefresh,
    SetPower,
    SetBrightness,
    PaletteWrite,
    PaletteRead,
}

/// State machine for the command/data register pair (design doc §5): either idle, collecting a
/// write-sequence command's argument bytes, or popping a read-sequence command's result bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOp {
    Idle,
    Write { command: Command, buffer: [u8; 4], filled: usize, expected: usize },
    Read { remaining: [u8; 3], next: usize },
}

/// Computes the bitmask covering `matrices` matrices (bits `0..matrices` set), used for the
/// dirty/auto-refresh/power masks' construction- and reset-time defaults (design doc §7). `8`
/// matrices fills the mask completely rather than overflowing the `1 << 8` shift.
fn matrix_mask(matrices: u32) -> u8 {
    if matrices >= 8 { 0xFF } else { ((1u16 << matrices) - 1) as u8 }
}

/// One matrix's newly composited frame (design doc §10), pushed to an attached sink on every
/// swap of that matrix -- whether by `CMD_SWAP` or auto-refresh. Unlike `DisplayFrame`, which
/// `CharDisplay` pushes wholesale once per vsync, this is delivered per matrix actually swapped,
/// since this device's swap granularity is per-matrix (design doc §7).
pub struct LedMatrixFrame {
    /// Which matrix this frame belongs to.
    pub matrix_index: u8,
    /// RGBA pixels (32x32x4 bytes), from `compositing::composite_matrix`.
    pub pixels: Vec<u8>,
}

/// A memory-mapped RGB LED matrix display device.
pub struct LedMatrix {
    name: &'static str,
    address_range: AddressRange,
    matrices: u32,
    pixel_bytes: usize,

    /// CPU-addressable pixel memory. Fixed identity for the device's lifetime -- the CPU always
    /// reads/writes this buffer, regardless of swap state (spec §5.1).
    pixels: Vec<u8>,
    /// Scanout buffers, one contiguous `pixel_bytes`-length buffer covering every matrix,
    /// populated per-matrix by [`Self::swap_matrix`].
    scanout: Vec<u8>,

    /// Per-matrix dirty flags, one bit per matrix (spec §4.1).
    dirty: u8,
    /// Persistent auto-refresh mask, one bit per matrix (spec §6).
    autorefresh_mask: u8,
    /// Persistent power-state mask, one bit per matrix (spec §4.2, design doc §6). Pure state in
    /// this work unit -- no visible effect yet.
    power_mask: u8,
    /// Global brightness level, `0..=255` (spec §4.2, design doc §6). Pure state in this work
    /// unit -- no visible effect yet. Defaults to full brightness, matching the "works out of the
    /// box" rationale already applied to the auto-refresh and power defaults.
    brightness: u8,

    /// The single, shared 256-entry color palette (spec §2), mutable at runtime only via
    /// `CMD_PALETTE_WRITE` (spec §4.2).
    palette: Vec<Rgb565>,

    /// In-progress command/data register sequence (design doc §5).
    pending: PendingOp,

    /// Cycle-accounted auto-refresh cadence (design doc §8), reusing `CharDisplay`'s approach
    /// exactly: the fixed number of CPU cycles per frame, derived once at construction from
    /// `clock_hz` (or [`crate::emulator::device::display::NOMINAL_CLOCK_HZ`] as a fallback) and
    /// `frame_rate_hz`.
    cycles_per_frame: u64,
    cycle_accumulator: u64,

    /// Sender for structured diagnostic messages (e.g. `reset()`).
    log_sender: LogSender,

    /// Push channel for composited per-matrix frames (design doc §10), attached by the config
    /// module when a host (the debugger) wants to receive them.
    frame_sink: Option<mpsc::Sender<LedMatrixFrame>>,
}

impl LedMatrix {
    /// Creates a new device. `address_range` must span exactly `matrices * 1024 + 2` bytes;
    /// callers (the config module) are responsible for computing it from `matrices`.
    ///
    /// `clock_hz` is the CPU's configured clock speed in Hz, or `None` if the CPU runs
    /// unthrottled (`ClockSpeed::unlimited()`); see
    /// [`crate::emulator::device::display::NOMINAL_CLOCK_HZ`].
    ///
    /// `palette` is the initial 256-entry color palette (spec §2.1); it remains mutable at
    /// runtime via `CMD_PALETTE_WRITE`.
    pub fn new(
        name: &'static str,
        address_range: AddressRange,
        matrices: u32,
        clock_hz: Option<u64>,
        frame_rate_hz: u32,
        palette: Vec<Rgb565>,
    ) -> Self {
        debug_assert!((1..=8).contains(&matrices), "matrices must be 1..=8 (validated by the config module)");
        debug_assert_eq!(palette.len(), PALETTE_LEN, "palette must have exactly PALETTE_LEN entries");
        let pixel_bytes = matrices as usize * PIXELS_PER_MATRIX;
        let effective_clock_hz = clock_hz.unwrap_or(super::display::NOMINAL_CLOCK_HZ);
        let cycles_per_frame = (effective_clock_hz / frame_rate_hz.max(1) as u64).max(1);
        let mask = matrix_mask(matrices);
        Self {
            name,
            address_range,
            matrices,
            pixel_bytes,
            pixels: vec![0; pixel_bytes],
            scanout: vec![0; pixel_bytes],
            dirty: mask,
            autorefresh_mask: mask,
            power_mask: mask,
            brightness: 0xFF,
            palette,
            pending: PendingOp::Idle,
            cycles_per_frame,
            cycle_accumulator: 0,
            log_sender: LogSender::default(),
            frame_sink: None,
        }
    }

    /// Number of attached matrices, fixed at configuration time.
    pub fn matrices(&self) -> u32 {
        self.matrices
    }

    /// Attaches a push channel for composited per-matrix frames (design doc §10). Once set,
    /// every matrix swap -- whether from `CMD_SWAP` or auto-refresh -- composites that matrix's
    /// scanout buffer and pushes the result via [`mpsc::Sender::try_send`], the same
    /// never-blocks contract `CharDisplay::attach_frame_sink` upholds: if the consumer isn't
    /// keeping up, the frame is silently dropped rather than stalling CPU execution.
    pub fn attach_frame_sink(&mut self, sink: mpsc::Sender<LedMatrixFrame>) {
        self.frame_sink = Some(sink);
    }

    /// The color palette's current contents (spec §2, §2.1), reflecting any runtime
    /// `CMD_PALETTE_WRITE` updates.
    pub fn palette(&self) -> &[Rgb565] {
        &self.palette
    }

    /// Returns matrix `index`'s current scanout buffer (spec §5.1): the 1,024 palette-index bytes
    /// last copied from CPU-addressable pixel memory by a swap, used by a later work unit's
    /// compositing and by this unit's tests.
    pub fn frame_source(&self, index: u32) -> &[u8] {
        let start = index as usize * PIXELS_PER_MATRIX;
        &self.scanout[start..start + PIXELS_PER_MATRIX]
    }

    /// Installs a log sender for diagnostic messages (e.g. `reset()`).
    pub fn set_log_sender(&mut self, sender: LogSender) {
        self.log_sender = sender;
    }

    /// Marks matrix `offset / PIXELS_PER_MATRIX` dirty and stores `value` in CPU-addressable
    /// pixel memory (spec §4.1): every write marks the matrix dirty, regardless of whether the
    /// value differs from what was already there.
    fn write_pixel(&mut self, offset: usize, value: u8) {
        self.pixels[offset] = value;
        let matrix_index = offset / PIXELS_PER_MATRIX;
        self.dirty |= 1 << matrix_index;
    }

    /// Copies matrix `index`'s CPU-addressable buffer into its scanout buffer and clears its
    /// dirty flag (spec §5.1, §5.2), unconditionally -- callers decide whether dirty state gates
    /// the swap (`CMD_SWAP` does not; auto-refresh does).
    fn swap_matrix(&mut self, index: u32) {
        let start = index as usize * PIXELS_PER_MATRIX;
        let end = start + PIXELS_PER_MATRIX;
        self.scanout[start..end].copy_from_slice(&self.pixels[start..end]);
        self.dirty &= !(1 << index);
        if let Some(sink) = &self.frame_sink {
            let pixels = compositing::composite_matrix(&self.scanout[start..end], &self.palette);
            let _ = sink.try_send(LedMatrixFrame { matrix_index: index as u8, pixels });
        }
    }

    /// Writes the command register (spec §4.2): always replaces [`PendingOp`] wholesale,
    /// discarding whatever partial sequence was in progress, and arms the write-sequence state
    /// machine for the selected command. An unrecognized command code returns to idle.
    fn write_command(&mut self, value: u8) {
        let armed = |command, expected| PendingOp::Write { command, buffer: [0; 4], filled: 0, expected };
        self.pending = match value {
            CMD_SWAP => armed(Command::Swap, 1),
            CMD_SET_AUTOREFRESH => armed(Command::SetAutorefresh, 1),
            CMD_SET_POWER => armed(Command::SetPower, 1),
            CMD_SET_BRIGHTNESS => armed(Command::SetBrightness, 1),
            CMD_PALETTE_WRITE => armed(Command::PaletteWrite, 4),
            CMD_PALETTE_READ => armed(Command::PaletteRead, 1),
            _ => PendingOp::Idle,
        };
    }

    /// Writes the data register (spec §4.3): advances an armed write-sequence command by one
    /// byte, applying its effect once the full argument sequence has been collected. A no-op
    /// while idle or while a read-sequence command's result bytes are being popped (nothing armed
    /// to advance).
    fn write_data(&mut self, value: u8) {
        let PendingOp::Write { command, mut buffer, filled, expected } = self.pending else { return };
        buffer[filled] = value;
        let filled = filled + 1;
        if filled < expected {
            self.pending = PendingOp::Write { command, buffer, filled, expected };
            return;
        }
        self.pending = self.apply_command(command, buffer);
    }

    /// Applies a fully-collected write-sequence command's effect (design doc §5, §6; spec §4.2)
    /// and returns the [`PendingOp`] to transition to: idle for every command except
    /// `CMD_PALETTE_READ`, whose one argument byte (the index) instead arms the 3-byte read
    /// sequence that returns the addressed entry's channel bytes.
    fn apply_command(&mut self, command: Command, buffer: [u8; 4]) -> PendingOp {
        match command {
            Command::Swap => {
                let mask = buffer[0];
                for i in 0..self.matrices {
                    if mask & (1 << i) != 0 {
                        self.swap_matrix(i);
                    }
                }
            }
            Command::SetAutorefresh => self.autorefresh_mask = buffer[0],
            Command::SetPower => self.power_mask = buffer[0],
            Command::SetBrightness => self.brightness = buffer[0],
            Command::PaletteWrite => {
                let index = buffer[0] as usize;
                self.palette[index] = Rgb565::from_rgb888(buffer[1], buffer[2], buffer[3]);
            }
            Command::PaletteRead => {
                let index = buffer[0] as usize;
                let (r, g, b) = self.palette[index].to_rgb888();
                return PendingOp::Read { remaining: [r, g, b], next: 0 };
            }
        }
        PendingOp::Idle
    }

    /// Reads the data register (spec §4.3): pops the next byte of an armed read sequence, or
    /// returns `0` when idle, mid write-sequence, or past the sequence's byte count.
    fn read_data(&mut self) -> u8 {
        let PendingOp::Read { remaining, next } = self.pending else { return 0 };
        if next >= remaining.len() {
            return 0;
        }
        let value = remaining[next];
        let next = next + 1;
        self.pending = if next == remaining.len() { PendingOp::Idle } else { PendingOp::Read { remaining, next } };
        value
    }

    /// Side-effect-free equivalent of [`Self::read_data`] (spec §4.3): returns the next armed
    /// read-sequence byte without advancing the sequence.
    fn peek_data(&self) -> u8 {
        match self.pending {
            PendingOp::Read { remaining, next } if next < remaining.len() => remaining[next],
            _ => 0,
        }
    }

    /// The cycle-accounted auto-refresh cadence tick (design doc §8, spec §6): every matrix that
    /// is both in the auto-refresh mask and currently dirty is swapped and has its dirty flag
    /// cleared. Matrices not in the mask, or in the mask but not dirty, are left untouched.
    fn on_frame_tick(&mut self) {
        for i in 0..self.matrices {
            let bit = 1u8 << i;
            if self.autorefresh_mask & bit != 0 && self.dirty & bit != 0 {
                self.swap_matrix(i);
            }
        }
    }
}

impl IoDevice for LedMatrix {
    fn read(&mut self, address: u16) -> u8 {
        let offset = (address - self.address_range.start) as usize;
        if offset < self.pixel_bytes {
            self.pixels[offset]
        } else if offset == self.pixel_bytes {
            0
        } else if offset == self.pixel_bytes + 1 {
            self.read_data()
        } else {
            0
        }
    }

    fn write(&mut self, address: u16, value: u8) {
        let offset = (address - self.address_range.start) as usize;
        if offset < self.pixel_bytes {
            self.write_pixel(offset, value);
        } else if offset == self.pixel_bytes {
            self.write_command(value);
        } else if offset == self.pixel_bytes + 1 {
            self.write_data(value);
        }
    }

    fn peek(&self, address: u16) -> u8 {
        let offset = (address - self.address_range.start) as usize;
        if offset < self.pixel_bytes {
            self.pixels[offset]
        } else if offset == self.pixel_bytes {
            0
        } else if offset == self.pixel_bytes + 1 {
            self.peek_data()
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
            self.on_frame_tick();
        }
    }

    fn reset(&mut self) {
        let mask = matrix_mask(self.matrices);
        self.dirty = mask;
        self.autorefresh_mask = mask;
        self.power_mask = mask;
        self.pending = PendingOp::Idle;
        self.cycle_accumulator = 0;
        log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} reset", self.identity());
    }

    fn name(&self) -> &str {
        self.name
    }

    fn identity_address(&self) -> u16 {
        self.address_range.start
    }

    /// Drops the frame sink, closing the channel -- the debugger's bridge task's `recv()` loop
    /// mirrors `CharDisplay::shutdown` exactly (this device has no transport to shut down).
    fn shutdown(&mut self) {
        self.frame_sink = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE_NAME: &str = "led_matrix";
    const BASE_ADDRESS: u16 = 0xD000;

    fn test_palette() -> Vec<Rgb565> {
        (0..PALETTE_LEN)
            .map(|i| Rgb565::new((i % 32) as u8, (i % 64) as u8, ((i * 7) % 32) as u8))
            .collect()
    }

    fn address_range(matrices: u32) -> AddressRange {
        let size = matrices as usize * PIXELS_PER_MATRIX + 2;
        AddressRange::new(BASE_ADDRESS, BASE_ADDRESS + (size as u16 - 1))
    }

    fn device(matrices: u32) -> LedMatrix {
        LedMatrix::new(DEVICE_NAME, address_range(matrices), matrices, Some(1_000_000), 100, test_palette())
    }

    fn pixel_addr(offset: u16) -> u16 {
        BASE_ADDRESS + offset
    }

    fn command_addr(matrices: u32) -> u16 {
        BASE_ADDRESS + (matrices as usize * PIXELS_PER_MATRIX) as u16
    }

    fn data_addr(matrices: u32) -> u16 {
        command_addr(matrices) + 1
    }

    fn write_palette_write(device: &mut LedMatrix, matrices: u32, index: u8, r: u8, g: u8, b: u8) {
        device.write(command_addr(matrices), CMD_PALETTE_WRITE);
        device.write(data_addr(matrices), index);
        device.write(data_addr(matrices), r);
        device.write(data_addr(matrices), g);
        device.write(data_addr(matrices), b);
    }

    #[test]
    fn pixel_memory_read_write_round_trip() {
        let mut device = device(2);
        device.write(pixel_addr(0), 0x41);
        device.write(pixel_addr(PIXELS_PER_MATRIX as u16 - 1), 0x42);
        device.write(pixel_addr(2 * PIXELS_PER_MATRIX as u16 - 1), 0x43);
        assert_eq!(device.read(pixel_addr(0)), 0x41);
        assert_eq!(device.read(pixel_addr(PIXELS_PER_MATRIX as u16 - 1)), 0x42);
        assert_eq!(device.read(pixel_addr(2 * PIXELS_PER_MATRIX as u16 - 1)), 0x43);
        assert_eq!(device.peek(pixel_addr(0)), 0x41);
    }

    #[test]
    fn claims_only_within_configured_range() {
        let device = device(1);
        assert!(device.claims(BASE_ADDRESS));
        assert!(device.claims(data_addr(1)));
        assert!(!device.claims(BASE_ADDRESS - 1));
        assert!(!device.claims(data_addr(1) + 1));
    }

    #[test]
    fn identity_address_and_name() {
        let device = device(1);
        assert_eq!(device.identity_address(), BASE_ADDRESS);
        assert_eq!(device.name(), DEVICE_NAME);
    }

    #[test]
    fn command_register_always_reads_zero() {
        let mut device = device(1);
        device.write(command_addr(1), CMD_PALETTE_WRITE);
        assert_eq!(device.read(command_addr(1)), 0);
        assert_eq!(device.peek(command_addr(1)), 0);
    }

    #[test]
    fn writing_pixel_marks_only_that_matrix_dirty() {
        let mut device = device(2);
        // Clear the construction-time all-dirty default first.
        device.write(command_addr(2), CMD_SWAP);
        device.write(data_addr(2), 0b11);
        assert_eq!(device.dirty, 0);

        device.write(pixel_addr(0), 0x41); // matrix 0
        assert_eq!(device.dirty, 0b01);
    }

    #[test]
    fn writing_pixel_marks_dirty_even_when_value_unchanged() {
        let mut device = device(1);
        device.write(command_addr(1), CMD_SWAP);
        device.write(data_addr(1), 0b1);
        assert_eq!(device.dirty, 0);

        device.write(pixel_addr(0), 0); // already 0, still marks dirty
        assert_eq!(device.dirty, 0b1);
    }

    #[test]
    fn cmd_swap_copies_requested_matrices_unconditionally_and_clears_their_dirty_bits() {
        let mut device = device(2);
        device.write(pixel_addr(0), 0x41);
        device.write(pixel_addr(PIXELS_PER_MATRIX as u16), 0x55); // matrix 1
        assert_eq!(device.frame_source(0)[0], 0);
        assert_eq!(device.frame_source(1)[0], 0);

        device.write(command_addr(2), CMD_SWAP);
        device.write(data_addr(2), 0b11);

        assert_eq!(device.frame_source(0)[0], 0x41);
        assert_eq!(device.frame_source(1)[0], 0x55);
        assert_eq!(device.dirty, 0);
    }

    #[test]
    fn cmd_swap_only_affects_requested_matrices() {
        let mut device = device(2);
        device.write(pixel_addr(0), 0x41);
        device.write(pixel_addr(PIXELS_PER_MATRIX as u16), 0x55);

        device.write(command_addr(2), CMD_SWAP);
        device.write(data_addr(2), 0b01); // only matrix 0

        assert_eq!(device.frame_source(0)[0], 0x41);
        assert_eq!(device.frame_source(1)[0], 0, "matrix 1 must not have been swapped");
        assert_eq!(device.dirty, 0b10, "matrix 1's dirty bit must remain set");
    }

    #[test]
    fn cmd_swap_re_copies_even_when_matrix_is_already_clean() {
        let mut device = device(1);
        // Clear the construction-time dirty default first, without changing pixel content.
        device.write(command_addr(1), CMD_SWAP);
        device.write(data_addr(1), 0b1);
        assert_eq!(device.dirty, 0);

        // Requesting a swap of an already-clean matrix must still copy pixel memory into scanout
        // (spec §5.3: CMD_SWAP ignores dirty state entirely).
        device.write(pixel_addr(0), 0x99);
        device.write(command_addr(1), CMD_SWAP);
        device.write(data_addr(1), 0b1);
        assert_eq!(device.frame_source(0)[0], 0x99);
        assert_eq!(device.dirty, 0);
    }

    #[test]
    fn dirty_defaults_to_all_matrices_at_construction() {
        let device = device(4);
        assert_eq!(device.dirty, 0b1111);
    }

    #[test]
    fn autorefresh_mask_defaults_to_all_matrices_at_construction() {
        let device = device(4);
        assert_eq!(device.autorefresh_mask, 0b1111);
    }

    #[test]
    fn power_mask_defaults_to_all_matrices_at_construction() {
        let device = device(4);
        assert_eq!(device.power_mask, 0b1111);
    }

    #[test]
    fn eight_matrices_fills_masks_completely() {
        let device = device(8);
        assert_eq!(device.dirty, 0xFF);
        assert_eq!(device.autorefresh_mask, 0xFF);
        assert_eq!(device.power_mask, 0xFF);
    }

    #[test]
    fn cmd_set_autorefresh_replaces_mask_wholesale() {
        let mut device = device(4);
        device.write(command_addr(4), CMD_SET_AUTOREFRESH);
        device.write(data_addr(4), 0b0101);
        assert_eq!(device.autorefresh_mask, 0b0101);
    }

    #[test]
    fn cmd_set_power_replaces_mask_wholesale() {
        let mut device = device(4);
        device.write(command_addr(4), CMD_SET_POWER);
        device.write(data_addr(4), 0b0010);
        assert_eq!(device.power_mask, 0b0010);
    }

    #[test]
    fn cmd_set_brightness_sets_global_level() {
        let mut device = device(1);
        device.write(command_addr(1), CMD_SET_BRIGHTNESS);
        device.write(data_addr(1), 0x7F);
        assert_eq!(device.brightness, 0x7F);
    }

    #[test]
    fn auto_refresh_only_swaps_matrices_that_are_enabled_and_dirty() {
        let mut device = device(2);
        // Clear the construction-time dirty default first.
        device.write(command_addr(2), CMD_SWAP);
        device.write(data_addr(2), 0b11);

        // Disable auto-refresh for matrix 1.
        device.write(command_addr(2), CMD_SET_AUTOREFRESH);
        device.write(data_addr(2), 0b01);

        device.write(pixel_addr(0), 0x41); // matrix 0: enabled + dirty
        device.write(pixel_addr(PIXELS_PER_MATRIX as u16), 0x55); // matrix 1: dirty but disabled

        device.tick(10_000); // one cadence tick (cycles_per_frame = 1_000_000 / 100)

        assert_eq!(device.frame_source(0)[0], 0x41, "enabled + dirty matrix must be swapped");
        assert_eq!(device.frame_source(1)[0], 0, "disabled matrix must not be swapped despite being dirty");
        assert_eq!(device.dirty, 0b10, "matrix 1 must remain dirty");
    }

    #[test]
    fn auto_refresh_does_not_swap_a_clean_matrix() {
        let mut device = device(1);
        device.write(command_addr(1), CMD_SWAP);
        device.write(data_addr(1), 0b1); // clear the construction-time dirty default

        device.tick(10_000);
        assert_eq!(device.dirty, 0, "no write occurred since the swap, so nothing should be dirty");
        assert_eq!(device.frame_source(0)[0], 0);
    }

    #[test]
    fn auto_refresh_cadence_derived_from_clock_and_frame_rate() {
        let mut device = device(1);
        device.write(pixel_addr(0), 0x41);
        // cycles_per_frame = 1_000_000 / 100 = 10_000; one cycle short must not fire yet.
        device.tick(9_999);
        assert_eq!(device.frame_source(0)[0], 0);
        device.tick(1);
        assert_eq!(device.frame_source(0)[0], 0x41);
    }

    #[test]
    fn nominal_clock_used_when_clock_hz_unavailable() {
        let mut device = LedMatrix::new(DEVICE_NAME, address_range(1), 1, None, super::super::display::DEFAULT_FRAME_RATE_HZ, test_palette());
        let cycles_per_frame = super::super::display::NOMINAL_CLOCK_HZ / super::super::display::DEFAULT_FRAME_RATE_HZ as u64;
        device.write(pixel_addr(0), 0x41);
        device.tick(cycles_per_frame as u32 - 1);
        assert_eq!(device.frame_source(0)[0], 0);
        device.tick(1);
        assert_eq!(device.frame_source(0)[0], 0x41);
    }

    #[test]
    fn palette_write_masks_components_to_rgb565() {
        let mut device = device(1);
        write_palette_write(&mut device, 1, 2, 10, 20, 30);
        // spec §4.2.1: masked down to native bit width, then read back scaled -- not guaranteed
        // to equal the original bytes exactly.
        assert_eq!(device.palette()[2], Rgb565::from_rgb888(10, 20, 30));
    }

    #[test]
    fn palette_read_returns_scaled_channel_bytes_in_order() {
        let mut device = device(1);
        write_palette_write(&mut device, 1, 5, 0xFF, 0x00, 0xFF);

        device.write(command_addr(1), CMD_PALETTE_READ);
        device.write(data_addr(1), 5); // index

        assert_eq!(device.read(data_addr(1)), 0xFF); // red
        assert_eq!(device.read(data_addr(1)), 0x00); // green
        assert_eq!(device.read(data_addr(1)), 0xFF); // blue
        assert_eq!(device.read(data_addr(1)), 0, "past the 3-byte sequence, reads return 0");
    }

    #[test]
    fn palette_read_round_trip_may_change_the_original_byte() {
        let mut device = device(1);
        write_palette_write(&mut device, 1, 7, 0x0F, 0x0F, 0x0F);

        device.write(command_addr(1), CMD_PALETTE_READ);
        device.write(data_addr(1), 7);

        let red = device.read(data_addr(1));
        assert_ne!(red, 0x0F, "spec §4.2.1: round trip is not guaranteed exact");
    }

    #[test]
    fn palette_read_zero_and_max_round_trip_exactly() {
        let mut device = device(1);
        write_palette_write(&mut device, 1, 9, 0x00, 0x00, 0x00);
        device.write(command_addr(1), CMD_PALETTE_READ);
        device.write(data_addr(1), 9);
        assert_eq!((device.read(data_addr(1)), device.read(data_addr(1)), device.read(data_addr(1))), (0, 0, 0));

        write_palette_write(&mut device, 1, 9, 0xFF, 0xFF, 0xFF);
        device.write(command_addr(1), CMD_PALETTE_READ);
        device.write(data_addr(1), 9);
        assert_eq!((device.read(data_addr(1)), device.read(data_addr(1)), device.read(data_addr(1))), (0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn reissuing_command_mid_sequence_discards_partial_sequence() {
        let mut device = device(1);
        device.write(command_addr(1), CMD_PALETTE_WRITE);
        device.write(data_addr(1), 3); // index
        device.write(data_addr(1), 0xFF); // red (would-be first sequence, discarded)

        device.write(command_addr(1), CMD_PALETTE_WRITE); // re-issue resets to expect index
        device.write(data_addr(1), 3);
        device.write(data_addr(1), 11);
        device.write(data_addr(1), 22);
        device.write(data_addr(1), 33);

        assert_eq!(device.palette()[3], Rgb565::from_rgb888(11, 22, 33));
    }

    #[test]
    fn data_register_writes_ignored_when_no_command_armed() {
        let mut device = device(1);
        let original = device.palette().to_vec();
        device.write(data_addr(1), 10);
        device.write(data_addr(1), 20);
        assert_eq!(device.palette(), original.as_slice());
    }

    #[test]
    fn data_register_write_ignored_while_a_read_sequence_is_armed() {
        let mut device = device(1);
        write_palette_write(&mut device, 1, 1, 1, 2, 3);
        device.write(command_addr(1), CMD_PALETTE_READ);
        device.write(data_addr(1), 1);

        device.write(data_addr(1), 0xAA); // must not disturb the in-progress read sequence
        assert_eq!(device.read(data_addr(1)), device.palette()[1].to_rgb888().0);
    }

    #[test]
    fn peek_data_does_not_advance_the_read_sequence() {
        let mut device = device(1);
        write_palette_write(&mut device, 1, 1, 1, 2, 3);
        device.write(command_addr(1), CMD_PALETTE_READ);
        device.write(data_addr(1), 1);

        let expected = device.palette()[1].to_rgb888().0;
        assert_eq!(device.peek(data_addr(1)), expected);
        assert_eq!(device.peek(data_addr(1)), expected, "peek must not consume the byte");
        assert_eq!(device.read(data_addr(1)), expected, "read still returns the same first byte");
    }

    #[test]
    fn reset_restores_default_masks_and_clears_pending_sequence() {
        let mut device = device(4);
        device.write(command_addr(4), CMD_SWAP);
        device.write(data_addr(4), 0b1111);
        device.write(command_addr(4), CMD_SET_AUTOREFRESH);
        device.write(data_addr(4), 0b0001);
        device.write(command_addr(4), CMD_SET_POWER);
        device.write(data_addr(4), 0b0001);
        device.write(command_addr(4), CMD_PALETTE_WRITE);
        device.write(data_addr(4), 1); // mid-sequence

        device.reset();

        assert_eq!(device.dirty, 0b1111);
        assert_eq!(device.autorefresh_mask, 0b1111);
        assert_eq!(device.power_mask, 0b1111);
        // The mid-sequence palette write must have been discarded: completing it with fresh
        // bytes must not apply a color built from a mix of pre- and post-reset bytes.
        let original = device.palette()[1];
        device.write(data_addr(4), 1);
        device.write(data_addr(4), 2);
        device.write(data_addr(4), 3);
        assert_eq!(device.palette()[1], original, "data-only writes with nothing armed are ignored");
    }

    #[test]
    fn reset_preserves_pixel_memory() {
        let mut device = device(1);
        device.write(pixel_addr(0), 0x41);
        device.reset();
        assert_eq!(device.peek(pixel_addr(0)), 0x41);
    }

    #[test]
    fn reset_logs_device_message() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let mut device = device(1);
        device.set_log_sender(sender);
        device.reset();
        let received = rx.recv().unwrap();
        assert_eq!(received.category, LogCategory::Device);
        assert_eq!(received.message, format!("{DEVICE_NAME}@0x{BASE_ADDRESS:04x} reset"));
    }

    #[test]
    fn no_irq_capability() {
        let device = device(1);
        assert!(!device.irq_active());
    }

    #[tokio::test]
    async fn cmd_swap_pushes_a_composited_frame_for_each_requested_matrix() {
        let mut device = device(2);
        let (tx, mut rx) = mpsc::channel(4);
        device.attach_frame_sink(tx);
        device.write(pixel_addr(0), 1);
        device.write(pixel_addr(PIXELS_PER_MATRIX as u16), 1);

        device.write(command_addr(2), CMD_SWAP);
        device.write(data_addr(2), 0b11);

        let first = rx.try_recv().expect("expected a frame for matrix 0");
        assert_eq!(first.matrix_index, 0);
        assert_eq!(first.pixels.len(), PIXELS_PER_MATRIX * 4);
        let second = rx.try_recv().expect("expected a frame for matrix 1");
        assert_eq!(second.matrix_index, 1);
        assert!(rx.try_recv().is_err(), "no further frames expected");
    }

    #[tokio::test]
    async fn auto_refresh_pushes_a_composited_frame_for_the_swapped_matrix() {
        let mut device = device(1);
        let (tx, mut rx) = mpsc::channel(4);
        // Clear the construction-time dirty default first, without a sink attached, so the only
        // frame this test observes is the one auto-refresh pushes below.
        device.write(command_addr(1), CMD_SWAP);
        device.write(data_addr(1), 0b1);
        device.attach_frame_sink(tx);

        device.write(pixel_addr(0), 0x41);
        device.tick(10_000); // one cadence tick (cycles_per_frame = 1_000_000 / 100)

        let frame = rx.try_recv().expect("expected a frame pushed by auto-refresh");
        assert_eq!(frame.matrix_index, 0);
    }

    #[tokio::test]
    async fn shutdown_drops_the_frame_sink_closing_the_channel() {
        let mut device = device(1);
        let (tx, mut rx) = mpsc::channel(4);
        device.attach_frame_sink(tx);
        device.shutdown();
        device.write(command_addr(1), CMD_SWAP);
        device.write(data_addr(1), 0b1);
        assert!(rx.try_recv().is_err(), "channel should be closed once the sink is dropped");
    }
}
