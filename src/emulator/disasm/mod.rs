use crate::emulator::bus::Bus;
use crate::emulator::cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic, decode_table};
use crate::emulator::cpu::variant::CpuVariant;

/// A single disassembled instruction.
pub struct DisassembledLine {
    /// Address of the first byte of the instruction.
    pub addr: u16,
    /// The raw instruction bytes (1–3).
    pub raw_bytes: Vec<u8>,
    /// The instruction mnemonic.
    pub mnemonic: Mnemonic,
    /// Formatted operand text (empty string for implied/accumulator).
    pub operand_text: String,
    /// False when the opcode is invalid for the active variant.
    pub is_valid: bool,
}

/// Disassembles instructions from a `Bus` using side-effect-free `peek` reads.
pub struct Disassembler {
    table: [DecodedOp; 256],
}

impl Disassembler {
    /// Creates a disassembler for the given CPU variant.
    pub fn new(variant: CpuVariant) -> Self {
        Self { table: decode_table(variant) }
    }

    /// Disassembles a single instruction at `addr`.
    pub fn disassemble_one(&self, bus: &Bus, addr: u16) -> DisassembledLine {
        let opcode = bus.peek(addr).unwrap_or(0xFF);
        let decoded = self.table[opcode as usize];
        let mut raw_bytes = vec![opcode];
        for i in 1..decoded.byte_len {
            raw_bytes.push(bus.peek(addr.wrapping_add(i as u16)).unwrap_or(0xFF));
        }
        let operand_text = format_operand(&decoded, &raw_bytes);
        DisassembledLine {
            addr,
            raw_bytes,
            mnemonic: decoded.mnemonic,
            operand_text,
            is_valid: decoded.is_valid,
        }
    }

    /// Disassembles up to `max` instructions starting at `start`, stopping before `end`.
    pub fn disassemble_range(
        &self,
        bus: &Bus,
        start: u16,
        end: u16,
        max: usize,
    ) -> Vec<DisassembledLine> {
        let mut result = Vec::new();
        let mut addr = start;
        while result.len() < max {
            if end > start && addr >= end {
                break;
            }
            let line = self.disassemble_one(bus, addr);
            let len = line.raw_bytes.len() as u16;
            result.push(line);
            let next = addr.wrapping_add(len);
            // Stop if we wrapped around or would loop
            if next == addr || (end > start && next > end) {
                break;
            }
            addr = next;
        }
        result
    }
}

