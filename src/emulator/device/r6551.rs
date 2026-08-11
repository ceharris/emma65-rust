//! Rockwell R6551 Asynchronous Communications Interface Adapter (ACIA).
//!
//! Provides four addressable registers:
//!
//! | Offset | Read              | Write                            |
//! |--------|-------------------|----------------------------------|
//! | 0      | RX Data Register  | TX Data Register                 |
//! | 1      | Status Register   | Programmed Reset (any value)     |
//! | 2      | Command Register  | Command Register                 |
//! | 3      | Control Register  | Control Register                 |
//!
//! **Status Register (offset 1 read):**
//! - Bit 7: IRQ — interrupt pending
//! - Bit 4: TDRE — Transmit Data Register Empty (ready to send)
//! - Bit 3: RDRF — Receive Data Register Full (byte available)
//! - Bit 2: OVRN — Overrun error
//!
//! **Command Register (offset 2):**
//! - Bit 1 (IRD): Receive IRQ Disable — `0` = RX interrupt enabled, `1` = disabled
//! - Bits 3–2 (TIC): Transmit interrupt control — `01` = TX interrupt enabled, others = disabled
//!
//! **Control Register (offset 3):**
//! - Bit 4: Receiver clock source — `0` = external (poll every tick), `1` = internal (baud rate)
//! - Bits 3–0: Baud rate select when bit 4 = 1 (0x1=50 … 0xF=19200 baud)
//!
//! TX is immediate: bytes are sent to the transport on write.
//!
//! # TDRE behaviour and the WDC 65C51 hardware bug
//!
//! The WDC 65C51 made by Western Design Center has a well-known silicon bug: TDRE is permanently
//! stuck high and is never cleared after a TX write. Software targeting the real chip therefore
//! cannot poll TDRE to detect transmit-ready; it must use fixed timing delays instead.
//!
//! This emulation supports two modes, selectable at construction time:
//!
//! - **Correct mode** (default): TDRE clears when a byte is written to the TX register and
//!   is restored after one byte-period worth of cycles (or on the next `tick()` call in
//!   external-clock mode). Use this for new software that does not rely on the hardware bug.
//! - **Bug-compatible mode** ([`R6551::with_tdre_bug`]): TDRE is permanently set,
//!   matching real-hardware behaviour. Use this when running software written for the
//!   actual WDC 65C51 chip.
//!
//! RX is timer-driven: `tick()` polls the transport once per byte period at the configured
//! baud rate, or on every call when using the external clock (default).

use std::collections::VecDeque;

use crate::emulator::device::IoDevice;
use crate::emulator::transport::{Transport, TransportRelay};
use crate::emulator::{LogCategory, LogLevel, LogSender, log_msg};

/// Rockwell R6551 ACIA (Asynchronous Communications Interface Adapter).
pub struct R6551 {
    name: &'static str,
    address: u16,
    transport: Option<Box<dyn Transport>>,
    /// Paired with `transport`; drained into `rx_buffer` once per `tick()`.
    relay: Option<TransportRelay>,
    /// Bytes drained from `relay` but not yet clocked into `rx_data` by
    /// `poll_transport`'s baud-rate pacing. Decouples "arrived from the
    /// transport" from "visible to the CPU at the configured baud rate" —
    /// before this device's relay migration, the transport itself
    /// (implicitly, via its own buffering) played this role, since
    /// `poll_transport` read directly from it at most once per byte period.
    rx_buffer: VecDeque<u8>,
    rx_data: u8,
    rdrf: bool,
    tdre: bool,
    overrun: bool,
    command: u8,
    control: u8,
    cycle_accum: u32,
    cycles_per_byte: u32,
    tdre_bug_compatible: bool,
    tx_cycles_remaining: u32,
    clock_hz: u64,
    overrun_enabled: bool,
    /// Sender for structured diagnostic messages (e.g. `reset()`).
    log_sender: LogSender,
}

const DEFAULT_CLOCK_HZ: u64 = 1_000_000;

const RX_IRQ_ENABLE: u8 = 0x2;
const TX_IRQ_MASK: u8 = 0xC;
const TX_IRQ_ENABLE: u8 = 0x4;

const COMMAND_DTR: u8 = 0x1;

