use log::debug;
use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use crate::emulator::transport::{Transport, TransportError};

use super::ring::Ring;

pub use super::ring::RING_CAPACITY;

pub const CONSOLE_NAME: &str = "console";

/// A buffered console device with support for an interrupt-driven break key input.
///
/// Provides two 8-bit addressable registers:
///
/// | Offset |      Name      |
/// |--------|----------------|
/// | 0      | Data Register  |
/// | 1      | Latch Register |
///
/// ## Data Register
/// - Reading the data register returns either the contents of the latch register (if non-zero), or
///   the next input byte from connected transport  (if available), or zero if no input is available.
///   The latch register and the interrupt status are both reset by a read of the data register.
/// - Writing the data register sends a byte to the connected transport; has no effect if no
///   transport is connected.
///
/// ## Latch Register
/// - Reading the latch register fetches the next byte of input from the connected transport if
///   no value is already latched and an input byte is available; i.e. the fetch occurs only if the
///   latch register is zero at the time of the read. The returned value remains in the latch for
///   a subsequent read of either the data register or latch register. Interrupt status is cleared
///   by a read of the latch register.
/// - Writing the latch register overwrites the current contents of the latch and drains the input
///   buffer. If the value written corresponds to the configured break key (if any), an interrupt
///   is triggered just as it would if the configured break key code was received from the transport.
///   Writing any other value resets the interrupt status.
///
/// ## Break Key
/// The device can be configured with a break key code (one byte; e.g. ASCII Ctrl+C). When the
/// configured break key value is read from the transport, the Latch Register is set to the break
/// key value, the input buffer is drained, and the CPU's IRQ signal is asserted.
///
pub struct Console {
    /// Optional transport for byte-stream IO.
    transport: Option<Box<dyn Transport>>,
    /// Destination for async transport error events.
    error_sender: Option<ErrorSender>,
    /// Identity used in error events.
    device_id: Option<DeviceId>,
    /// ASCII character code for the optional break key code (e.g. 0x3 for Ctrl+C)
    break_key: Option<u8>,
    /// Input ring buffer
    ring: Ring<u8>,
    /// Current value of the device's latch register
    latch: u8,
    /// Flag that when true indicates the break key was received from the transport
    interrupt_flag: bool,
}

impl Console {

    /// Creates a new `BufferedConsole` with no transport attached.
    pub fn new() -> Self {
        Self {
            transport: None,
            error_sender: None,
            device_id: None,
            break_key: None,
            ring: Ring::new(0u8),
            latch: 0,
            interrupt_flag: false,
        }
    }

    /// Attaches a transport for byte-stream IO.
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>) {
        self.transport = Some(transport);
    }

    /// Sets the error sender for async transport event reporting.
    pub fn set_error_sender(&mut self, sender: ErrorSender, id: DeviceId) {
        self.error_sender = Some(sender);
        self.device_id = Some(id);
    }

    /// Sets the break key to recognize when reading from the transport
    pub fn set_break_key(&mut self, break_key: u8) {
        self.break_key = Some(break_key);
    }

    fn report_error(&self, error: TransportError) {
        if let (Some(sender), Some(id)) = (&self.error_sender, self.device_id) {
            use crate::emulator::device::DeviceEvent;
            let _ = sender.send(DeviceEvent::TransportError { device: id, error });
        }
    }

}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}


impl IoDevice for Console {

    /// Read device register at `offset`.
    fn read(&mut self, offset: u16) -> u8 {
        match offset {
            0 => {          // data register
                self.interrupt_flag = false;
                if self.latch != 0 {
                    let b = self.latch;
                    self.latch = 0;
                    b
                } else {
                    self.ring.get().unwrap_or(0)
                }
            },
            1 => {          // latch register
                self.interrupt_flag = false;
                if self.latch == 0 {
                    // if nothing latch, latch next input byte if any
                    self.latch = self.ring.get().unwrap_or(0);
                }
                self.latch
            },
            _ => 0,
        }
    }

    /// Writes `value` to device register at `offset`.
    fn write(&mut self, offset: u16, value: u8) {
        match offset {
            0 => {          // data register
                // send value to transport if we have one, otherwise write is a no-op
                if let Some(transport) = self.transport.as_mut()
                    && let Err(e) = transport.send(value) {
                    self.report_error(e);
                }
            },
            1 => {          // latch register
                self.latch = value;
                self.ring.clear();
                if self.break_key.is_none() {
                    self.interrupt_flag = false;
                } else {
                    let break_key = self.break_key.unwrap();
                    self.interrupt_flag = break_key == value;
                }
            },
            _ => (),
        }
    }

    /// Reads device register at `offset` without side effects.
    fn peek(&self, offset: u16) -> u8 {
        match offset {
            0 => if self.latch != 0 {
                self.latch
            } else {
                self.ring.peek().unwrap_or(0)
            }
            1 => self.latch,
            _ => 0,
        }
    }

    /// Polls a connected transport, if any.
    fn tick(&mut self, _cycles: u32) {
        if let Some(b) = self.transport.as_mut().and_then(|t| t.try_recv()) {
            if let Some(break_key) = self.break_key && b == break_key {
                self.latch = b;
                self.ring.clear();
                self.interrupt_flag = true;
            } else {
                self.ring.put(b);
            }
        }
    }

    /// Resets the console device by draining the input buffer, clearing the latch, and clearing any pending interrupt.
    fn reset(&mut self) {
        self.ring.clear();
        self.latch = 0;
        self.interrupt_flag = false;
        debug!("{} {} reset", self.name(), self.device_id.unwrap())
    }