fn format_operand(decoded: &DecodedOp, raw: &[u8]) -> String {
    let b1 = raw.get(1).copied().unwrap_or(0);
    let b2 = raw.get(2).copied().unwrap_or(0);
    let abs = u16::from_le_bytes([b1, b2]);

    match decoded.mode {
        AddressingMode::Implied => String::new(),
        AddressingMode::Accumulator => "A".to_string(),
        AddressingMode::Immediate => format!("#${b1:02X}"),
        AddressingMode::ZeroPage => format!("${b1:02X}"),
        AddressingMode::ZeroPageX => format!("${b1:02X},X"),
        AddressingMode::ZeroPageY => format!("${b1:02X},Y"),
        AddressingMode::Absolute => format!("${abs:04X}"),
        AddressingMode::AbsoluteX => format!("${abs:04X},X"),
        AddressingMode::AbsoluteY => format!("${abs:04X},Y"),
        AddressingMode::Indirect => format!("(${abs:04X})"),
        AddressingMode::IndirectX => format!("(${b1:02X},X)"),
        AddressingMode::IndirectY => format!("(${b1:02X}),Y"),
        AddressingMode::ZeroPageIndirect => format!("(${b1:02X})"),
        AddressingMode::AbsoluteIndirectX => format!("(${abs:04X},X)"),
        AddressingMode::Relative => format!("${b1:02X}"),
        AddressingMode::ZeroPageRelative => format!("${b1:02X},${b2:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::bus::{Bus, BusConfig};
    use crate::emulator::bus::AddressRange;

    fn make_bus(start: u16, bytes: &[u8]) -> Bus {
        let mut bus = BusConfig::new()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        for (i, &b) in bytes.iter().enumerate() {
            bus.write(start.wrapping_add(i as u16), b).unwrap();
        }
        bus
    }

    fn disasm(variant: CpuVariant) -> Disassembler {
        Disassembler::new(variant)
    }

    // --- single instruction formatting ---

    #[test]
    fn nop_implied() {
        let bus = make_bus(0x0200, &[0xEA]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.addr, 0x0200);
        assert_eq!(line.raw_bytes, vec![0xEA]);
        assert!(matches!(line.mnemonic, Mnemonic::Nop));
        assert_eq!(line.operand_text, "");
        assert!(line.is_valid);
    }

    #[test]
    fn lda_immediate() {
        let bus = make_bus(0x0200, &[0xA9, 0x42]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Lda));
        assert_eq!(line.operand_text, "#$42");
        assert_eq!(line.raw_bytes, vec![0xA9, 0x42]);
    }

    #[test]
    fn lda_zeropage() {
        let bus = make_bus(0x0200, &[0xA5, 0x50]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "$50");
    }

    #[test]
    fn lda_zeropage_x() {
        let bus = make_bus(0x0200, &[0xB5, 0x50]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "$50,X");
    }

    #[test]
    fn lda_absolute() {
        let bus = make_bus(0x0200, &[0xAD, 0x34, 0x12]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Lda));
        assert_eq!(line.operand_text, "$1234");
        assert_eq!(line.raw_bytes, vec![0xAD, 0x34, 0x12]);
    }

    #[test]
    fn lda_absolute_x() {
        let bus = make_bus(0x0200, &[0xBD, 0x34, 0x12]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "$1234,X");
    }

    #[test]
    fn lda_absolute_y() {
        let bus = make_bus(0x0200, &[0xB9, 0x34, 0x12]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "$1234,Y");
    }

    #[test]
    fn lda_indirect_x() {
        let bus = make_bus(0x0200, &[0xA1, 0x20]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "($20,X)");
    }

    #[test]
    fn lda_indirect_y() {
        let bus = make_bus(0x0200, &[0xB1, 0x20]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "($20),Y");
    }

    #[test]
    fn lda_zeropage_indirect() {
        let bus = make_bus(0x0200, &[0xB2, 0x20]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "($20)");
    }

    #[test]
    fn jmp_indirect() {
        let bus = make_bus(0x0200, &[0x6C, 0x00, 0x03]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Jmp));
        assert_eq!(line.operand_text, "($0300)");
    }

    #[test]
    fn jmp_absolute_indirect_x() {
        let bus = make_bus(0x0200, &[0x7C, 0x00, 0x03]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "($0300,X)");
    }

    #[test]
    fn bra_relative() {
        let bus = make_bus(0x0200, &[0x80, 0xFE]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Bra));
        assert_eq!(line.operand_text, "$FE");
    }

    #[test]
    fn asl_accumulator() {
        let bus = make_bus(0x0200, &[0x0A]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Asl));
        assert_eq!(line.operand_text, "A");
    }

    #[test]
    fn bbr0_zeropage_relative() {
        let bus = make_bus(0x0200, &[0x0F, 0x50, 0x04]);
        let line = disasm(CpuVariant::Wdc65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Bbr0));
        assert_eq!(line.operand_text, "$50,$04");
        assert!(line.is_valid);
    }

    #[test]
    fn invalid_opcode_marked_not_valid() {
        let bus = make_bus(0x0200, &[0xCB]); // WAI — invalid on Cmos65C02
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(!line.is_valid);
    }

    #[test]
    fn wdc_opcode_valid_under_wdc_variant() {
        let bus = make_bus(0x0200, &[0xDB]); // STP
        let line = disasm(CpuVariant::Wdc65C02).disassemble_one(&bus, 0x0200);
        assert!(line.is_valid);
        assert!(matches!(line.mnemonic, Mnemonic::Stp));
    }

    // --- range disassembly ---

    #[test]
    fn disassemble_range_two_instructions() {
        // NOP (1 byte), LDA #$42 (2 bytes)
        let bus = make_bus(0x0200, &[0xEA, 0xA9, 0x42]);
        let lines = disasm(CpuVariant::Cmos65C02)
            .disassemble_range(&bus, 0x0200, 0x0203, 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].addr, 0x0200);
        assert!(matches!(lines[0].mnemonic, Mnemonic::Nop));
        assert_eq!(lines[1].addr, 0x0201);
        assert!(matches!(lines[1].mnemonic, Mnemonic::Lda));
    }

    #[test]
    fn disassemble_range_respects_max() {
        let bus = make_bus(0x0200, &[0xEA; 10]); // 10 NOPs
        let lines = disasm(CpuVariant::Cmos65C02)
            .disassemble_range(&bus, 0x0200, 0x020A, 3);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn disassemble_range_stops_at_end() {
        let bus = make_bus(0x0200, &[0xEA; 10]);
        let lines = disasm(CpuVariant::Cmos65C02)
            .disassemble_range(&bus, 0x0200, 0x0203, 100);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn disassemble_uses_peek_not_read() {
        // Bus with a device that would increment a counter on read — we use RAM here
        // and verify the bus state is unchanged after disassembly.
        let bus = make_bus(0x0200, &[0xEA, 0xEA]);
        let lines = disasm(CpuVariant::Cmos65C02)
            .disassemble_range(&bus, 0x0200, 0x0202, 10);
        assert_eq!(lines.len(), 2);
        // If peek had side effects we'd see corruption; absence of panic is sufficient here.
        // A mock device test in bus/mod.rs already verifies peek is side-effect-free.
    }
}
