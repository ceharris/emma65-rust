//! Reconstructs disassembled instruction lines from a stream of trace records,
//! as recorded by [`Cpu::step()`](crate::emulator::Cpu) bus tracing.

use super::{DisassembledLine, Disassembler};
use crate::emulator::bus::SymbolTable;
use crate::emulator::cpu::variant::CpuVariant;
use crate::emulator::{TraceKind, TraceRecord};

/// The in-progress instruction being reconstructed from trace records.
struct Pending {
    /// The instruction id this reconstruction belongs to.
    instr_id: u64,
    /// The instruction's starting address, from the preceding `Registers` snapshot.
    addr: u16,
    /// Opcode/operand bytes observed so far, in address order.
    raw_bytes: Vec<u8>,
    /// Total instruction length, known once the opcode byte has been observed.
    expected_len: Option<u8>,
}

/// Consumes a stream of [`TraceRecord`]s and reconstructs [`DisassembledLine`]s.
///
/// Bus reads only contribute to the instruction being reconstructed when their
/// address is the next sequential byte after the opcode; this naturally
/// excludes effective-address/pointer-table reads and instruction data reads,
/// since those don't land on consecutive addresses starting at the opcode.
/// Writes are never part of an opcode/operand fetch and are always ignored.
pub struct TraceDisassembler {
    disassembler: Disassembler,
    symbols: SymbolTable,
    pending: Option<Pending>,
}

impl TraceDisassembler {
    /// Creates a new trace disassembler for `variant`. Labels are resolved
    /// against a one-time clone of `symbols` taken at construction — matching
    /// the labels in effect when the trace was recorded, not any later state.
    pub fn new(variant: CpuVariant, symbols: SymbolTable) -> Self {
        Self { disassembler: Disassembler::new(variant), symbols, pending: None }
    }

    /// Feeds one trace record. Returns a completed line once an instruction's
    /// opcode/operand bytes have all been observed.
    pub fn feed(&mut self, rec: &TraceRecord) -> Option<DisassembledLine> {
        match rec.kind {
            TraceKind::Registers(regs) => {
                // A new instruction has started; any unfinished prior `Pending`
                // (e.g. from a step that halted before completing its own
                // opcode/operand fetch) is dropped.
                self.pending = Some(Pending {
                    instr_id: rec.instr_id,
                    addr: regs.pc,
                    raw_bytes: Vec::new(),
                    expected_len: None,
                });
                None
            }
            TraceKind::Read { addr, value } => self.feed_read(rec.instr_id, addr, value),
            TraceKind::Write { .. } => None,
        }
    }

