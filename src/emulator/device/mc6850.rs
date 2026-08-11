//! Motorola MC6850 Asynchronous Communications Interface Adapter (ACIA).
//!
//! Provides two addressable registers:
//!
//! | Offset | Read             | Write            |
//! |--------|------------------|------------------|
//! | 0      | Status Register  | Control Register |
//! | 1      | RX Data Register | TX Data Register |
//!
//! **Control Register (offset 0 write):**
//! - Bits 1–0 (CD): Counter Divide — `11` = master reset; `00`/`01`/`10` = ÷1/÷16/÷64
//! - Bits 4–2 (WS): Word Select — data bits, parity, stop bits
//! - Bits 6–5 (TC): Transmit Control — `10` = TX interrupt enabled; others = disabled
//! - Bit 7 (RIE): Receive Interrupt Enable — `1` = enabled
//!
//! **Status Register (offset 0 read):**
//! - Bit 0: RDRF — Receive Data Register Full
//! - Bit 1: TDRE — Transmit Data Register Empty
//! - Bit 2: DCD — Data Carrier Detect (always 0 in emulation)
//! - Bit 3: CTS — Clear To Send (always 0 in emulation)
//! - Bit 4: FE — Framing Error (always 0)
//! - Bit 5: OVRN — Overrun Error
//! - Bit 6: PE — Parity Error (always 0)
//! - Bit 7: IRQ — Interrupt Requested
//!
//! TX is immediate: bytes sent on write. TDRE clears on TX write and is restored on the
//! next `tick()` call, reflecting the real hardware's transmit-busy signaling.
//! RX is polled on every `tick()` call.

use std::collections::VecDeque;

use crate::emulator::device::IoDevice;
use crate::emulator::transport::{Transport, TransportRelay};
use crate::emulator::{LogCategory, LogLevel, LogSender, log_msg};

/// Motorola MC6850 Asynchronous Communications Interface Adapter (ACIA).
pub struct Mc6850 {
    name: &'static str,
    address: u16,
    transport: Option<Box<dyn Transport>>,
    /// Paired with `transport`; drained into `rx_buffer` once per `tick()`.
    relay: Option<TransportRelay>,
    /// Bytes drained from `relay` but not yet clocked into `rx_data`.
    /// Decouples "arrived from the transport" from "visible to the CPU,
    /// one byte at a time, gated by RDRF" — before this device's relay
    /// migration, the transport itself played this role implicitly, since
    /// `tick()` read directly from it at most once per call.
    rx_buffer: VecDeque<u8>,
    control: u8,
    rx_data: u8,
    rdrf: bool,
    tdre: bool,
    overrun: bool,
    tx_pending: bool,
    /// Sender for structured diagnostic messages (e.g. `reset()`).
    log_sender: LogSender,
}

/// Control register bit masks.
const CD_MASK: u8 = 0x03;
const CD_MASTER_RESET: u8 = 0x03;
const TC_MASK: u8 = 0x60;
const TC_TX_IRQ: u8 = 0x40; // TC bits = 10
const RIE_MASK: u8 = 0x80;

impl Mc6850 {
    /// Creates a new `Mc6850` with no transport and master-reset state.
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            address: 0,
            transport: None,
            relay: None,
            rx_buffer: VecDeque::new(),
            control: 0,
            rx_data: 0,
            rdrf: false,
            tdre: true,
            overrun: false,
            tx_pending: false,
            log_sender: LogSender::default(),
        }
    }

    /// Sets the address at which this device is registered on the bus.
    pub fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }

    /// Attaches a transport and its paired relay for byte-stream IO.
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>, relay: TransportRelay) {
        self.transport = Some(transport);
        self.relay = Some(relay);
    }

    /// Installs a log sender for diagnostic messages (e.g. `reset()`).
    pub fn set_log_sender(&mut self, sender: LogSender) {
        self.log_sender = sender;
    }

    fn status(&self) -> u8 {
        let mut s = 0u8;
        if self.rdrf { s |= 0x01; }
        if self.tdre { s |= 0x02; }
        if self.overrun { s |= 0x20; }
        if self.irq_active() { s |= 0x80; }
        s
    }

    fn rx_irq_enabled(&self) -> bool {
        self.control & RIE_MASK != 0
    }

    fn tx_irq_enabled(&self) -> bool {
        self.control & TC_MASK == TC_TX_IRQ
    }

    fn apply_master_reset(&mut self) {
        self.rx_data = 0;
        self.rdrf = false;
        self.tdre = true;
        self.overrun = false;
        self.tx_pending = false;
    }
}

impl IoDevice for Mc6850 {

