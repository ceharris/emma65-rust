use crate::emulator::device::{DeviceId, ErrorSender, IoDevice};
use crate::emulator::transport::{Transport, TransportError};

/// Simple polling console device.
///
/// Provides two addressable registers:
///
/// | Offset | Read                                      | Write                        |
/// |--------|-------------------------------------------|------------------------------|
/// | 0      | Data Input (see below)                    | Data Output — send to transport |
/// | 1      | Data Latch — poll and latch input byte    | Set latch (0 = clear, non-zero = simulate input) |
///
/// **Offset 0 read (Data Input):**
/// - When the latch is zero: returns non-zero if a byte is available, zero if none.
/// - When the latch is non-zero: returns the latched value and resets the latch to zero.
///
/// **Offset 1 read (Data Latch):**
/// - Calls `try_recv()` on the transport. If a byte is available, stores it in the latch
///   and returns the latched value. Returns zero if nothing is available.
///
/// No IRQ — purely poll-based. Designed for use with `PipeTransport` connecting to a
/// terminal emulator.
pub struct Console {
    /// Optional transport for byte-stream IO.
    transport: Option<Box<dyn Transport>>,
    /// Destination for async transport error events.
    error_sender: Option<ErrorSender>,
    /// Identity used in error events.
    device_id: Option<DeviceId>,
    /// Current value of the Data Latch register.
    latch: u8,
}

