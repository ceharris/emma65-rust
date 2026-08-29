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

use super::input_buffer::InputBuffer;
use crate::emulator::device::IoDevice;
use crate::emulator::transport::{Transport, TransportRelay};
use crate::emulator::{LogCategory, LogLevel, LogSender, log_msg};

/// A buffered console device with support for an interrupt-driven break key input.
pub struct Console {
    name: &'static str,
    address: u16,
    transport: Option<Box<dyn Transport>>,
    /// Paired with `transport`, drained once per `tick()`. `None` exactly
    /// when `transport` is `None` — every path that attaches a transport
    /// (a configured `TransportSpec` or an injected `TransportSlot`) also
    /// supplies its relay.
    relay: Option<TransportRelay>,
    input: InputBuffer,
    /// Sender for structured diagnostic messages (e.g. `reset()`).
    log_sender: LogSender,
}

impl Console {

    /// Creates a new `BufferedConsole` with no transport attached.
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            address: 0,
            transport: None,
            relay: None,
            input: InputBuffer::new(),
            log_sender: LogSender::default(),
        }
    }

    /// Sets the address at which this device is registered on the bus.
    pub fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }

    /// Attaches a transport for byte-stream IO, along with its paired relay.
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>, relay: TransportRelay) {
        self.transport = Some(transport);
        self.relay = Some(relay);
    }

    /// Sets the break key to recognize when reading from the transport
    pub fn set_break_key(&mut self, break_key: u8) {
        self.input.set_break_key(break_key);
    }

    /// Installs a log sender for diagnostic messages (e.g. `reset()`).
    pub fn set_log_sender(&mut self, sender: LogSender) {
        self.log_sender = sender;
    }

}

impl IoDevice for Console {

    fn read(&mut self, address: u16) -> u8 {
        match address - self.address {
            0 => self.input.read_data(),
            1 => self.input.read_latch(),
            _ => 0,
        }
    }

    fn write(&mut self, address: u16, value: u8) {
        match address - self.address {
            0 => {          // data register
                // send value to transport if we have one, otherwise write is a no-op
                if let Some(transport) = self.transport.as_mut() {
                    transport.send(value);
                }
            },
            1 => self.input.write_latch(value),
            _ => (),
        }
    }

    fn peek(&self, address: u16) -> u8 {
        match address - self.address {
            0 => self.input.peek_data(),
            1 => self.input.peek_latch(),
            _ => 0,
        }
    }

    fn tick(&mut self, _cycles: u32) {
        let input = &mut self.input;
        if let Some(relay) = self.relay.as_mut() {
            relay.drain_bytes_into(|b| input.push(b));
        }
    }

