//! A buffered console device with support for an interrupt-driven break key input.
//!
//! This device is similar to the typical single port console devices of early microcomputers,
//! in which a single memory-mapped port is read to receive ASCII characters from a keyboard device,
//! and is written to print ASCII characters to display. In this emulation, the keyboard and
//! display are replaced with an IPC transport (typically a pipe connected to the virtual terminal
//! device provided with the Emma65 debugger).
//!
//! This implementation incorporates an integral ring buffer that holds input characters until they
//! are read by the program running on the 6502. An additional latch register allows the 6502
//! program to perform a single-character lookahead and to drain the input buffer when desired. An
//! optional break key code (e.g. ASCII Ctrl+C) may be configured; when this break key code is
//! detected in the input the transport, the input buffer is drained, the break key code is latched
//! and the CPU's IRQ signal is asserted.
//!
//! Provides two 8-bit addressable registers:
//!
//! | Offset |      Name      |
//! |--------|----------------|
//! | 0      | Data Register  |
//! | 1      | Latch Register |
//!
//! ## Data Register
//! - Reading the data register returns either the contents of the latch register (if non-zero), or
//!   the next input byte from connected transport  (if available), or zero if no input is available.
//!   The latch register and the interrupt status are both reset by a read of the data register.
//! - Writing the data register sends a byte to the connected transport; has no effect if no
//!   transport is connected.
//!
//! ## Latch Register
//! - Reading the latch register fetches the next byte of input from the connected transport if
//!   no value is already latched and an input byte is available; i.e. the fetch occurs only if the
//!   latch register is zero at the time of the read. The returned value remains in the latch for
//!   a subsequent read of either the data register or latch register. Interrupt status is cleared
//!   by a read of the latch register.
//! - Writing the latch register overwrites the current contents of the latch and drains the input
//!   buffer. If the value written corresponds to the configured break key (if any), an interrupt
//!   is triggered just as it would if the configured break key code was received from the transport.
//!   Writing any other value resets the interrupt status.
//!
//! ## Break Key
//! The device can be configured with a break key code (one byte; e.g. ASCII Ctrl+C). When the
//! configured break key value is read from the transport, the Latch Register is set to the break
//! key value, the input buffer is drained, and the CPU's IRQ signal is asserted. Reading the
//! Data Register or Latch Register, or writing the Latch Register resets the interrupt condition.
//!

use super::ring::Ring;
use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use crate::emulator::transport;
use crate::emulator::transport::{Transport, TransportError};
use log::debug;

pub use super::ring::RING_CAPACITY;

/// A buffered console device with support for an interrupt-driven break key input.
pub struct Console {
    name: &'static str,
    address: u16,
    transport: Option<Box<dyn Transport>>,
    report_error: Box<dyn Fn(TransportError) + Send>,
    break_key: Option<u8>,
    ring: Ring<u8>,
    latch: u8,
    interrupt_flag: bool,
}

impl Console {

    /// Creates a new `BufferedConsole` with no transport attached.
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            address: 0,
            transport: None,
            report_error: transport::no_op_reporter(),
            break_key: None,
            ring: Ring::new(0u8),
            latch: 0,
            interrupt_flag: false,
        }
    }

    /// Sets the address at which this device is registered on the bus.
    pub fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }

    /// Attaches a transport for byte-stream IO.
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>) {
        self.transport = Some(transport);
    }

    /// Sets the error sender for async transport event reporting.
    pub fn set_error_sender(&mut self, sender: ErrorSender, id: DeviceId) {
        self.report_error = transport::reporter(sender, id);
    }

    /// Sets the break key to recognize when reading from the transport
    pub fn set_break_key(&mut self, break_key: u8) {
        self.break_key = Some(break_key);
    }

}

impl IoDevice for Console {