impl Console {
    /// Creates a new `Console` with no transport attached.
    pub fn new() -> Self {
        Self {
            transport: None,
            error_sender: None,
            device_id: None,
            latch: 0,
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

    /// Sends a transport error event if an error sender is configured.
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
    /// Offset 0 read: Data Input.
    ///
    /// If the latch is non-zero, returns the latched value and clears the latch.
    /// Otherwise returns non-zero if a byte is available on the transport, zero if not.
    fn read(&mut self, offset: u16) -> u8 {
        match offset {
            0 => {
                if self.latch != 0 {
                    let val = self.latch;
                    self.latch = 0;
                    val
                } else {
                    match self.transport.as_mut().and_then(|t| t.try_recv()) {
                        Some(_) => 0xFF,
                        None => 0x00,
                    }
                }
            }
            1 => {
                if let Some(byte) = self.transport.as_mut().and_then(|t| t.try_recv()) {
                    self.latch = byte;
                }
                self.latch
            }
            _ => 0,
        }
    }

    /// Offset 0 write: sends `value` to the transport.
    /// Offset 1 write: sets the latch (zero = clear, non-zero = simulate input).
    fn write(&mut self, offset: u16, value: u8) {
        match offset {
            0 => {
                if let Some(transport) = self.transport.as_mut()
                    && let Err(e) = transport.send(value) {
                    self.report_error(e);
                }
            }
            1 => {
                self.latch = value;
            }
            _ => {}
        }
    }

    /// Peek does not consume input or trigger transport IO.
    fn peek(&self, offset: u16) -> u8 {
        match offset {
            0 if self.latch != 0 => self.latch,
            0 => 0,
            1 => self.latch,
            _ => 0,
        }
    }

    fn name(&self) -> &str {
        "console"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::transport::PipeTransport;

    fn console_with_pipe() -> (Console, PipeTransport) {
        let (local, remote) = PipeTransport::pair().unwrap();
        let mut console = Console::new();
        console.attach_transport(Box::new(local));
        (console, remote)
    }

    // --- Output (offset 0 write) ---

    #[test]
    fn write_offset0_sends_byte_to_transport() {
        let (mut console, mut remote) = console_with_pipe();
        console.write(0, 0x42);
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x42));
    }

    // --- Data Latch (offset 1) ---

    #[test]
    fn read_offset1_latches_byte_from_transport() {
        let (mut console, mut remote) = console_with_pipe();
        remote.send(0xAB).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(console.read(1), 0xAB);
        assert_eq!(console.read(1), 0xAB); // latch holds until cleared
    }

    #[test]
    fn read_offset1_returns_zero_when_no_input() {
        let mut console = Console::new();
        assert_eq!(console.read(1), 0);
    }

    #[test]
    fn write_offset1_nonzero_simulates_input() {
        let mut console = Console::new();
        console.write(1, 0x55);
        assert_eq!(console.read(1), 0x55);
    }

    #[test]
    fn write_offset1_zero_clears_latch() {
        let mut console = Console::new();
        console.write(1, 0x55);
        console.write(1, 0x00);
        assert_eq!(console.read(1), 0x00);
    }

    // --- Data Input (offset 0 read) ---

    #[test]
    fn read_offset0_returns_nonzero_when_byte_available() {
        let (mut console, mut remote) = console_with_pipe();
        remote.send(0x01).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        // latch is zero, so offset 0 peeks at availability
        assert_ne!(console.read(0), 0);
    }

    #[test]
    fn read_offset0_returns_zero_when_no_byte_available() {
        let mut console = Console::new();
        assert_eq!(console.read(0), 0);
    }

    #[test]
    fn read_offset0_returns_latch_and_clears_when_latch_nonzero() {
        let mut console = Console::new();
        console.write(1, 0x77); // simulate input via latch
        assert_eq!(console.read(0), 0x77);
        assert_eq!(console.read(0), 0x00); // latch was cleared
    }

    // --- No transport ---

    #[test]
    fn no_transport_reads_return_zero() {
        let mut console = Console::new();
        assert_eq!(console.read(0), 0);
        assert_eq!(console.read(1), 0);
    }

    #[test]
    fn no_transport_writes_are_silent() {
        let mut console = Console::new();
        console.write(0, 0xFF); // should not panic
    }

    // --- Peek ---

    #[test]
    fn peek_does_not_consume_input_or_trigger_transport() {
        let (mut console, mut remote) = console_with_pipe();
        remote.send(0xCC).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        // peek at offset 0: latch is zero, so returns 0 (no transport call)
        assert_eq!(console.peek(0), 0);
        // the byte should still be available for a real read
        assert_eq!(console.read(1), 0xCC);
    }

    #[test]
    fn peek_offset0_returns_latch_without_clearing() {
        let mut console = Console::new();
        console.write(1, 0x33);
        assert_eq!(console.peek(0), 0x33);
        assert_eq!(console.peek(0), 0x33); // still there
    }

    #[test]
    fn peek_offset1_returns_latch_without_consuming() {
        let mut console = Console::new();
        console.write(1, 0x44);
        assert_eq!(console.peek(1), 0x44);
        assert_eq!(console.read(1), 0x44);
    }

    // --- Integration: CPU program writes bytes via Bus ---

    #[test]
    fn integration_cpu_program_writes_appear_on_transport() {
        use crate::emulator::{
            AddressRange, BusConfig, CpuVariant, DeviceId, PipeTransport,
        };
        use crate::emulator::exec::StepResult;

        let (local, mut remote) = PipeTransport::pair().unwrap();
        let mut console = Console::new();
        console.attach_transport(Box::new(local));

        // Map all of RAM (including reset vector region) plus console at 0xF000.
        // Using RAM for 0xFF00–0xFFFF lets us write the reset vector after build().
        let bus = BusConfig::new()
            .ram(AddressRange::new(0x0000, 0xEFFF)).unwrap()
            .device(AddressRange::new(0xF000, 0xF001), DeviceId(1), Box::new(console)).unwrap()
            .ram(AddressRange::new(0xFF00, 0xFFFF)).unwrap()
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
            match cpu.step() {
                StepResult::Stopped => break,
                StepResult::Error(e) => panic!("CPU error: {:?}", e),
                _ => {}
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x41));
        assert_eq!(remote.try_recv(), Some(0x42));
    }
}
