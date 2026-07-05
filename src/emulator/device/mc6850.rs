use log::{debug};
use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use crate::emulator::transport::{Transport, TransportError};

/// Motorola MC6850 ACIA (Asynchronous Communications Interface Adapter).
///
/// Provides two addressable registers:
///
/// | Offset | Read             | Write            |
/// |--------|------------------|------------------|
/// | 0      | Status Register  | Control Register |
/// | 1      | RX Data Register | TX Data Register |
///
/// **Control Register (offset 0 write):**
/// - Bits 1–0 (CD): Counter Divide — `11` = master reset; `00`/`01`/`10` = ÷1/÷16/÷64
/// - Bits 4–2 (WS): Word Select — data bits, parity, stop bits
/// - Bits 6–5 (TC): Transmit Control — `10` = TX interrupt enabled; others = disabled
/// - Bit 7 (RIE): Receive Interrupt Enable — `1` = enabled
///
/// **Status Register (offset 0 read):**
/// - Bit 0: RDRF — Receive Data Register Full
/// - Bit 1: TDRE — Transmit Data Register Empty
/// - Bit 2: DCD — Data Carrier Detect (always 0 in emulation)
/// - Bit 3: CTS — Clear To Send (always 0 in emulation)
/// - Bit 4: FE — Framing Error (always 0)
/// - Bit 5: OVRN — Overrun Error
/// - Bit 6: PE — Parity Error (always 0)
/// - Bit 7: IRQ — Interrupt Requested
///
/// TX is immediate: bytes sent on write. TDRE clears on TX write and is restored on the
/// next `tick()` call, reflecting the real hardware's transmit-busy signalling.
/// RX is polled on every `tick()` call.
pub struct Mc6850 {
    /// Optional transport for byte-stream IO.
    transport: Option<Box<dyn Transport>>,
    /// Destination for async transport error events.
    error_sender: Option<ErrorSender>,
    /// Identity used in error events.
    device_id: Option<DeviceId>,
    /// Current control register value.
    control: u8,
    /// Most recently received byte.
    rx_data: u8,
    /// Receive Data Register Full — byte waiting to be read.
    rdrf: bool,
    /// Transmit Data Register Empty — clears on TX write; restored on the next tick.
    tdre: bool,
    /// Overrun — set when a new byte arrives while RDRF is still set.
    overrun: bool,
    /// When true, TDRE was cleared by a TX write and will be restored on the next tick.
    tx_pending: bool,
}

/// Control register bit masks.
const CD_MASK: u8 = 0x03;
const CD_MASTER_RESET: u8 = 0x03;
const TC_MASK: u8 = 0x60;
const TC_TX_IRQ: u8 = 0x40; // TC bits = 10
const RIE_MASK: u8 = 0x80;

impl Mc6850 {
    /// Creates a new `Mc6850` with no transport and master-reset state.
    pub fn new() -> Self {
        Self {
            transport: None,
            error_sender: None,
            device_id: None,
            control: 0,
            rx_data: 0,
            rdrf: false,
            tdre: true,
            overrun: false,
            tx_pending: false,
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

    fn report_error(&self, error: TransportError) {
        if let (Some(sender), Some(id)) = (&self.error_sender, self.device_id) {
            use crate::emulator::device::DeviceEvent;
            let _ = sender.send(DeviceEvent::TransportError { device: id, error });
        }
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

impl Default for Mc6850 {
    fn default() -> Self {
        Self::new()
    }
}

impl IoDevice for Mc6850 {
    /// Reads the register at `offset`.
    ///
    /// Reading offset 1 (RX data) clears RDRF and overrun.
    fn read(&mut self, offset: u16) -> u8 {
        match offset {
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

    /// Writes the register at `offset`.
    ///
    /// Writing offset 0 updates the control register; master reset (`CD=11`) resets device state.
    /// Writing offset 1 sends a byte to the transport.
    fn write(&mut self, offset: u16, value: u8) {
        match offset {
            0 => {
                self.control = value;
                if value & CD_MASK == CD_MASTER_RESET {
                    self.apply_master_reset();
                }
            }
            1 => {
                if let Some(transport) = self.transport.as_mut()
                    && let Err(e) = transport.send(value) {
                    self.report_error(e);
                }
                self.tdre = false;
                self.tx_pending = true;
            }
            _ => {}
        }
    }

    /// Reads registers without side effects. Does not clear RDRF or overrun.
    fn peek(&self, offset: u16) -> u8 {
        match offset {
            0 => self.status(),
            1 => self.rx_data,
            _ => 0,
        }
    }

    /// Restores TDRE after a TX write and polls the transport for an incoming byte.
    fn tick(&mut self, _cycles: u32) {
        if self.tx_pending {
            self.tx_pending = false;
            self.tdre = true;
        }
        if !self.rdrf
            && let Some(byte) = self.transport.as_mut().and_then(|t| t.try_recv())
        {
            self.rx_data = byte;
            self.rdrf = true;
        }
    }

    /// Resets the control and status registers as if a hardware reset has occurred.
    fn reset(&mut self) {
        let transport = std::mem::take(&mut self.transport);
        let error_sender = self.error_sender.take();
        let device_id = self.device_id;
        *self = Self::new();
        self.transport = transport;
        self.error_sender = error_sender;
        self.device_id = device_id;
        debug!("{} {} reset", self.name(), self.device_id.unwrap());
    }

    /// Returns `true` when IRQ is asserted:
    /// RDRF with RX interrupt enabled, or TDRE with TX interrupt enabled.
    fn irq_active(&self) -> bool {
        (self.rdrf && self.rx_irq_enabled()) || (self.tdre && self.tx_irq_enabled())
    }

    fn name(&self) -> &str {
        "acia/6850"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::transport::PipeTransport;
    use std::time::Duration;

    fn device_with_pipe() -> (Mc6850, PipeTransport) {
        let (local, remote) = PipeTransport::pair().unwrap();
        let mut device = Mc6850::new();
        device.attach_transport(Box::new(local));
        (device, remote)
    }

    // --- Initial state ---

    #[test]
    fn new_has_tdre_set() {
        let device = Mc6850::new();
        assert_ne!(device.peek(0) & 0x02, 0);
    }

    #[test]
    fn new_has_rdrf_clear() {
        let device = Mc6850::new();
        assert_eq!(device.peek(0) & 0x01, 0);
    }

    // --- Control register ---

    #[test]
    fn write_control_register_stores_value() {
        let mut device = Mc6850::new();
        device.write(0, 0x56); // WS + TC bits, CD=10 (not master reset)
        assert_eq!(device.control, 0x56);
    }

    #[test]
    fn master_reset_clears_rdrf() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0xAA).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1); // RDRF
        assert_ne!(device.peek(0) & 0x01, 0); // RDRF set
        device.write(0, 0x03); // master reset
        assert_eq!(device.peek(0) & 0x01, 0); // RDRF cleared
    }

    #[test]
    fn master_reset_keeps_tdre_set() {
        let mut device = Mc6850::new();
        device.write(0, 0x03); // master reset
        assert_ne!(device.peek(0) & 0x02, 0);
    }

    // --- TX ---

    #[test]
    fn tx_sends_byte_to_transport() {
        let (mut device, mut remote) = device_with_pipe();
        device.write(1, 0x58);
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x58));
    }

    #[test]
    fn tx_no_transport_is_silent() {
        let mut device = Mc6850::new();
        device.write(1, 0xFF); // should not panic
    }

    #[test]
    fn tdre_clears_on_tx_write() {
        let (mut device, _remote) = device_with_pipe();
        assert_ne!(device.peek(0) & 0x02, 0); // TDRE set before write
        device.write(1, 0x41);
        assert_eq!(device.peek(0) & 0x02, 0); // TDRE cleared after TX write
    }

    #[test]
    fn tdre_restores_after_tick() {
        let (mut device, _remote) = device_with_pipe();
        device.write(1, 0x41);
        assert_eq!(device.peek(0) & 0x02, 0); // TDRE cleared
        device.tick(1);
        assert_ne!(device.peek(0) & 0x02, 0); // TDRE restored
    }

    // --- RX ---

    #[test]
    fn rx_byte_sets_rdrf() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0xBB).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        assert_ne!(device.peek(0) & 0x01, 0); // RDRF set
    }

