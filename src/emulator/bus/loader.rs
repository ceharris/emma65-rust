//! Loader support for the [`Bus`].
//!
//! This module provides a `BusLoadTarget` for use with the [`loader`] module.
//! This allows load operations to target the memory devices attached to a [`Bus`].
//!
use super::Bus;

use crate::emulator::config::loader::{LoadError, LoadTarget};

/// A [`LoadTarget`] that writes using the [`Bus::patch`] method.
pub struct BusLoadTarget<'a> {
    bus: &'a mut Bus,
    bias: usize,
}

impl<'a> BusLoadTarget<'a> {
    /// Constructs a new instance that will target the specified bus.
    /// The `bias` specifies a fixed offset to be applied to write operations.
    pub fn new(bus: &'a mut Bus, bias: usize) -> Self {
        Self { bus, bias }
    }
}

impl<'a> LoadTarget for BusLoadTarget<'a> {

    /// Returns an error result if `data_len` exceeds the size of the bus address space
    /// (65536 bytes).
    fn check_fit(&self, data_len: usize) -> Result<(), LoadError> {
        if data_len + self.bias <= 0x10000 {
            Ok(())
        } else {
            Err(LoadError::SizeMismatch { actual: data_len, expected: 0x10000 - self.bias })
        }
    }

    /// Writes `data` to the address that corresponds to the given `offset` plus the configured
    /// bias. Returns an error only if `offset + self.bias` is larger than the bus address space.
    ///
    /// The write operation is performed using the [`Bus::patch`] method, which bypasses read-only
    /// restrictions associated with ROM devices.
    ///
    fn write(&mut self, offset: usize, data: u8) -> Result<(), LoadError> {
        let effective_offset = self.bias + offset;
        if effective_offset <= 0xFFFF {
            self.bus.patch(effective_offset as u16, data);
            Ok(())
        } else {
            Err(LoadError::OutOfBounds { address: effective_offset, size: 1 })
        }
    }

    /// Writes the slice `data` to the address range that corresponds to the given `offset` plus the
    /// configured bias. Returns an error only if `offset + data.len() + self.bias` is larger than
    /// the bus address space.
    ///
    /// The write operation is performed using the [`Bus::patch`] method, which bypasses read-only
    /// restrictions associated with ROM devices.
    ///
    fn write_slice(&mut self, offset: usize, data: &[u8]) -> Result<(), LoadError> {
        for (i, value) in data.iter().enumerate() {
            self.write(offset + i, *value)?;
        }
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::AddressRange;

    fn ram_bus() -> Bus {
        Bus::config().ram_with_fill(AddressRange::new(0, 0xFFFF), 0).unwrap().build()
    }

    fn load_target(bus: &mut Bus, bias: usize) -> BusLoadTarget<'_> {
        BusLoadTarget::new(bus, bias)
    }

    #[test]
    fn check_fit_full_range() {
        let mut bus = ram_bus();
        let target = load_target(&mut bus, 0);
        target.check_fit(0).unwrap();
        target.check_fit(0x10000).unwrap();
        target.check_fit(0x10001).unwrap_err();
    }

    #[test]
    fn check_fit_biased_range() {
        let mut bus = ram_bus();
        let target = load_target(&mut bus, 0x8000);
        target.check_fit(0).unwrap();
        target.check_fit(0x8000).unwrap();
        target.check_fit(0x8001).unwrap_err();
    }

    #[test]
    fn write_full_range() {
        let mut bus = ram_bus();
        let mut target = load_target(&mut bus, 0);
        for i in 0..0x10000 {
            target.write(i, 0xFF).unwrap();
        }
    }

    #[test]
    fn write_full_range_exceeded() {
        let mut bus = ram_bus();
        let mut target = load_target(&mut bus, 0);
        target.write(0x10000, 0xFF).unwrap_err();
    }

    #[test]
    fn write_biased_range() {
        let mut bus = ram_bus();
        let mut target = load_target(&mut bus, 0x8000);
        for i in 0..0x8000 {
            target.write(i, 0xFF).unwrap();
        }
    }

    #[test]
    fn write_biased_range_exceeded() {
        let mut bus = ram_bus();
        let mut target = load_target(&mut bus, 0x8000);
        target.write(0x8000, 0xFF).unwrap_err();
    }

    #[test]
    fn write_slice_full_range() {
        let mem: Vec<u8> = vec![0xFF; 0x10000];
        let mut bus = ram_bus();
        let mut target = load_target(&mut bus, 0);
        target.write_slice(0, &mem).unwrap();
    }

    #[test]
    fn write_slice_full_range_exceeded() {
        let mem: Vec<u8> = vec![0xFF; 0x10001];
        let mut bus = ram_bus();
        let mut target = load_target(&mut bus, 0);
        target.write_slice(0, &mem).unwrap_err();
    }

    #[test]
    fn write_slice_biased_range() {
        let mem: Vec<u8> = vec![0xFF; 0x8000];
        let mut bus = ram_bus();
        let mut target = load_target(&mut bus, 0x8000);
        target.write_slice(0, &mem).unwrap();
    }

    #[test]
    fn write_slice_biased_range_exceeded() {
        let mem: Vec<u8> = vec![0xFF; 0x8001];
        let mut bus = ram_bus();
        let mut target = load_target(&mut bus, 0x8000);
        target.write_slice(0, &mem).unwrap_err();
    }

}