    fn read(&mut self, address: u16) -> u8 {
        match address - self.address {
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

    fn write(&mut self, address: u16, value: u8) {
        match address - self.address {
            0 => {          // data register
                // send value to transport if we have one, otherwise write is a no-op
                if let Some(transport) = self.transport.as_mut()
                    && let Err(e) = transport.send(value) {
                    (self.report_error)(e);
                }
            },
            1 => {          // latch register
                self.latch = value;
                self.ring.clear();
                if let Some(break_key) = self.break_key {
                    self.interrupt_flag = break_key == value;
                } else {
                    self.interrupt_flag = false;
                }
            },
            _ => (),
        }
    }

    fn peek(&self, address: u16) -> u8 {
        match address - self.address {
            0 => if self.latch != 0 {
                self.latch
            } else {
                self.ring.peek().unwrap_or(0)
            }
            1 => self.latch,
            _ => 0,
        }
    }

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

    fn reset(&mut self) {
        self.ring.clear();
        self.latch = 0;
        self.interrupt_flag = false;
        debug!("{} @{} reset", self.name(), self.address)
    }

    fn irq_active(&self) -> bool { self.interrupt_flag }

    fn name(&self) -> &str { self.name }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::InternalPipeTransport;
    use std::time::Duration;

    const DEVICE_NAME: &str = "console";

    fn device() -> Console {
        Console::new(DEVICE_NAME)
    }

    fn device_with_pipe() -> (Console, InternalPipeTransport) {
        let (local, remote) = InternalPipeTransport::pair().unwrap();
        let mut device = device();
        device.attach_transport(Box::new(local));
        (device, remote)
    }

    #[test]
    fn read_data_register_resets_interrupt_flag() {
        let mut device = device();
        device.interrupt_flag = true;
        device.read(0);
        assert!(!device.interrupt_flag, "expected interrupt flag reset")
    }

    #[test]
    fn read_data_register_zero_when_nothing_latched_or_buffered() {
        let mut device = device();
        assert_eq!(device.read(0), 0);
    }

    #[test]
    fn read_data_register_latched_value() {
        let mut device = device();
        device.latch = 0x42;
        assert_eq!(device.read(0), 0x42);
    }

    #[test]
    fn read_data_register_buffered_value() {
        let mut device = device();
        device.latch = 0;
        device.ring.put(0x42);
        assert_eq!(device.read(0), 0x42);
        assert_eq!(device.latch, 0);
    }

    #[test]
    fn read_latch_register_resets_interrupt_flag() {
        let mut device = device();
        device.interrupt_flag = true;
        device.read(1);
        assert!(!device.interrupt_flag, "expected interrupt flag reset")
    }

    #[test]
    fn read_latch_register_latched_value() {
        let mut device = device();
        device.latch = 0x42;
        device.ring.put(0x43);
        assert_eq!(device.read(1), 0x42);
        assert_eq!(device.latch, 0x42);
        assert!(!device.ring.is_empty());
    }

    #[test]
    fn read_latch_register_latches_buffered_value() {
        let mut device = device();
        device.latch = 0;
        device.ring.put(0x42);
        assert_eq!(device.read(1), 0x42);
        assert_eq!(device.latch, 0x42);
    }

    #[test]
    fn read_latch_register_zero_when_nothing_latched_or_buffered() {
        let mut device = device();
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
        let mut device = device();
        assert_eq!(device.latch, 0);
        device.write(1, 0x42);
        assert_eq!(device.latch, 0x42);
        device.write(1, 0);
        assert_eq!(device.latch, 0);
    }

    #[test]
    fn write_latch_register_clears_ring() {
        let mut device = device();
        device.ring.put(0x42);
        device.write(1, 0);
        assert_eq!(device.latch, 0);
        assert!(device.ring.is_empty(), "expected empty ring");
    }

    #[test]
    fn write_break_key_to_latch_register_sets_interrupt_flag() {
        let mut device = device();
        device.set_break_key(0x3);
        assert_eq!(device.latch, 0);
        device.write(1, 0x3);
        assert_eq!(device.latch, 0x3);
        assert!(device.interrupt_flag, "expected interrupt flag set");
    }

    #[test]
    fn write_latch_register_clears_interrupt_flag() {
        let mut device = device();
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
        for i in 0..(RING_CAPACITY - 1) {
            assert_eq!(device.ring.get(), Some(i as u8));
        }
        assert!(device.ring.is_empty(), "expected empty ring");
    }

    #[test]
    fn integration_cpu_program_writes_appear_on_transport() {
        use crate::emulator::{
            AddressRange, BusConfig, CpuVariant, DeviceId, InternalPipeTransport,
        };
        use crate::emulator::exec::StepResult;

        let (local, mut remote) = InternalPipeTransport::pair().unwrap();
        let mut console = device().with_address(0xF000);
        console.attach_transport(Box::new(local));

        // Map all of RAM (including reset vector region) plus console at 0xF000.
        // Using RAM for 0xFF00–0xFFFF lets us write the reset vector after build().
        let bus = BusConfig::new()
            .ram_with_fill(AddressRange::new(0x0000, 0xEFFF), 0).unwrap()
            .device(AddressRange::new(0xF000, 0xF001), DeviceId(1), Box::new(console)).unwrap()
            .ram_with_fill(AddressRange::new(0xFF00, 0xFFFF), 0).unwrap()
            .build();

        let mut cpu = crate::emulator::Cpu::builder(CpuVariant::Wdc65C02)
            .bus(bus)
            .build()
            .unwrap();

        // Write program into RAM at 0x0200:
        //   LDA #$41   ; A9 41
        //   STA $F000  ; 8D 00 F0  -- write 'A' to console output
        //   LDA #$42   ; A9 42
        //   STA $F000  ; 8D 00 F0  -- write 'B'
        //   STP        ; DB
        let prog: &[u8] = &[
            0xA9, 0x41,
            0x8D, 0x00, 0xF0,
            0xA9, 0x42,
            0x8D, 0x00, 0xF0,
            0xDB,
        ];
        for (i, &b) in prog.iter().enumerate() {
            let _ = cpu.bus_mut().write(0x0200 + i as u16, b);
        }
        // Reset vector → 0x0200.
        let _ = cpu.bus_mut().write(0xFFFC, 0x00);
        let _ = cpu.bus_mut().write(0xFFFD, 0x02);

        let _ = cpu.reset();
        loop {
            match cpu.step(None) {
                StepResult::Stopped => break,
                StepResult::Error(e) => panic!("CPU error: {:?}", e),
                _ => {}
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x41));
        assert_eq!(remote.try_recv(), Some(0x42));
    }

    #[test]
    fn integration_transport_input_readable_by_cpu() {
        use crate::emulator::{
            AddressRange, BusConfig, CpuVariant, DeviceId, InternalPipeTransport,
        };
        use crate::emulator::exec::StepResult;

        let (local, mut remote) = InternalPipeTransport::pair().unwrap();
        let mut console = device().with_address(0xF000);
        console.attach_transport(Box::new(local));

        let bus = BusConfig::new()
            .ram_with_fill(AddressRange::new(0x0000, 0xEFFF), 0).unwrap()
            .device(AddressRange::new(0xF000, 0xF001), DeviceId(1), Box::new(console)).unwrap()
            .ram_with_fill(AddressRange::new(0xFF00, 0xFFFF), 0).unwrap()
            .build();

        let mut cpu = crate::emulator::Cpu::builder(CpuVariant::Wdc65C02)
            .bus(bus)
            .build()
            .unwrap();

        // Program at 0x0200:
        //   NOP        ; EA        -- tick the bus at least once
        //   LDA $F001  ; AD 01 F0  -- latch a byte from transport (latch reg)
        //   STA $0300  ; 8D 00 03  -- store it in RAM
        //   STP        ; DB
        let prog: &[u8] = &[
            0xEA,
            0xAD, 0x01, 0xF0,
            0x8D, 0x00, 0x03,
            0xDB,
        ];
        for (i, &b) in prog.iter().enumerate() {
            let _ = cpu.bus_mut().write(0x0200 + i as u16, b);
        }
        let _ = cpu.bus_mut().write(0xFFFC, 0x00);
        let _ = cpu.bus_mut().write(0xFFFD, 0x02);

        let _ = cpu.reset();

        // Send a byte from the remote end before the CPU starts.
        remote.send(0x5A).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        loop {
            match cpu.step(None) {
                StepResult::Stopped => break,
                StepResult::Error(e) => panic!("CPU error: {:?}", e),
                _ => {}
            }
        }

        assert_eq!(cpu.bus_mut().read(0x0300).unwrap(), 0x5A);
    }

    #[test]
    fn reset_clears_latch() {
        let mut console = device();
        console.latch = 0xff;
        console.reset();
        assert_eq!(console.latch, 0, "reset must clear the latch");
    }

}