use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use crate::emulator::transport::{Transport, TransportError};

/// WDC 65C51 ACIA (Asynchronous Communications Interface Adapter).
///
/// Provides four addressable registers:
///
/// | Offset | Read              | Write                            |
/// |--------|-------------------|----------------------------------|
/// | 0      | RX Data Register  | TX Data Register                 |
/// | 1      | Status Register   | Programmed Reset (any value)     |
/// | 2      | Command Register  | Command Register                 |
/// | 3      | Control Register  | Control Register                 |
///
/// **Status Register (offset 1 read):**
/// - Bit 7: IRQ — interrupt pending
/// - Bit 4: TDRE — Transmit Data Register Empty (ready to send)
/// - Bit 3: RDRF — Receive Data Register Full (byte available)
/// - Bit 2: OVRN — Overrun error
///
/// **Command Register (offset 2):**
/// - Bit 1 (IRD): Receive IRQ Disable — `0` = RX interrupt enabled, `1` = disabled
/// - Bits 3–2 (TIC): Transmit interrupt control — `01` = TX interrupt enabled, others = disabled
///
/// **Control Register (offset 3):**
/// - Bit 4: Receiver clock source — `0` = external (poll every tick), `1` = internal (baud rate)
/// - Bits 3–0: Baud rate select when bit 4 = 1 (0x1=50 … 0xF=19200 baud)
///
/// TX is immediate: bytes are sent to the transport on write; TDRE is always set.
/// RX is timer-driven: `tick()` polls the transport once per byte period at the configured
/// baud rate, or on every call when using the external clock (default).
pub struct Acia6551 {
    /// Optional transport for byte-stream IO.
    transport: Option<Box<dyn Transport>>,
    /// Destination for async transport error events.
    error_sender: Option<ErrorSender>,
    /// Identity used in error events.
    device_id: Option<DeviceId>,
    /// Most recently received byte.
    rx_data: u8,
    /// Receive Data Register Full — set when a byte has been received and not yet read.
    rdrf: bool,
    /// Transmit Data Register Empty — always true since TX is immediate.
    tdre: bool,
    /// Overrun error — set when a new byte arrives while RDRF is already set.
    overrun: bool,
    /// Command register value.
    command: u8,
    /// Control register value.
    control: u8,
    /// Accumulated cycles since the last transport poll.
    cycle_accum: u32,
    /// Cycles between transport polls; 0 = poll every tick (external clock mode).
    cycles_per_byte: u32,
}

impl Acia6551 {
    /// Creates a new `Acia6551` with no transport, external clock mode, and TDRE set.
    pub fn new() -> Self {
        Self {
            transport: None,
            error_sender: None,
            device_id: None,
            rx_data: 0,
            rdrf: false,
            tdre: true,
            overrun: false,
            command: 0,
            control: 0,
            cycle_accum: 0,
            cycles_per_byte: 0,
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
        if self.irq_active() { s |= 0x80; }
        if self.tdre { s |= 0x10; }
        if self.rdrf { s |= 0x08; }
        if self.overrun { s |= 0x04; }
        s
    }

    fn rx_irq_enabled(&self) -> bool {
        (self.command & 0x02) == 0
    }

    fn tx_irq_enabled(&self) -> bool {
        (self.command & 0x0C) == 0x04
    }

    fn poll_transport(&mut self) {
        if let Some(byte) = self.transport.as_mut().and_then(|t| t.try_recv()) {
            if self.rdrf {
                self.overrun = true;
            }
            self.rx_data = byte;
            self.rdrf = true;
        }
    }

    /// Returns cycles-per-byte for the given control register value, or 0 for external clock.
    fn compute_cycles_per_byte(control: u8) -> u32 {
        if (control & 0x10) == 0 {
            return 0; // external receiver clock: poll every tick
        }
        let baud: u32 = match control & 0x0F {
            0x01 => 50,
            0x02 => 75,
            0x03 => 110,
            0x04 => 134,
            0x05 => 150,
            0x06 => 300,
            0x07 => 600,
            0x08 => 1200,
            0x09 => 1800,
            0x0A => 2400,
            0x0B => 3600,
            0x0C => 4800,
            0x0D => 7200,
            0x0E => 9600,
            0x0F => 19200,
            _ => return 0,
        };
        // Assume 1 MHz CPU clock; 10 bits per byte (start + 8 data + stop).
        1_000_000 * 10 / baud
    }
}

impl Default for Acia6551 {
    fn default() -> Self {
        Self::new()
    }
}

impl IoDevice for Acia6551 {
    /// Reads the register at `offset`.
    ///
    /// Reading offset 0 (RX data) clears RDRF and overrun.
    fn read(&mut self, offset: u16) -> u8 {
        match offset {
            0 => {
                let val = self.rx_data;
                self.rdrf = false;
                self.overrun = false;
                val
            }
            1 => self.status(),
            2 => self.command,
            3 => self.control,
            _ => 0,
        }
    }

