//! A memory-mapped keyboard input device, the input-only counterpart to
//! [`Console`](super::console::Console) — meant to pair with an output-only display device (e.g.
//! `display/char`) in a config that has no `console`.
//!
//! Provides the same two 8-bit addressable registers as `Console`, backed by a shared
//! [`InputBuffer`](super::input_buffer::InputBuffer):
//!
//! | Offset |      Name      |
//! |--------|----------------|
//! | 0      | Data Register  |
//! | 1      | Latch Register |
//!
//! Semantics for both registers are identical to `Console`'s input half (see its module docs).
//! The one difference: **writing the Data Register is a no-op**. This device has no outbound byte
//! stream to send to — it is purely input.

use super::IoDevice;
use super::input_buffer::InputBuffer;
use crate::emulator::transport::{Transport, TransportRelay};
use crate::emulator::{LogCategory, LogLevel, LogSender, log_msg};

/// A memory-mapped, input-only keyboard device.
pub struct Keyboard {
    name: &'static str,
    address: u16,
    transport: Option<Box<dyn Transport>>,
    /// Paired with `transport`, drained once per `tick()`. `None` exactly
    /// when `transport` is `None`.
    relay: Option<TransportRelay>,
    input: InputBuffer,
    /// Sender for structured diagnostic messages (e.g. `reset()`).
    log_sender: LogSender,
}

impl Keyboard {

    /// Creates a new `Keyboard` with no transport attached.
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

    /// Attaches a transport for byte-stream input, along with its paired relay.
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>, relay: TransportRelay) {
        self.transport = Some(transport);
        self.relay = Some(relay);
    }

    /// Sets the break key to recognize when reading from the transport.
    pub fn set_break_key(&mut self, break_key: u8) {
        self.input.set_break_key(break_key);
    }

    /// Installs a log sender for diagnostic messages (e.g. `reset()`).
    pub fn set_log_sender(&mut self, sender: LogSender) {
        self.log_sender = sender;
    }

}

impl IoDevice for Keyboard {

    fn read(&mut self, address: u16) -> u8 {
        match address - self.address {
            0 => self.input.read_data(),
            1 => self.input.read_latch(),
            _ => 0,
        }
    }

    fn write(&mut self, address: u16, value: u8) {
        match address - self.address {
            0 => (), // data register: this device is input-only, writes have no effect
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

    const DEVICE_NAME: &str = "keyboard";

    fn device() -> Keyboard {
        Keyboard::new(DEVICE_NAME)
    }

    /// See `Console`'s test module for the rationale behind this hand-fed relay harness.
    fn spawn_byte_relay(capacity: usize) -> (Sender<u8>, ChannelRelay<u8>) {
        let (tx, rx) = unbounded();
        (tx, ChannelRelay::spawn(rx, capacity))
    }

    fn device_with_pipe(relay_capacity: usize) -> (Keyboard, InternalPipeTransport, Sender<u8>) {
        let (local, remote) = InternalPipeTransport::pair_direct().unwrap();
        let (tx, relay) = spawn_byte_relay(relay_capacity);
        let mut device = device();
        device.attach_transport(Box::new(local), TransportRelay::Byte(relay));
        (device, remote, tx)
    }

    #[test]
    fn read_data_register_delegates_to_input_buffer() {
        let mut device = device();
        device.input.write_latch(0x42);
        assert_eq!(device.read(0), 0x42);
    }

    #[test]
    fn read_latch_register_delegates_to_input_buffer() {
        let mut device = device();
        device.input.push(0x42);
        assert_eq!(device.read(1), 0x42);
    }

    #[test]
    fn write_data_register_is_noop() {
        let (mut device, mut remote, _tx) = device_with_pipe(256);
        device.write(0, 0x42);
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(remote.try_recv(), None, "expected no outbound byte");
        assert_eq!(device.peek(0), 0, "expected no state change");
    }

    #[test]
    fn write_latch_register_delegates_to_input_buffer() {
        let mut device = device();
        device.write(1, 0x42);
        assert_eq!(device.peek(1), 0x42);
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
    fn reset_logs_device_message() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let mut device = device().with_address(0xF000);
        device.set_log_sender(sender);
        device.reset();
        let received = rx.recv().unwrap();
        assert_eq!(received.category, LogCategory::Device);
        assert_eq!(received.message, format!("{DEVICE_NAME}@0xf000 reset"));
    }

    #[test]
    fn integration_transport_input_readable_by_cpu() {
        use crate::emulator::cpu::StepResult;
        use crate::emulator::{
            AddressRange, BusConfig, CpuVariant, DeviceId, InternalPipeTransport,
        };

        let (local, _remote) = InternalPipeTransport::pair_direct().unwrap();
        let (tx, relay) = spawn_byte_relay(256);
        let mut keyboard = device().with_address(0xF000);
        keyboard.attach_transport(Box::new(local), TransportRelay::Byte(relay));

        let bus = BusConfig::new()
            .ram_with_fill(AddressRange::new(0x0000, 0xEFFF), 0).unwrap()
            .device(AddressRange::new(0xF000, 0xF001), DeviceId(1), Box::new(keyboard)).unwrap()
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

}