    #[test]
    fn rx_read_returns_byte_and_clears_rdrf() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0x44).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        assert_eq!(device.read(1), 0x44);
        assert_eq!(device.peek(0) & 0x01, 0); // RDRF cleared
    }

    #[test]
    fn second_byte_held_in_transport_until_first_read() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0x01).unwrap();
        remote.send(0x02).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1); // receives 0x01 → RDRF
        device.tick(1); // 0x02 stays in pipe (RDRF still set)
        assert_eq!(device.read(1), 0x01);
        device.tick(1); // now receives 0x02
        assert_eq!(device.read(1), 0x02);
    }

    // --- IRQ ---

    #[test]
    fn irq_on_rdrf_when_rx_irq_enabled() {
        let (mut device, mut remote) = device_with_pipe();
        device.write(0, 0x81); // RIE=1, CD=01
        remote.send(0x01).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        assert!(device.irq_active());
    }

    #[test]
    fn no_irq_on_rdrf_when_rx_irq_disabled() {
        let (mut device, mut remote) = device_with_pipe();
        device.write(0, 0x01); // RIE=0, CD=01
        remote.send(0x01).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        assert!(!device.irq_active());
    }

    #[test]
    fn irq_on_tdre_when_tx_irq_enabled() {
        let mut device = Mc6850::new();
        device.write(0, 0x41); // TC=10 (TX IRQ enabled), CD=01
        assert!(device.irq_active()); // TDRE is always set
    }

    #[test]
    fn no_irq_on_tdre_when_tx_irq_disabled() {
        let mut device = Mc6850::new();
        device.write(0, 0x01); // TC=00, CD=01
        assert!(!device.irq_active());
    }

    // --- Peek ---

    #[test]
    fn peek_does_not_clear_rdrf() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0x99).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        let _ = device.peek(1);
        assert_ne!(device.peek(0) & 0x01, 0); // RDRF still set
    }

    #[test]
    fn peek_returns_rx_data_without_consuming() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0x33).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        assert_eq!(device.peek(1), 0x33);
        assert_eq!(device.read(1), 0x33); // still available
    }

    // reset

    #[test]
    fn reset_preserves_bus_config() {
        let (mut device, _) = device_with_pipe();
        device.device_id = Some(DeviceId(0));
        device.reset();
        assert!(device.transport.is_some(), "expected transport to be preserved");
        assert!(device.device_id.is_some(), "expected device ID to be preserved");
    }

    #[test]
    fn reset_clears_irq() {
        let mut device = Mc6850::new();
        device.rdrf = true;
        device.tdre = true;
        device.control = RIE_MASK | TC_TX_IRQ;
        assert!(device.irq_active(), "expected IRQ active");
        device.reset();
        assert!(!device.irq_active(), "IRQ must not be active after reset");
    }

    #[test]
    fn reset_clears_control_and_status_registers_and_pending_irq() {
        let mut device = Mc6850::new();
        device.rdrf = true;
        device.tdre = true;
        device.reset();
        assert!(device.tdre, "TRDE must be set after reset");
        assert!(!device.rdrf, "RDRF must be clear after reset");
    }

}