    /// Writes the register at `offset`.
    ///
    /// Writing offset 0 sends a byte to the transport. Writing offset 1 is a programmed
    /// reset that clears the overrun flag. Offsets 2 and 3 update command and control.
    fn write(&mut self, offset: u16, value: u8) {
        match offset {
            0 => {
                if let Some(transport) = self.transport.as_mut()
                    && let Err(e) = transport.send(value) {
                    self.report_error(e);
                }
            }
            1 => {
                // Programmed reset: clears overrun (any value written)
                self.overrun = false;
            }
            2 => {
                self.command = value;
            }
            3 => {
                self.control = value;
                self.cycles_per_byte = Self::compute_cycles_per_byte(value);
                self.cycle_accum = 0;
            }
            _ => {}
        }
    }

    /// Reads registers without side effects. Does not clear RDRF or overrun.
    fn peek(&self, offset: u16) -> u8 {
        match offset {
            0 => self.rx_data,
            1 => self.status(),
            2 => self.command,
            3 => self.control,
            _ => 0,
        }
    }

    /// Advances baud rate timing and polls the transport for incoming bytes.
    fn tick(&mut self, cycles: u32) {
        if self.cycles_per_byte == 0 {
            self.poll_transport();
        } else {
            self.cycle_accum += cycles;
            while self.cycle_accum >= self.cycles_per_byte {
                self.cycle_accum -= self.cycles_per_byte;
                self.poll_transport();
            }
        }
    }

    /// Returns `true` when IRQ is asserted:
    /// RDRF with RX interrupt enabled, or TDRE with TX interrupt enabled.
    fn irq_active(&self) -> bool {
        (self.rdrf && self.rx_irq_enabled()) || (self.tdre && self.tx_irq_enabled())
    }

    fn name(&self) -> &str {
        "acia6551"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::transport::PipeTransport;
    use std::time::Duration;

    fn device_with_pipe() -> (Acia6551, PipeTransport) {
        let (local, remote) = PipeTransport::pair().unwrap();
        let mut device = Acia6551::new();
        device.attach_transport(Box::new(local));
        (device, remote)
    }

    // --- Initial state ---

    #[test]
    fn new_has_tdre_set() {
        let device = Acia6551::new();
        assert_ne!(device.peek(1) & 0x10, 0);
    }

    #[test]
    fn new_has_rdrf_clear() {
        let device = Acia6551::new();
        assert_eq!(device.peek(1) & 0x08, 0);
    }

    // --- Command and Control register read/write ---

    #[test]
    fn write_read_command_register() {
        let mut device = Acia6551::new();
        device.write(2, 0x0A);
        assert_eq!(device.read(2), 0x0A);
    }

    #[test]
    fn write_read_control_register() {
        let mut device = Acia6551::new();
        device.write(3, 0x1E); // 9600 baud, internal clock
        assert_eq!(device.read(3), 0x1E);
    }

    // --- TX ---

    #[test]
    fn tx_sends_byte_to_transport() {
        let (mut device, mut remote) = device_with_pipe();
        device.write(0, 0x41);
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x41));
    }