impl R6551 {
    /// Creates a new `R6551` in correct (non-bug-compatible) mode with TDRE set.
    ///
    /// The default CPU clock is 1 MHz. Use [`R6551::with_clock_hz`] to match the actual
    /// CPU clock speed so that baud rate timing is accurate.
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            address: 0,
            transport: None,
            relay: None,
            rx_buffer: VecDeque::new(),
            rx_data: 0,
            rdrf: false,
            tdre: true,
            overrun: false,
            command: 0,
            control: 0,
            cycle_accum: 0,
            cycles_per_byte: 0,
            tdre_bug_compatible: false,
            tx_cycles_remaining: 0,
            clock_hz: DEFAULT_CLOCK_HZ,
            overrun_enabled: false,
            log_sender: LogSender::default(),
        }
    }

    /// Sets the CPU clock frequency used to compute baud rate timing.
    ///
    /// Only used when the control register selects internal clock mode (bit 4 set).
    /// In external clock mode the transport is polled on every `tick()` regardless
    /// of this value.
    ///
    /// Defaults to 1 MHz if not set.
    pub fn with_clock_hz(mut self, clock_hz: u64) -> Self {
        self.clock_hz = clock_hz;
        self
    }

    /// Enables or disables WDC 65C51 bug-compatible mode: TDRE is permanently set and never cleared
    /// after a TX write, matching the behavior of the real hardware.
    ///
    /// Use this when running software written for the actual WDC 65C51 chip that relies
    /// on timing delays rather than polling TDRE.
    pub fn with_tdre_bug(mut self, enabled: bool) -> Self {
        self.tdre_bug_compatible = enabled;
        self
    }

    /// Enables or disables receive overrun in internal clock mode.
    ///
    /// When enabled, a byte arriving from the transport while RDRF is already set will
    /// overwrite `rx_data` and set the overrun flag, matching real 65C51 hardware where
    /// the shift register clocks in the next byte regardless of whether the CPU has read
    /// the previous one.
    ///
    /// Has no effect in external-clock mode, where the transport is not timing-driven and
    /// bytes are held in the pipe until RDRF is cleared.
    pub fn with_overrun(mut self, enabled: bool) -> Self {
        self.overrun_enabled = enabled;
        self
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
        if self.irq_active() { s |= 0x80; }
        if self.tdre { s |= 0x10; }
        if self.rdrf { s |= 0x08; }
        if self.overrun { s |= 0x04; }
        s
    }

    fn rx_is_enabled(&self) -> bool {
        (self.command & COMMAND_DTR) != 0
    }

    fn rx_irq_enabled(&self) -> bool {
        self.rx_is_enabled() && (self.command & RX_IRQ_ENABLE) == 0
    }

    fn tx_irq_enabled(&self) -> bool {
        (self.command & TX_IRQ_MASK) == TX_IRQ_ENABLE
    }

    fn poll_transport(&mut self, allow_overrun: bool) {
        if !self.rx_is_enabled() || (self.rdrf && !allow_overrun) {
            return;
        }
        if let Some(byte) = self.rx_buffer.pop_front() {
            if self.rdrf {
                self.overrun = true;
            }
            self.rx_data = byte;
            self.rdrf = true;
        }
    }

    /// Returns cycles-per-byte for the given control register value and CPU clock, or 0 for external clock.
    ///
    /// Uses 10 bits per byte (1 start + 8 data + 1 stop). The control register's word-select
    /// bits (bits 6–5) encode the actual data bits, parity, and stop-bit configuration, but
    /// this calculation ignores them. Revisit whether using the configured word size and stop
    /// bit count would be feasible and useful.
    fn compute_cycles_per_byte(control: u8, clock_hz: u64) -> u32 {
        if (control & 0x10) == 0 {
            return 0; // external receiver clock: poll every tick
        }
        let baud: u64 = match control & 0x0F {
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
        (clock_hz * 10 / baud) as u32
    }
}

impl IoDevice for R6551 {