    /// Tests whether the device has a pending interrupt.
    fn irq_active(&self) -> bool { self.interrupt_flag }

    /// Gets the name of the device.
    fn name(&self) -> &str { CONSOLE_NAME }

}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use crate::emulator::PipeTransport;
    use super::*;

    fn device_with_pipe() -> (Console, PipeTransport) {
        let (local, remote) = PipeTransport::pair().unwrap();
        let mut device = Console::new();
        device.attach_transport(Box::new(local));
        (device, remote)
    }

    #[test]
    fn read_data_register_resets_interrupt_flag() {
        let mut device = Console::new();
        device.interrupt_flag = true;
        device.read(0);
        assert!(!device.interrupt_flag, "expected interrupt flag reset")
    }

    #[test]
    fn read_data_register_zero_when_nothing_latched_or_buffered() {
        let mut device = Console::new();
        assert_eq!(device.read(0), 0);
    }

    #[test]
    fn read_data_register_latched_value() {
        let mut device = Console::new();
        device.latch = 0x42;
        assert_eq!(device.read(0), 0x42);
    }

    #[test]
    fn read_data_register_buffered_value() {
        let mut device = Console::new();
        device.latch = 0;
        device.ring.put(0x42);
        assert_eq!(device.read(0), 0x42);
        assert_eq!(device.latch, 0);
    }

    #[test]
    fn read_latch_register_resets_interrupt_flag() {
        let mut device = Console::new();
        device.interrupt_flag = true;
        device.read(1);
        assert!(!device.interrupt_flag, "expected interrupt flag reset")
    }

    #[test]
    fn read_latch_register_latched_value() {
        let mut device = Console::new();
        device.latch = 0x42;
        device.ring.put(0x43);
        assert_eq!(device.read(1), 0x42);
        assert_eq!(device.latch, 0x42);
        assert!(!device.ring.is_empty());
    }

    #[test]
    fn read_latch_register_latches_buffered_value() {
        let mut device = Console::new();
        device.latch = 0;
        device.ring.put(0x42);
        assert_eq!(device.read(1), 0x42);
        assert_eq!(device.latch, 0x42);
    }

    #[test]
    fn read_latch_register_zero_when_nothing_latched_or_buffered() {
        let mut device = Console::new();
        assert_eq!(device.read(1), 0);
    }

    #[test]
    fn write_data_register_sends_byte_to_transport() {
        let (mut device, mut transport) = device_with_pipe();
        device.write(0, 0x42);
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(transport.try_recv(), Some(0x42));
    }

    #[test]
    fn write_latch_register_sets_latch() {
        let mut device = Console::new();
        assert_eq!(device.latch, 0);
        device.write(1, 0x42);
        assert_eq!(device.latch, 0x42);
        device.write(1, 0);
        assert_eq!(device.latch, 0);
    }

    #[test]
    fn write_latch_register_clears_ring() {
        let mut device = Console::new();
        device.ring.put(0x42);
        device.write(1, 0);
        assert_eq!(device.latch, 0);
        assert!(device.ring.is_empty(), "expected empty ring");
    }

    #[test]
    fn write_break_key_to_latch_register_sets_interrupt_flag() {
        let mut device = Console::new();
        device.set_break_key(0x3);
        assert_eq!(device.latch, 0);
        device.write(1, 0x3);
        assert_eq!(device.latch, 0x3);
        assert!(device.interrupt_flag, "expected interrupt flag set");
    }

    #[test]
    fn write_latch_register_clears_interrupt_flag() {
        let mut device = Console::new();
        device.interrupt_flag = true;
        device.write(1, 0x42);
        assert_eq!(device.latch, 0x42);
        assert!(!device.interrupt_flag, "expected interrupt flag reset");
    }

    #[test]
    fn tick_buffers_input_from_transport() {
        let (mut device, mut transport) = device_with_pipe();
        transport.send(0x42).unwrap();
        device.tick(1);
        assert_eq!(device.ring.peek(), Some(0x42));
    }

    #[test]
    fn tick_latches_break_key_and_sets_interrupt_flag() {
        let (mut device, mut transport) = device_with_pipe();
        device.set_break_key(0x3);
        transport.send(0x3).unwrap();
        device.tick(1);
        assert_eq!(device.latch, 0x3);
        assert!(device.interrupt_flag, "expected interrupt flag set");
    }

    #[test]
    fn tick_clears_ring_on_break_key() {
        let (mut device, mut transport) = device_with_pipe();
        device.set_break_key(0x3);
        transport.send(0x3).unwrap();
        device.ring.put(0x42);
        device.ring.put(0x43);
        device.tick(1);
        assert!(device.ring.is_empty(), "expected empty ring");
    }

    #[test]
    fn tick_tail_drop_when_ring_full() {
        let (mut device, mut transport) = device_with_pipe();
        // send as many bytes as ring's capacity (one greater than what can be held)
        for i in 0..RING_CAPACITY {
            transport.send(i as u8).unwrap();
        }
        // attempt to buffer at ring's capacity (one greater than what can be held)
        for _ in 0..RING_CAPACITY {
            device.tick(1);
        }
        for i in 0..(RING_CAPACITY - 1){
            assert_eq!(device.ring.get(), Some(i as u8));
        }
        assert!(device.ring.is_empty(), "expected empty ring");
    }
}