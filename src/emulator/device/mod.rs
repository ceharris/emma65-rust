/// Uniquely identifies a device registered on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(pub u32);

/// A device that can be mapped into the bus address space.
pub trait IoDevice {
    /// Reads a byte from `offset` relative to the device's base address, with side effects.
    fn read(&mut self, offset: u16) -> u8;
    /// Writes `value` to `offset` relative to the device's base address.
    fn write(&mut self, offset: u16, value: u8);
    /// Reads a byte from `offset` relative to the device's base address, without side effects.
    fn peek(&self, offset: u16) -> u8;
    /// Advances device state by `cycles` clock cycles. Called after each CPU instruction.
    fn tick(&mut self, _cycles: u8) {}
}