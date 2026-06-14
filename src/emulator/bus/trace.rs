use std::io::{BufWriter, Write};
use std::time::Instant;
use crate::emulator::bus::region::BusOp;

/// A single recorded bus transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceRecord {
    /// Nanoseconds elapsed on the host monotonic clock since the trace epoch.
    pub timestamp_ns: u64,
    /// The bus address accessed.
    pub addr: u16,
    /// The byte value read or written.
    pub value: u8,
    /// Whether this was a read or a write.
    pub op: BusOp,
}

/// Receives bus trace records as they are generated.
pub trait BusTraceCallback: Send {
    /// Called once for each bus read or write. Never called for `peek`.
    fn record(&mut self, rec: TraceRecord);
}

/// Writes trace records as fixed-width 12-byte binary records.
///
/// Record layout (little-endian):
/// - Bytes 0–7: `timestamp_ns` (u64)
/// - Bytes 8–9: `addr` (u16)
/// - Byte 10:   `value` (u8)
/// - Byte 11:   `op` (0 = Read, 1 = Write)
pub struct BinaryTraceWriter<W: Write> {
    writer: BufWriter<W>,
}

impl<W: Write> BinaryTraceWriter<W> {
    /// Creates a new writer wrapping `inner`.
    pub fn new(inner: W) -> Self {
        Self { writer: BufWriter::new(inner) }
    }

    /// Flushes buffered records to the underlying writer.
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

impl<W: Write + Send> BusTraceCallback for BinaryTraceWriter<W> {
    fn record(&mut self, rec: TraceRecord) {
        let op_byte: u8 = match rec.op {
            BusOp::Read  => 0,
            BusOp::Write => 1,
        };
        let mut buf = [0u8; 12];
        buf[0..8].copy_from_slice(&rec.timestamp_ns.to_le_bytes());
        buf[8..10].copy_from_slice(&rec.addr.to_le_bytes());
        buf[10] = rec.value;
        buf[11] = op_byte;
        // Best-effort write; errors are silently swallowed to avoid disrupting emulation.
        let _ = self.writer.write_all(&buf);
    }
}

/// Manages the monotonic clock epoch and current instruction timestamp for bus tracing.
pub(super) struct TraceState {
    /// Monotonic epoch captured at `TraceState::new()`.
    epoch: Instant,
    /// Nanoseconds since epoch, set once per CPU instruction.
    current_ns: u64,
}

impl TraceState {
    pub(super) fn new() -> Self {
        Self { epoch: Instant::now(), current_ns: 0 }
    }

    /// Updates the timestamp to the current wall-clock time. Called by `Cpu::step()` once per instruction.
    pub(super) fn tick(&mut self) {
        self.current_ns = self.epoch.elapsed().as_nanos() as u64;
    }

    pub(super) fn current_ns(&self) -> u64 {
        self.current_ns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CapturingCallback(Vec<TraceRecord>);

    impl BusTraceCallback for CapturingCallback {
        fn record(&mut self, rec: TraceRecord) {
            self.0.push(rec);
        }
    }

    #[test]
    fn trace_record_fields() {
        let rec = TraceRecord { timestamp_ns: 12345, addr: 0x0200, value: 0xAB, op: BusOp::Read };
        assert_eq!(rec.timestamp_ns, 12345);
        assert_eq!(rec.addr, 0x0200);
        assert_eq!(rec.value, 0xAB);
        assert_eq!(rec.op, BusOp::Read);
    }

    #[test]
    fn binary_writer_record_layout() {
        let buf = {
            let mut buf = Vec::new();
            let mut writer = BinaryTraceWriter::new(&mut buf);
            writer.record(TraceRecord { timestamp_ns: 0x0102030405060708, addr: 0x1234, value: 0xAB, op: BusOp::Read });
            writer.record(TraceRecord { timestamp_ns: 0, addr: 0x5678, value: 0xCD, op: BusOp::Write });
            writer.flush().unwrap();
            drop(writer);
            buf
        };

        assert_eq!(buf.len(), 24);

        // First record
        assert_eq!(&buf[0..8], &0x0102030405060708u64.to_le_bytes());
        assert_eq!(&buf[8..10], &0x1234u16.to_le_bytes());
        assert_eq!(buf[10], 0xAB);
        assert_eq!(buf[11], 0); // Read

        // Second record
        assert_eq!(&buf[12..20], &0u64.to_le_bytes());
        assert_eq!(&buf[20..22], &0x5678u16.to_le_bytes());
        assert_eq!(buf[22], 0xCD);
        assert_eq!(buf[23], 1); // Write
    }

    #[test]
    fn capturing_callback_receives_records() {
        let mut cb = CapturingCallback(Vec::new());
        cb.record(TraceRecord { timestamp_ns: 1, addr: 0x0100, value: 0x42, op: BusOp::Write });
        cb.record(TraceRecord { timestamp_ns: 2, addr: 0x0101, value: 0x00, op: BusOp::Read });
        assert_eq!(cb.0.len(), 2);
        assert_eq!(cb.0[0].op, BusOp::Write);
        assert_eq!(cb.0[1].op, BusOp::Read);
    }

    #[test]
    fn trace_state_tick_advances_monotonically() {
        let mut state = TraceState::new();
        state.tick();
        let t1 = state.current_ns();
        state.tick();
        let t2 = state.current_ns();
        assert!(t2 >= t1, "timestamps must be monotonically non-decreasing");
    }
}