    fn read(&mut self, address: u16) -> u8 {
        match address - self.address {
            0 => self.status(),
            1 => {
                let val = self.rx_data;
                self.rdrf = false;
                self.overrun = false;
                val
            }
            _ => 0,
        }
    }

    fn write(&mut self, address: u16, value: u8) {
        match address - self.address {
            0 => {
                self.control = value;
                if value & CD_MASK == CD_MASTER_RESET {
                    self.apply_master_reset();
                }
            }
            1 => {
                if let Some(transport) = self.transport.as_mut() {
                    transport.send(value);
                }
                self.tdre = false;
                self.tx_pending = true;
            }
            _ => {}
        }
    }

    fn peek(&self, address: u16) -> u8 {
        match address - self.address {
            0 => self.status(),
            1 => self.rx_data,
            _ => 0,
        }
    }

    fn tick(&mut self, _cycles: u32) {
        if let Some(relay) = self.relay.as_mut() {
            let rx_buffer = &mut self.rx_buffer;
            relay.drain_bytes_into(|b| rx_buffer.push_back(b));
        }

        if self.tx_pending {
            self.tx_pending = false;
            self.tdre = true;
        }
        if !self.rdrf
            && let Some(byte) = self.rx_buffer.pop_front()
        {
            self.rx_data = byte;
            self.rdrf = true;
        }
    }