    fn reset(&mut self) {
        self.input.reset();
        log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} reset", self.identity());
    }

    fn irq_active(&self) -> bool { self.input.irq_active() }

    fn name(&self) -> &str { self.name }

    fn identity_address(&self) -> u16 { self.address }

    fn shutdown(&mut self) {
        if let Some(transport) = self.transport.as_mut() {
            transport.shutdown();
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::InternalPipeTransport;
    use crate::emulator::transport::ChannelRelay;
    use crossbeam_channel::{Sender, unbounded};
    use std::time::Duration;

    const DEVICE_NAME: &str = "console";

    fn device() -> Console {
        Console::new(DEVICE_NAME)
    }

    /// A `ChannelRelay<u8>` fed by a plain, unbounded `crossbeam_channel` (so
    /// the test's own `tx.send()` calls never block regardless of how fast
    /// the relay thread drains into its `capacity`-sized ring), for
    /// deterministic control over exactly what a device's `tick()` observes
    /// as "arrived" — independent of `InternalPipeTransport`'s own OS-pipe
    /// timing. `pair_direct()` gives both ends of the test pipe no relay of
    /// their own, so this hand-fed one stands in for it.
    fn spawn_byte_relay(capacity: usize) -> (Sender<u8>, ChannelRelay<u8>) {
        let (tx, rx) = unbounded();
        (tx, ChannelRelay::spawn(rx, capacity))
    }

    /// `remote` is the device's outbound-write sink (verified via
    /// `remote.try_recv()`); `tx` feeds the device's inbound relay directly,
    /// simulating bytes arriving from an external peer. `relay_capacity`
    /// should comfortably exceed however many bytes a test sends before its
    /// first `tick()`, so the relay thread has fully drained (not parked)
    /// by the time `tick()` runs.
    fn device_with_pipe(relay_capacity: usize) -> (Console, InternalPipeTransport, Sender<u8>) {
        let (local, remote) = InternalPipeTransport::pair_direct().unwrap();
        let (tx, relay) = spawn_byte_relay(relay_capacity);
        let mut device = device();
        device.attach_transport(Box::new(local), TransportRelay::Byte(relay));
        (device, remote, tx)
    }

    #[test]
    fn read_data_register_delegates_to_input_buffer() {
        let mut device = device();
        device.write(1, 0x42);
        assert_eq!(device.read(0), 0x42);
    }

    #[test]
    fn read_latch_register_delegates_to_input_buffer() {
        let mut device = device();
        device.input.push(0x42);
        assert_eq!(device.read(1), 0x42);
    }

    #[test]
    fn write_data_register_sends_byte_to_transport() {
        let (mut device, mut remote, _tx) = device_with_pipe(256);
        device.write(0, 0x42);
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x42));
    }

    #[test]
    fn write_latch_register_delegates_to_input_buffer() {
        let mut device = device();
        device.write(1, 0x42);
        assert_eq!(device.peek(1), 0x42);
    }

    #[test]
    fn write_break_key_to_latch_register_sets_interrupt_flag() {
        let mut device = device();
        device.set_break_key(0x3);
        device.write(1, 0x3);
        assert_eq!(device.peek(1), 0x3);
        assert!(device.irq_active(), "expected interrupt flag set");
    }

    #[test]
    fn peek_delegates_to_input_buffer_without_side_effects() {
        let mut device = device();
        device.input.push(0x42);
        assert_eq!(device.peek(0), 0x42);
        assert_eq!(device.peek(0), 0x42, "peek must not consume the buffered byte");
    }

    #[test]
    fn tick_buffers_input_from_transport() {
        let (mut device, _remote, tx) = device_with_pipe(256);
        tx.send(0x42).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert_eq!(device.peek(0), 0x42);
    }

    #[test]
    fn tick_latches_break_key_and_sets_interrupt_flag() {
        let (mut device, _remote, tx) = device_with_pipe(256);
        device.set_break_key(0x3);
        tx.send(0x3).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert_eq!(device.peek(1), 0x3);
        assert!(device.irq_active(), "expected interrupt flag set");
    }

    #[test]
    fn integration_cpu_program_writes_appear_on_transport() {
        use crate::emulator::cpu::StepResult;
        use crate::emulator::{
            AddressRange, BusConfig, CpuVariant, DeviceId, InternalPipeTransport,
        };

        let (local, mut remote) = InternalPipeTransport::pair_direct().unwrap();
        let (_tx, relay) = spawn_byte_relay(256);
        let mut console = device().with_address(0xF000);
        console.attach_transport(Box::new(local), TransportRelay::Byte(relay));

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
            match cpu.step(None, true) {
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
        use crate::emulator::cpu::StepResult;
        use crate::emulator::{
            AddressRange, BusConfig, CpuVariant, DeviceId, InternalPipeTransport,
        };

        let (local, _remote) = InternalPipeTransport::pair_direct().unwrap();
        let (tx, relay) = spawn_byte_relay(256);
        let mut console = device().with_address(0xF000);
        console.attach_transport(Box::new(local), TransportRelay::Byte(relay));

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
        tx.send(0x5A).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        loop {
            match cpu.step(None, true) {
                StepResult::Stopped => break,
                StepResult::Error(e) => panic!("CPU error: {:?}", e),
                _ => {}
            }
        }
        cpu.bus_mut().tick_devices(1);
        assert_eq!(cpu.bus_mut().read(0x0300).unwrap(), 0x5A);
    }

    #[test]
    fn reset_clears_latch() {
        let mut console = device();
        console.write(1, 0xff);
        console.reset();
        assert_eq!(console.peek(1), 0, "reset must clear the latch");
    }

    #[test]
    fn reset_logs_device_message() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let mut console = device().with_address(0xF000);
        console.set_log_sender(sender);
        console.reset();
        let received = rx.recv().unwrap();
        assert_eq!(received.category, LogCategory::Device);
        assert_eq!(received.message, format!("{DEVICE_NAME}@0xf000 reset"));
    }

}