    #[test]
    fn tx_no_transport_is_silent() {
        let mut device = Acia6551::new();
        device.write(0, 0xFF); // should not panic
    }

    // --- RX via tick() ---

    #[test]
    fn rx_byte_sets_rdrf() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0xBB).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1); // external clock: poll every tick
        assert_ne!(device.peek(1) & 0x08, 0); // RDRF set
    }

    #[test]
    fn rx_read_data_returns_byte_and_clears_rdrf() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0x55).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        assert_eq!(device.read(0), 0x55);
        assert_eq!(device.peek(1) & 0x08, 0); // RDRF cleared
    }

    #[test]
    fn overrun_set_when_second_byte_arrives_before_read() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0x01).unwrap();
        remote.send(0x02).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1); // receives 0x01 → RDRF
        device.tick(1); // receives 0x02 → OVRN
        assert_ne!(device.peek(1) & 0x04, 0); // OVRN set
    }

    #[test]
    fn programmed_reset_clears_overrun() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0x01).unwrap();
        remote.send(0x02).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        device.tick(1);
        device.write(1, 0x00); // programmed reset
        assert_eq!(device.peek(1) & 0x04, 0); // OVRN cleared
    }

    // --- Baud rate timing ---

    #[test]
    fn baud_rate_setting_controls_poll_timing() {
        let (mut device, mut remote) = device_with_pipe();
        device.write(3, 0x1F); // 19200 baud, internal receiver clock
        remote.send(0x42).unwrap();
        std::thread::sleep(Duration::from_millis(1));

        // One byte period at 19200 baud on a 1 MHz clock: 10/19200 * 1_000_000 = 520 cycles
        device.tick(519);
        assert_eq!(device.peek(1) & 0x08, 0); // not yet

        device.tick(1); // crosses threshold
        assert_ne!(device.peek(1) & 0x08, 0); // RDRF set
    }

    // --- IRQ ---

    #[test]
    fn irq_active_on_rdrf_when_rx_irq_enabled() {
        let (mut device, mut remote) = device_with_pipe();
        device.write(2, 0x00); // IRD=0: RX IRQ enabled
        remote.send(0x01).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        assert!(device.irq_active());
    }

    #[test]
    fn irq_inactive_when_rx_irq_disabled() {
        let (mut device, mut remote) = device_with_pipe();
        device.write(2, 0x02); // IRD=1: RX IRQ disabled
        remote.send(0x01).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        assert!(!device.irq_active());
    }

    #[test]
    fn irq_active_on_tdre_when_tx_irq_enabled() {
        let mut device = Acia6551::new();
        device.write(2, 0x04); // TIC=01: TX IRQ enabled
        assert!(device.irq_active()); // TDRE is always set
    }

    #[test]
    fn irq_inactive_on_tdre_when_tx_irq_disabled() {
        let mut device = Acia6551::new();
        device.write(2, 0x00); // TIC=00: TX IRQ disabled
        assert!(!device.irq_active());
    }

    // --- Peek ---

    #[test]
    fn peek_does_not_clear_rdrf() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0xCC).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        let _ = device.peek(0); // peek at data register
        assert_ne!(device.peek(1) & 0x08, 0); // RDRF still set
    }

    #[test]
    fn peek_returns_rx_data_without_consuming() {
        let (mut device, mut remote) = device_with_pipe();
        remote.send(0x77).unwrap();
        std::thread::sleep(Duration::from_millis(1));
        device.tick(1);
        assert_eq!(device.peek(0), 0x77);
        assert_eq!(device.read(0), 0x77); // still available
    }
}