    fn feed_read(&mut self, instr_id: u64, addr: u16, value: u8) -> Option<DisassembledLine> {
        let pending = self.pending.as_mut()?;
        if instr_id != pending.instr_id {
            return None;
        }
        let next_addr = pending.addr.wrapping_add(pending.raw_bytes.len() as u16);
        if addr != next_addr {
            return None;
        }
        pending.raw_bytes.push(value);
        if pending.expected_len.is_none() {
            pending.expected_len = Some(self.disassembler.decoded(pending.raw_bytes[0]).byte_len);
        }
        if Some(pending.raw_bytes.len() as u8) != pending.expected_len {
            return None;
        }
        let pending = self.pending.take().unwrap();
        Some(self.disassembler.build_line(pending.addr, pending.raw_bytes, &self.symbols))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::bus::{AddressRange, Bus, BusConfig};
    use crate::emulator::{CpuBuilder, Mnemonic, StepResult, TraceCallback};
    use std::sync::{Arc, Mutex};

    struct CapturingCallback(Arc<Mutex<Vec<TraceRecord>>>);

    impl TraceCallback for CapturingCallback {
        fn record(&mut self, rec: TraceRecord) {
            self.0.lock().unwrap().push(rec);
        }
    }

    fn program_bus(start: u16, prog: &[u8]) -> Bus {
        let mut bus = BusConfig::new()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        for (i, &b) in prog.iter().enumerate() {
            bus.write(start.wrapping_add(i as u16), b).unwrap();
        }
        bus
    }

    fn traced_records(variant: CpuVariant, start: u16, prog: &[u8], steps: usize) -> Vec<TraceRecord> {
        let mut bus = program_bus(start, prog);
        bus.write(0xFFFC, (start & 0xFF) as u8).unwrap();
        bus.write(0xFFFD, (start >> 8) as u8).unwrap();

        let mut cpu = CpuBuilder::new(variant).bus(bus).build().unwrap();
        cpu.reset().unwrap();

        let records = Arc::new(Mutex::new(Vec::new()));
        cpu.set_trace_callback(Some(Box::new(CapturingCallback(records.clone()))));

        for _ in 0..steps {
            assert!(matches!(cpu.step(None, true), StepResult::Executed(_)), "expected instruction to execute");
        }

        records.lock().unwrap().clone()
    }

    fn feed_all(td: &mut TraceDisassembler, records: &[TraceRecord]) -> Vec<DisassembledLine> {
        records.iter().filter_map(|rec| td.feed(rec)).collect()
    }

    #[test]
    fn single_byte_implied_instruction() {
        let records = traced_records(CpuVariant::Wdc65C02, 0x0200, &[0xE8], 1); // INX
        let mut td = TraceDisassembler::new(CpuVariant::Wdc65C02, SymbolTable::new());
        let lines = feed_all(&mut td, &records);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].addr, 0x0200);
        assert_eq!(lines[0].raw_bytes, vec![0xE8]);
        assert!(lines[0].mnemonic == Mnemonic::Inx);
    }

    #[test]
    fn two_byte_immediate_instruction() {
        let records = traced_records(CpuVariant::Wdc65C02, 0x0200, &[0xA9, 0x55], 1); // LDA #$55
        let mut td = TraceDisassembler::new(CpuVariant::Wdc65C02, SymbolTable::new());
        let lines = feed_all(&mut td, &records);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].addr, 0x0200);
        assert_eq!(lines[0].raw_bytes, vec![0xA9, 0x55]);
        assert!(lines[0].mnemonic == Mnemonic::Lda);
    }

    #[test]
    fn indirect_indexed_instruction_excludes_pointer_and_data_reads() {
        // LDA ($10),Y with the zero-page pointer at $10/$11 pointing at $0300.
        let mut bus = program_bus(0x0200, &[0xB1, 0x10]);
        bus.write(0x0010, 0x00).unwrap();
        bus.write(0x0011, 0x03).unwrap();
        bus.write(0x0300, 0x99).unwrap();
        bus.write(0xFFFC, 0x00).unwrap();
        bus.write(0xFFFD, 0x02).unwrap();

        let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02).bus(bus).build().unwrap();
        cpu.reset().unwrap();
        let records = Arc::new(Mutex::new(Vec::new()));
        cpu.set_trace_callback(Some(Box::new(CapturingCallback(records.clone()))));
        assert!(matches!(cpu.step(None, true), StepResult::Executed(_)));
        let records = records.lock().unwrap().clone();

        // The instruction is only 2 bytes, but its execution reads the pointer
        // bytes at $10/$11 and the final data byte at $0300 — none of those
        // should be mistaken for opcode/operand bytes.
        assert!(records.len() > 3, "test setup should have produced pointer/data reads to exclude");

        let mut td = TraceDisassembler::new(CpuVariant::Wdc65C02, SymbolTable::new());
        let lines = feed_all(&mut td, &records);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].raw_bytes, vec![0xB1, 0x10]);
        assert!(lines[0].mnemonic == Mnemonic::Lda);
    }

    #[test]
    fn instruction_with_data_write_ignores_the_write() {
        let records = traced_records(CpuVariant::Wdc65C02, 0x0200, &[0x8D, 0x00, 0x03], 1); // STA $0300
        let mut td = TraceDisassembler::new(CpuVariant::Wdc65C02, SymbolTable::new());
        let lines = feed_all(&mut td, &records);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].raw_bytes, vec![0x8D, 0x00, 0x03]);
        assert!(lines[0].mnemonic == Mnemonic::Sta);
    }

    #[test]
    fn matches_disassemble_range_over_a_real_program() {
        // LDA #$55 ; STA $0300 ; LDA $0300 ; INX
        let prog: &[u8] = &[0xA9, 0x55, 0x8D, 0x00, 0x03, 0xAD, 0x00, 0x03, 0xE8];
        let records = traced_records(CpuVariant::Wdc65C02, 0x0200, prog, 4);

        let mut td = TraceDisassembler::new(CpuVariant::Wdc65C02, SymbolTable::new());
        let trace_lines = feed_all(&mut td, &records);

        let reference_bus = program_bus(0x0200, prog);
        let reference = Disassembler::new(CpuVariant::Wdc65C02).disassemble_range(&reference_bus, 0x0200, 0x0200, 4);

        assert_eq!(trace_lines.len(), reference.len());
        for (t, r) in trace_lines.iter().zip(reference.iter()) {
            assert_eq!(t.addr, r.addr);
            assert_eq!(t.raw_bytes, r.raw_bytes);
            assert!(t.mnemonic == r.mnemonic);
            assert_eq!(t.operand_text, r.operand_text);
        }
    }
}
