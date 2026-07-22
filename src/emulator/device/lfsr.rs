//! 16-bit Galois LFSR pseudo-random number generator device.

use log::debug;

/// Default Galois tap mask: x¹⁶ + x¹⁴ + x¹³ + x¹¹ + 1 (maximal-length, 65535-state cycle).
const DEFAULT_TAPS: u16 = 0xB400;

/// Default initial LFSR state used at construction and after reset.
const DEFAULT_STATE: u16 = 0xACE1;

/// A memory-mapped 16-bit Galois LFSR pseudo-random number generator.
///
/// Occupies 2 bytes of address space:
///
/// | Offset | Read | Write |
/// |--------|------|-------|
/// | 0 (LOW) | Latch/advance state; return low byte | Buffer low seed byte |
/// | 1 (HIGH) | Return latched high byte | Load seed from `(seed_buf \| value << 8)` |
///
/// In **continuous** mode (default), the LFSR advances once per CPU clock cycle via
/// [`IoDevice::tick`]; reading only latches and returns the current state. In **step** mode,
/// the LFSR advances only when the LOW register is read.
///
/// Seeding: write LOW byte first (buffered), then write HIGH byte (loads the LFSR). A seed
/// of `0x0000` is clamped to `0x0001` to prevent a stuck state.
pub struct Lfsr16 {
    name: &'static str,
    address: u16,
    taps: u16,
    state: u16,
    seed_buf: u8,
    /// Latched 16-bit snapshot; HIGH reads return from here without advancing.
    latch: u16,
    /// When `true`, `tick()` drives advances; when `false`, LOW reads drive advances.
    continuous: bool,
}

impl Lfsr16 {
    /// Creates a new `Lfsr16` with default taps ([`DEFAULT_TAPS`]) and continuous advance mode.
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            address: 0,
            taps: DEFAULT_TAPS,
            state: DEFAULT_STATE,
            seed_buf: 0,
            latch: 0,
            continuous: true,
        }
    }

    /// Sets the base bus address for this device.
    pub fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }

    /// Sets the Galois tap mask. Use a maximal-length polynomial for a full-cycle sequence.
    pub fn with_taps(mut self, taps: u16) -> Self {
        self.taps = taps;
        self
    }

    /// Selects the advance mode: `true` for continuous (tick-driven), `false` for step (read-driven).
    pub fn with_continuous(mut self, continuous: bool) -> Self {
        self.continuous = continuous;
        self
    }

    /// Advances the LFSR by one step using the Galois feedback rule.
    fn advance(&mut self) {
        if self.state & 1 != 0 {
            self.state = (self.state >> 1) ^ self.taps;
        } else {
            self.state >>= 1;
        }
    }
}

impl super::IoDevice for Lfsr16 {
    fn read(&mut self, address: u16) -> u8 {
        match address - self.address {
            0 => {
                if !self.continuous {
                    self.advance();
                }
                self.latch = self.state;
                (self.latch & 0xFF) as u8
            }
            1 => (self.latch >> 8) as u8,
            _ => 0xFF,
        }
    }

    fn write(&mut self, address: u16, value: u8) {
        match address - self.address {
            0 => self.seed_buf = value,
            1 => {
                let seed = (self.seed_buf as u16) | ((value as u16) << 8);
                self.state = if seed == 0 { 1 } else { seed };
            }
            _ => {}
        }
    }

    fn peek(&self, address: u16) -> u8 {
        match address - self.address {
            0 => (self.latch & 0xFF) as u8,
            1 => (self.latch >> 8) as u8,
            _ => 0xFF,
        }
    }

    fn tick(&mut self, cycles: u32) {
        if self.continuous {
            for _ in 0..cycles {
                self.advance();
            }
        }
    }

    fn reset(&mut self) {
        self.state = DEFAULT_STATE;
        self.latch = 0;
        self.seed_buf = 0;
        debug!("{} @0x{:04x} reset", self.name(), self.address);
    }

    fn name(&self) -> &str {
        self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::device::IoDevice;

    const BASE: u16 = 0xD000;

    fn step_device() -> Lfsr16 {
        Lfsr16::new("lfsr").with_address(BASE).with_continuous(false)
    }

    fn continuous_device() -> Lfsr16 {
        Lfsr16::new("lfsr").with_address(BASE).with_continuous(true)
    }

    fn seed(dev: &mut Lfsr16, value: u16) {
        dev.write(BASE, (value & 0xFF) as u8);
        dev.write(BASE + 1, (value >> 8) as u8);
    }

    fn read16(dev: &mut Lfsr16) -> u16 {
        let lo = dev.read(BASE) as u16;
        let hi = dev.read(BASE + 1) as u16;
        lo | (hi << 8)
    }

    /// Galois LFSR step with taps 0xB400, seed 0x0001 produces:
    /// 0xB400, 0x5A00, 0x2D00, 0x1680, 0x0B40
    #[test]
    fn seed_test_known_sequence() {
        let mut dev = step_device();
        seed(&mut dev, 0x0001);
        assert_eq!(read16(&mut dev), 0xB400);
        assert_eq!(read16(&mut dev), 0x5A00);
        assert_eq!(read16(&mut dev), 0x2D00);
        assert_eq!(read16(&mut dev), 0x1680);
        assert_eq!(read16(&mut dev), 0x0B40);
    }

    /// In continuous mode tick() advances the LFSR; reads only latch, never advance.
    #[test]
    fn continuous_mode_tick_drives_advance() {
        let mut dev = continuous_device();
        seed(&mut dev, 0x0001);
        // 1 tick = 1 advance → state should be 0xB400
        dev.tick(1);
        assert_eq!(read16(&mut dev), 0xB400);
        // A second read should return the same latched value (no advance on read)
        assert_eq!(read16(&mut dev), 0xB400);
        // 2 more ticks → 0x5A00 → 0x2D00
        dev.tick(2);
        assert_eq!(read16(&mut dev), 0x2D00);
    }

    /// A zero seed is clamped to 0x0001 so the LFSR never gets stuck.
    #[test]
    fn stuck_zero_guard() {
        let mut dev = step_device();
        seed(&mut dev, 0x0000);
        // State must have been clamped to 0x0001; first advance → 0xB400
        assert_eq!(read16(&mut dev), 0xB400);
    }

    /// Multiple reads of the HIGH register return the same latched value until the
    /// next LOW read.
    #[test]
    fn high_byte_latch_stable() {
        let mut dev = step_device();
        seed(&mut dev, 0x0001);
        let lo = dev.read(BASE);        // advance → 0xB400, latch it
        let hi1 = dev.read(BASE + 1);  // return latched high
        let hi2 = dev.read(BASE + 1);  // still latched high — no advance
        let hi3 = dev.read(BASE + 1);
        assert_eq!(lo, 0x00);
        assert_eq!(hi1, 0xB4);
        assert_eq!(hi2, 0xB4);
        assert_eq!(hi3, 0xB4);
    }

    /// In step mode, only LOW reads advance; HIGH reads do not.
    #[test]
    fn step_mode_only_low_advances() {
        let mut dev = step_device();
        seed(&mut dev, 0x0001);
        // First LOW read: advance to 0xB400
        assert_eq!(dev.read(BASE), 0x00);
        // Several HIGH reads: no advance
        for _ in 0..5 {
            dev.read(BASE + 1);
        }
        // Next LOW read: advance to 0x5A00
        assert_eq!(dev.read(BASE), 0x00);
        assert_eq!(dev.read(BASE + 1), 0x5A);
    }
}