    fn reset(&mut self) {
        let address = self.address;
        let transport = std::mem::take(&mut self.transport);
        let relay = std::mem::take(&mut self.relay);
        let log_sender = self.log_sender.clone();
        *self = Self::new(self.name);
        self.address = address;
        self.transport = transport;
        self.relay = relay;
        self.log_sender = log_sender;
        log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} reset", self.identity());
    }

    fn irq_active(&self) -> bool {
        (self.rdrf && self.rx_irq_enabled()) || (self.tdre && self.tx_irq_enabled())
    }

    fn name(&self) -> &str {
        self.name
    }

    fn identity_address(&self) -> u16 {
        self.address
    }

    fn shutdown(&mut self) {
        if let Some(transport) = self.transport.as_mut() {
            transport.shutdown();
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::transport::{ChannelRelay, InternalPipeTransport};
    use crossbeam_channel::{Sender, unbounded};
    use std::time::Duration;

    const DEVICE_NAME: &str = "mc6850";

    fn device() -> Mc6850 {
        Mc6850::new(DEVICE_NAME)
    }

    /// A `ChannelRelay<u8>` fed by a plain, unbounded `crossbeam_channel`,
    /// for deterministic control over exactly what a device's `tick()`
    /// observes as "arrived" — independent of `InternalPipeTransport`'s own
    /// OS-pipe timing. `pair_direct()` gives both ends of the test pipe no
    /// relay of their own, so this hand-fed one stands in for it.
    fn spawn_byte_relay(capacity: usize) -> (Sender<u8>, ChannelRelay<u8>) {
        let (tx, rx) = unbounded();
        (tx, ChannelRelay::spawn(rx, capacity))
    }

    /// `remote` is the device's outbound-write sink (verified via
    /// `remote.try_recv()`); `tx` feeds the device's inbound relay directly,
    /// simulating bytes arriving from an external peer.
    fn device_with_pipe() -> (Mc6850, InternalPipeTransport, Sender<u8>) {
        let (local, remote) = InternalPipeTransport::pair_direct().unwrap();
        let (tx, relay) = spawn_byte_relay(256);
        let mut device = device();
        device.attach_transport(Box::new(local), TransportRelay::Byte(relay));
        (device, remote, tx)
    }

    // --- Initial state ---

    #[test]
    fn new_has_tdre_set() {
        let device = device();
        assert_ne!(device.peek(0) & 0x02, 0);
    }

    #[test]
    fn new_has_rdrf_clear() {
        let device = device();
        assert_eq!(device.peek(0) & 0x01, 0);
    }

    // --- Control register ---

    #[test]
    fn write_control_register_stores_value() {
        let mut device = device();
        device.write(0, 0x56); // WS + TC bits, CD=10 (not master reset)
        assert_eq!(device.control, 0x56);
    }

    #[test]
    fn master_reset_clears_rdrf() {
        let (mut device, _remote, tx) = device_with_pipe();
        tx.send(0xAA).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1); // RDRF
        assert_ne!(device.peek(0) & 0x01, 0); // RDRF set
        device.write(0, 0x03); // master reset
        assert_eq!(device.peek(0) & 0x01, 0); // RDRF cleared
    }

    #[test]
    fn master_reset_keeps_tdre_set() {
        let mut device = device();
        device.write(0, 0x03); // master reset
        assert_ne!(device.peek(0) & 0x02, 0);
    }

    // --- TX ---

    #[test]
    fn tx_sends_byte_to_transport() {
        let (mut device, mut remote, _tx) = device_with_pipe();
        device.write(1, 0x58);
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x58));
    }

    #[test]
    fn tx_no_transport_is_silent() {
        let mut device = device();
        device.write(1, 0xFF); // should not panic
    }

    #[test]
    fn tdre_clears_on_tx_write() {
        let (mut device, _remote, _tx) = device_with_pipe();
        assert_ne!(device.peek(0) & 0x02, 0); // TDRE set before write
        device.write(1, 0x41);
        assert_eq!(device.peek(0) & 0x02, 0); // TDRE cleared after TX write
    }

    #[test]
    fn tdre_restores_after_tick() {
        let (mut device, _remote, _tx) = device_with_pipe();
        device.write(1, 0x41);
        assert_eq!(device.peek(0) & 0x02, 0); // TDRE cleared
        device.tick(1);
        assert_ne!(device.peek(0) & 0x02, 0); // TDRE restored
    }

    // --- RX ---

    #[test]
    fn rx_byte_sets_rdrf() {
        let (mut device, _remote, tx) = device_with_pipe();
        tx.send(0xBB).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert_ne!(device.peek(0) & 0x01, 0); // RDRF set
    }

    #[test]
    fn rx_read_returns_byte_and_clears_rdrf() {
        let (mut device, _remote, tx) = device_with_pipe();
        tx.send(0x44).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert_eq!(device.read(1), 0x44);
        assert_eq!(device.peek(0) & 0x01, 0); // RDRF cleared
    }

    #[test]
    fn second_byte_held_in_transport_until_first_read() {
        let (mut device, _remote, tx) = device_with_pipe();
        tx.send(0x01).unwrap();
        tx.send(0x02).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1); // receives 0x01 → RDRF
        device.tick(1); // 0x02 stays in the internal rx buffer (RDRF still set)
        assert_eq!(device.read(1), 0x01);
        device.tick(1); // now receives 0x02
        assert_eq!(device.read(1), 0x02);
    }

    // --- IRQ ---

    #[test]
    fn irq_on_rdrf_when_rx_irq_enabled() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(0, 0x81); // RIE=1, CD=01
        tx.send(0x01).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert!(device.irq_active());
    }

    #[test]
    fn no_irq_on_rdrf_when_rx_irq_disabled() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(0, 0x01); // RIE=0, CD=01
        tx.send(0x01).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert!(!device.irq_active());
    }

    #[test]
    fn irq_on_tdre_when_tx_irq_enabled() {
        let mut device = device();
        device.write(0, 0x41); // TC=10 (TX IRQ enabled), CD=01
        assert!(device.irq_active()); // TDRE is always set
    }

    #[test]
    fn no_irq_on_tdre_when_tx_irq_disabled() {
        let mut device = device();
        device.write(0, 0x01); // TC=00, CD=01
        assert!(!device.irq_active());
    }

    // --- Peek ---

    #[test]
    fn peek_does_not_clear_rdrf() {
        let (mut device, _remote, tx) = device_with_pipe();
        tx.send(0x99).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        let _ = device.peek(1);
        assert_ne!(device.peek(0) & 0x01, 0); // RDRF still set
    }

    #[test]
    fn peek_returns_rx_data_without_consuming() {
        let (mut device, _remote, tx) = device_with_pipe();
        tx.send(0x33).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert_eq!(device.peek(1), 0x33);
        assert_eq!(device.read(1), 0x33); // still available
    }

    // reset

    #[test]
    fn reset_clears_irq() {
        let mut device = device();
        device.rdrf = true;
        device.tdre = true;
        device.control = RIE_MASK | TC_TX_IRQ;
        assert!(device.irq_active(), "expected IRQ active");
        device.reset();
        assert!(!device.irq_active(), "IRQ must not be active after reset");
    }

    #[test]
    fn reset_clears_control_and_status_registers_and_pending_irq() {
        let mut device = device();
        device.rdrf = true;
        device.tdre = true;
        device.reset();
        assert!(device.tdre, "TRDE must be set after reset");
        assert!(!device.rdrf, "RDRF must be clear after reset");
    }

    #[test]
    fn reset_logs_device_message() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let mut device = device().with_address(0xC000);
        device.set_log_sender(sender);
        device.reset();
        let received = rx.recv().unwrap();
        assert_eq!(received.category, LogCategory::Device);
        assert_eq!(received.message, format!("{DEVICE_NAME}@0xc000 reset"));
    }

}