    fn read(&mut self, address: u16) -> u8 {
        match address - self.address {
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

    fn write(&mut self, address: u16, value: u8) {
        match address - self.address {
            0 => {
                if let Some(transport) = self.transport.as_mut() {
                    transport.send(value);
                }
                if !self.tdre_bug_compatible {
                    self.tdre = false;
                    // Restore TDRE after one byte period (or on next tick if external clock).
                    self.tx_cycles_remaining = if self.cycles_per_byte > 0 {
                        self.cycles_per_byte
                    } else {
                        1
                    };
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
                self.cycles_per_byte = Self::compute_cycles_per_byte(value, self.clock_hz);
                self.cycle_accum = 0;
            }
            _ => {}
        }
    }

    fn peek(&self, address: u16) -> u8 {
        match address - self.address {
            0 => self.rx_data,
            1 => self.status(),
            2 => self.command,
            3 => self.control,
            _ => 0,
        }
    }

    fn tick(&mut self, cycles: u32) {
        if let Some(relay) = self.relay.as_mut() {
            let rx_buffer = &mut self.rx_buffer;
            relay.drain_bytes_into(|b| rx_buffer.push_back(b));
        }

        if !self.tdre && !self.tdre_bug_compatible {
            if cycles >= self.tx_cycles_remaining {
                self.tx_cycles_remaining = 0;
                self.tdre = true;
            } else {
                self.tx_cycles_remaining -= cycles;
            }
        }

        if self.cycles_per_byte == 0 {
            self.poll_transport(false);
        } else {
            self.cycle_accum += cycles;
            while self.cycle_accum >= self.cycles_per_byte {
                self.cycle_accum -= self.cycles_per_byte;
                self.poll_transport(self.overrun_enabled);
            }
        }
    }

    fn reset(&mut self) {
        let address = self.address;
        let transport = std::mem::take(&mut self.transport);
        let relay = std::mem::take(&mut self.relay);
        let clock_hz = self.clock_hz;
        let tdre_bug_compatible = self.tdre_bug_compatible;
        let overrun_enabled = self.overrun_enabled;
        let log_sender = self.log_sender.clone();
        *self = Self::new(self.name);
        self.address = address;
        self.transport = transport;
        self.relay = relay;
        self.clock_hz = clock_hz;
        self.tdre_bug_compatible = tdre_bug_compatible;
        self.overrun_enabled = overrun_enabled;
        self.log_sender = log_sender;
        log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} @0x{:04x} reset", self.name(), self.address);
    }

    fn irq_active(&self) -> bool {
        (self.rdrf && self.rx_irq_enabled()) || (self.tdre && self.tx_irq_enabled())
    }

    fn name(&self) -> &str {
        self.name
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

    const DEVICE_NAME: &str = "";

    fn device() -> R6551 {
        R6551::new(DEVICE_NAME)
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
    fn device_with_pipe() -> (R6551, InternalPipeTransport, Sender<u8>) {
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
        assert_ne!(device.peek(1) & 0x10, 0);
    }

    #[test]
    fn new_has_rdrf_clear() {
        let device = device();
        assert_eq!(device.peek(1) & 0x08, 0);
    }

    // --- Command and Control register read/write ---

    #[test]
    fn write_read_command_register() {
        let mut device = device();
        device.write(2, 0x0A);
        assert_eq!(device.read(2), 0x0A);
    }

    #[test]
    fn write_read_control_register() {
        let mut device = device();
        device.write(3, 0x1E); // 9600 baud, internal clock
        assert_eq!(device.read(3), 0x1E);
    }

    // --- TX ---

    #[test]
    fn tx_sends_byte_to_transport() {
        let (mut device, mut remote, _tx) = device_with_pipe();
        device.write(0, 0x41);
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x41));
    }

    #[test]
    fn tx_no_transport_is_silent() {
        let mut device = device();
        device.write(0, 0xFF); // should not panic
    }

    // --- RX via tick() ---

    #[test]
    fn rx_byte_deferred_when_dtr_not_asserted() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x0);   // deassert DTR
        tx.send(0xBB).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1); // external clock: poll every tick
        assert_eq!(device.peek(0), 0); // no value read
        assert_eq!(device.peek(1) & 0x08, 0); // RDRF not set
    }

    #[test]
    fn rx_byte_sets_rdrf() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x1);   // assert DTR
        tx.send(0xBB).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1); // external clock: poll every tick
        assert_ne!(device.peek(1) & 0x08, 0); // RDRF set
    }

    #[test]
    fn rx_read_data_returns_byte_and_clears_rdrf() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x1);   // assert DTR
        tx.send(0x55).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert_eq!(device.read(0), 0x55);
        assert_eq!(device.peek(1) & 0x08, 0); // RDRF cleared
    }

    #[test]
    fn second_byte_held_in_transport_until_first_read() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x1);   // assert DTR
        tx.send(0x01).unwrap();
        tx.send(0x02).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1); // receives 0x01 → RDRF
        device.tick(1); // 0x02 stays in the internal rx buffer (RDRF still set)
        assert_eq!(device.read(0), 0x01);
        device.tick(1); // now receives 0x02
        assert_eq!(device.read(0), 0x02);
    }

    // --- Overrun ---

    #[test]
    fn overrun_set_in_internal_clock_mode_with_overrun_enabled() {
        let (local, _remote) = InternalPipeTransport::pair_direct().unwrap();
        let (tx, relay) = spawn_byte_relay(256);
        let mut device = device()
            .with_clock_hz(1_000_000)
            .with_overrun(true);
        device.attach_transport(Box::new(local), TransportRelay::Byte(relay));
        // 19200 baud internal clock: cycles_per_byte = 1_000_000 * 10 / 19200 = 520
        device.write(2, 0x1);   // assert DTR
        device.write(3, 0x1F);
        tx.send(0x01).unwrap();
        tx.send(0x02).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(520); // receives 0x01 → RDRF
        device.tick(520); // receives 0x02 → OVRN (overwrites rx_data)
        assert_ne!(device.peek(1) & 0x04, 0); // OVRN set
        assert_eq!(device.read(0), 0x02); // second byte overwrote first
    }

    #[test]
    fn no_overrun_in_external_clock_mode_even_with_flag() {
        let (local, _remote) = InternalPipeTransport::pair_direct().unwrap();
        let (tx, relay) = spawn_byte_relay(256);
        let mut device = device()
            .with_overrun(true);
        device.attach_transport(Box::new(local), TransportRelay::Byte(relay));
        device.write(2, 0x1);   // assert DTR
        // Control defaults to 0x00 → external clock (cycles_per_byte = 0)
        tx.send(0x01).unwrap();
        tx.send(0x02).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1); // receives 0x01 → RDRF
        device.tick(1); // 0x02 stays in the internal rx buffer (external clock ignores overrun flag)
        assert_eq!(device.peek(1) & 0x04, 0); // OVRN not set
        assert_eq!(device.read(0), 0x01);
        device.tick(1); // now receives 0x02
        assert_eq!(device.read(0), 0x02);
    }

    // --- Baud rate timing ---

    #[test]
    fn baud_rate_setting_controls_poll_timing() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x1);  // assert DTR
        device.write(3, 0x1F); // 19200 baud, internal receiver clock
        tx.send(0x42).unwrap();
        std::thread::sleep(Duration::from_millis(5));

        // One byte period at 19200 baud on a 1 MHz clock: 10/19200 * 1_000_000 = 520 cycles
        device.tick(519);
        assert_eq!(device.peek(1) & 0x08, 0); // not yet

        device.tick(1); // crosses threshold
        assert_ne!(device.peek(1) & 0x08, 0); // RDRF set
    }

    // --- IRQ ---

    #[test]
    fn irq_active_on_rdrf_when_rx_irq_enabled_and_dtr_asserted() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x01); // IRD=0, DTR=1: RX IRQ enabled
        tx.send(0x01).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert!(device.irq_active());
    }

    #[test]
    fn irq_inactive_when_rx_dtr_not_asserted() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x2); // IRD=1, DTR=0: RX IRQ disabled
        tx.send(0x01).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert!(!device.irq_active());
    }

    #[test]
    fn irq_inactive_when_rx_irq_disabled() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x3); // IRD=1, DTR=1: RX IRQ disabled
        tx.send(0x01).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert!(!device.irq_active());
    }

    #[test]
    fn irq_active_on_tdre_when_tx_irq_enabled() {
        let mut device = device();
        device.write(2, 0x04); // TIC=01: TX IRQ enabled
        assert!(device.irq_active()); // TDRE is always set
    }

    #[test]
    fn irq_inactive_on_tdre_when_tx_irq_disabled() {
        let mut device = device();
        device.write(2, 0x00); // TIC=00: TX IRQ disabled
        assert!(!device.irq_active());
    }

    // --- TDRE behaviour ---

    #[test]
    fn tdre_clears_on_tx_write_in_correct_mode() {
        let (mut device, _remote, _tx) = device_with_pipe();
        assert_ne!(device.peek(1) & 0x10, 0); // TDRE set before write
        device.write(0, 0x41);
        assert_eq!(device.peek(1) & 0x10, 0); // TDRE cleared after TX write
    }

    #[test]
    fn tdre_restores_after_tick_in_correct_mode() {
        let (mut device, _remote, _tx) = device_with_pipe();
        device.write(0, 0x41); // clears TDRE; external clock sets tx_cycles_remaining = 1
        device.tick(1);
        assert_ne!(device.peek(1) & 0x10, 0); // TDRE restored
    }

    #[test]
    fn tdre_always_set_in_bug_compatible_mode() {
        let (local, _remote) = InternalPipeTransport::pair_direct().unwrap();
        let (_tx, relay) = spawn_byte_relay(256);
        let mut device = device().with_tdre_bug(true);
        device.attach_transport(Box::new(local), TransportRelay::Byte(relay));
        device.write(0, 0x41); // TX write — should NOT clear TDRE
        assert_ne!(device.peek(1) & 0x10, 0);
        device.tick(1000); // many ticks — TDRE must stay set
        assert_ne!(device.peek(1) & 0x10, 0);
    }

    #[test]
    fn tdre_restores_after_baud_rate_period_in_correct_mode() {
        let (mut device, _remote, _tx) = device_with_pipe();
        device.write(3, 0x1F); // 19200 baud, internal clock → 520 cycles/byte
        device.write(0, 0x41);
        assert_eq!(device.peek(1) & 0x10, 0); // TDRE cleared
        device.tick(519);
        assert_eq!(device.peek(1) & 0x10, 0); // still not restored
        device.tick(1);
        assert_ne!(device.peek(1) & 0x10, 0); // TDRE restored after full period
    }

    // --- Peek ---

    #[test]
    fn peek_does_not_clear_rdrf() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x1);   // assert DTR
        tx.send(0xCC).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        let _ = device.peek(0); // peek at data register
        assert_ne!(device.peek(1) & 0x08, 0); // RDRF still set
    }

    #[test]
    fn peek_returns_rx_data_without_consuming() {
        let (mut device, _remote, tx) = device_with_pipe();
        device.write(2, 0x1);   // assert DTR
        tx.send(0x77).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        device.tick(1);
        assert_eq!(device.peek(0), 0x77);
        assert_eq!(device.read(0), 0x77); // still available
    }

    // reset

    #[test]
    fn reset_clears_command_control_and_status_registers() {
        let mut device = device();
        device.rdrf = true;
        device.tdre = true;
        device.reset();
        assert_eq!(device.command, 0, "command register must be zero after reset");
        assert_eq!(device.control, 0, "command register must be zero after reset");
        assert!(device.tdre, "TRDE must be set after reset");
        assert!(!device.rdrf, "RDRF must be clear after reset");
    }

    #[test]
    fn reset_clears_irq() {
        let mut device = device();
        device.rdrf = true;
        device.tdre = true;
        device.command = RX_IRQ_ENABLE | TX_IRQ_ENABLE;
        assert!(device.irq_active(), "expected IRQ active");
        device.reset();
        assert!(!device.irq_active(), "IRQ must not be active after reset");
    }

    #[test]
    fn reset_preserves_configuration_attributes() {
        let mut device = device()
            .with_clock_hz(1_843_200)
            .with_tdre_bug(true)
            .with_overrun(true);
        device.reset();
        assert_eq!(device.clock_hz, 1_843_200, "clock_hz must be preserved after reset");
        assert!(device.tdre_bug_compatible, "tdre_bug_compatible must be preserved after reset");
        assert!(device.overrun_enabled, "overrun_enabled must be preserved after reset");
    }

    #[test]
    fn reset_logs_device_message() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let mut device = device().with_address(0xC000);
        device.set_log_sender(sender);
        device.reset();
        let received = rx.recv().unwrap();
        assert_eq!(received.category, LogCategory::Device);
        assert_eq!(received.message, format!("{DEVICE_NAME} @0xc000 reset"));
    }

}
