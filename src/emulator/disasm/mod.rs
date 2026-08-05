//! Disassembler: decodes bus memory into human-readable instruction listings.

use crate::emulator::bus::{Bus, SymbolTable};
use crate::emulator::cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic, decode_table};
use crate::emulator::cpu::variant::CpuVariant;

/// A single disassembled instruction.
pub struct DisassembledLine {
    /// Address of the first byte of the instruction.
    pub addr: u16,
    /// The raw instruction bytes (1–3).
    pub raw_bytes: Vec<u8>,
    /// Labels associated with `addr` from the symbol table associated with the [`Bus`].
    pub labels: Vec<String>,
    /// The instruction mnemonic.
    pub mnemonic: Mnemonic,
    /// Formatted operand text (empty string for implied/accumulator).
    pub operand_text: String,
    /// An optional comment field (used to provide additional details for immediate operands)
    pub comment_text: Option<String>,
    /// False when the opcode is invalid for the active variant.
    pub is_valid: bool,
}

/// Disassembles instructions from a `Bus` using side-effect-free `peek` reads.
pub struct Disassembler {
    table: [DecodedOp; 256],
}

impl Disassembler {

    const ASCII_CTRL_MNEMONICS: &str = "NULSOHSTXETXEOTENQACKBELBS HT LF VT FF CR SO SI \
                                        DLEDC1DC2DC3DC4NAKSYNETBCANEM SUBESCFS GS RS US ";

    fn ascii_ctrl_mnemonic(ctrl: u8) -> &'static str {
        let i = (3 * ctrl) as usize;
        &Self::ASCII_CTRL_MNEMONICS[i..i + 3]
    }

    fn immediate_mode_comment(operand: u8) -> String {
        if operand < 0x20 {
            format!("; {} ^{} ({})", operand, (operand + b'@') as char,
                    Self::ascii_ctrl_mnemonic(operand).trim())
        } else if operand == 0x20 {
            "; 32 ' ' (SPC)".to_string()
        } else if operand < 0x7F {
            format!("; {} '{}'", operand, operand as char)
        } else if operand == 0x7F {
            "; 127 (DEL)".to_string()
        } else {
            format!("; {} ({})", operand, operand as i8)
        }
    }

    /// Creates a disassembler for the given CPU variant.
    pub fn new(variant: CpuVariant) -> Self {
        Self { table: decode_table(variant) }
    }

    /// Peeks the opcode and operand bytes for the instruction at `addr` off `bus`.
    fn collect_bytes(&self, bus: &Bus, addr: u16) -> Vec<u8> {
        let opcode = bus.peek(addr).unwrap_or(0xFF);
        let decoded = self.table[opcode as usize];
        let mut raw_bytes = vec![opcode];
        for i in 1..decoded.byte_len {
            raw_bytes.push(bus.peek(addr.wrapping_add(i as u16)).unwrap_or(0xFF));
        }
        raw_bytes
    }

    /// Builds a `DisassembledLine` from already-collected instruction bytes.
    /// Pure: no bus access, only the opcode table and `symbol_table`.
    pub(crate) fn build_line(&self, addr: u16, raw_bytes: Vec<u8>, symbol_table: &SymbolTable) -> DisassembledLine {
        let decoded = self.table[raw_bytes[0] as usize];
        let names: Vec<&str> = symbol_table.names_for(addr).collect();
        let labels: Vec<String> = names.iter().map(|&s| s.to_string()).collect();
        let operand_text = format_operand(&decoded, &raw_bytes, addr, symbol_table);
        let comment_text = if decoded.mode == AddressingMode::Immediate {
            Some(Self::immediate_mode_comment(raw_bytes[1]))
        } else {
            None
        };
        DisassembledLine {
            addr,
            raw_bytes,
            labels,
            mnemonic: decoded.mnemonic,
            operand_text,
            comment_text,
            is_valid: decoded.is_valid,
        }
    }

    /// Disassembles a single instruction at `addr`.
    pub fn disassemble_one(&self, bus: &Bus, addr: u16) -> DisassembledLine {
        let raw_bytes = self.collect_bytes(bus, addr);
        self.build_line(addr, raw_bytes, bus.symbol_table())
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

fn format_operand(decoded: &DecodedOp, raw: &[u8], addr: u16, symbol_table: &SymbolTable) -> String {
    let b1 = raw.get(1).copied().unwrap_or(0);
    let b2 = raw.get(2).copied().unwrap_or(0);

    // Branch target is relative to the PC immediately after the instruction,
    // matching the CPU's own branch/bbr/bbs execution (see cpu/mod.rs).
    let pc_after = addr.wrapping_add(decoded.byte_len as u16);

    // Get the absolute address specified by the operand bytes
    let abs = absolute_address(b1, b2, pc_after, decoded.mode);

    match decoded.mode {
        AddressingMode::Implied =>
            String::new(),
        AddressingMode::Accumulator =>
            "A".to_string(),
        AddressingMode::Immediate =>
            format!("#${b1:02X}"),
        AddressingMode::ZeroPage |
        AddressingMode::Absolute |
        AddressingMode::Relative =>
            format_address(abs, decoded.mode, symbol_table),
        AddressingMode::ZeroPageX |
        AddressingMode::AbsoluteX =>
            format!("{},X", format_address(abs, decoded.mode, symbol_table)),
        AddressingMode::ZeroPageY |
        AddressingMode::AbsoluteY =>
            format!("{},Y", format_address(abs, decoded.mode, symbol_table)),
        AddressingMode::Indirect |
        AddressingMode::ZeroPageIndirect =>
            format!("({})", format_address(abs, decoded.mode, symbol_table)),
        AddressingMode::IndirectX |
        AddressingMode::AbsoluteIndirectX =>
            format!("({},X)", format_address(abs, decoded.mode, symbol_table)),
        AddressingMode::IndirectY =>
            format!("({}),Y", format_address(abs, decoded.mode, symbol_table)),
        AddressingMode::ZeroPageRelative =>
            format!("{},{}", format_address(b1 as u16, AddressingMode::ZeroPage, symbol_table),
                    format_address(abs, decoded.mode, symbol_table)),
    }
}

fn format_address(address: u16, mode: AddressingMode, symbol_table: &SymbolTable) -> String {
    let names: Vec<&str> = symbol_table.names_for(address).collect();
    if !names.is_empty() {
        names[0].to_string()
    } else {
        match mode {
            AddressingMode::ZeroPage |
            AddressingMode::ZeroPageX |
            AddressingMode::ZeroPageY |
            AddressingMode::IndirectX |
            AddressingMode::IndirectY |
            AddressingMode::ZeroPageIndirect => format!("${address:02X}"),
            AddressingMode::Absolute |
            AddressingMode::AbsoluteX |
            AddressingMode::AbsoluteY |
            AddressingMode::Indirect |
            AddressingMode::Relative |
            AddressingMode::ZeroPageRelative |
            AddressingMode::AbsoluteIndirectX => format!("${address:04X}"),
            _ => panic!("no address format for mode {:?}", mode),
        }
    }
}

fn absolute_address(b1: u8, b2: u8, pc_after: u16, mode: AddressingMode) -> u16 {
    if mode == AddressingMode::Relative {
        pc_after.wrapping_add(b1 as i8 as u16)
    } else if mode == AddressingMode::ZeroPageRelative {
        pc_after.wrapping_add(b2 as i8 as u16)
    } else {
        u16::from_le_bytes([b1, b2])
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::bus::AddressRange;
    use crate::emulator::bus::{Bus, BusConfig};

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

    #[test]
    fn ascii_ctrl_mnemonics() {
        assert_eq!(Disassembler::ascii_ctrl_mnemonic(0), "NUL");
        assert_eq!(Disassembler::ascii_ctrl_mnemonic(0x7), "BEL");
        assert_eq!(Disassembler::ascii_ctrl_mnemonic(0xD), "CR ");
        assert_eq!(Disassembler::ascii_ctrl_mnemonic(0x15), "NAK");
        assert_eq!(Disassembler::ascii_ctrl_mnemonic(0x18), "CAN");
        assert_eq!(Disassembler::ascii_ctrl_mnemonic(0x1F), "US ");
    }

    #[test]
    fn immediate_mode_comment_ascii_ctrl() {
        assert_eq!(Disassembler::immediate_mode_comment(0), "; 0 ^@ (NUL)");
        assert_eq!(Disassembler::immediate_mode_comment(1), "; 1 ^A (SOH)");
        assert_eq!(Disassembler::immediate_mode_comment(0x1A), "; 26 ^Z (SUB)");
        assert_eq!(Disassembler::immediate_mode_comment(0x1B), "; 27 ^[ (ESC)");
    }

    #[test]
    fn collect_bytes_and_build_line_match_disassemble_one() {
        let bus = make_bus(0x0200, &[0xA9, 0x55]); // LDA #$55
        let d = disasm(CpuVariant::Wdc65C02);

        let combined = d.disassemble_one(&bus, 0x0200);

        let raw_bytes = d.collect_bytes(&bus, 0x0200);
        let split = d.build_line(0x0200, raw_bytes, bus.symbol_table());

        assert_eq!(split.addr, combined.addr);
        assert_eq!(split.raw_bytes, combined.raw_bytes);
        assert_eq!(split.labels, combined.labels);
        assert!(split.mnemonic == combined.mnemonic);
        assert_eq!(split.operand_text, combined.operand_text);
        assert_eq!(split.comment_text, combined.comment_text);
        assert_eq!(split.is_valid, combined.is_valid);
    }

    #[test]
    fn immediate_mode_comment_ascii_space() {
        assert_eq!(Disassembler::immediate_mode_comment(0x20), "; 32 ' ' (SPC)");
    }
        #[test]
    fn immediate_mode_comment_ascii_printable() {
        assert_eq!(Disassembler::immediate_mode_comment(0x40), "; 64 '@'");
        assert_eq!(Disassembler::immediate_mode_comment(0x41), "; 65 'A'");
        assert_eq!(Disassembler::immediate_mode_comment(0x7E), "; 126 '~'");
    }

    #[test]
    fn immediate_mode_comment_ascii_del() {
        assert_eq!(Disassembler::immediate_mode_comment(0x7F), "; 127 (DEL)");
    }

    #[test]
    fn immediate_mode_comment_not_ascii() {
        assert_eq!(Disassembler::immediate_mode_comment(0x80), "; 128 (-128)");
        assert_eq!(Disassembler::immediate_mode_comment(0xFF), "; 255 (-1)");
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
        assert_eq!(line.comment_text, Some("; 66 'B'".to_string()));
        assert_eq!(line.raw_bytes, vec![0xA9, 0x42]);
    }

    #[test]
    fn lda_zeropage() {
        let bus = make_bus(0x0200, &[0xA5, 0x50]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "$50");
    }

    #[test]
    fn lda_zeropage_symbol() {
        let mut bus = make_bus(0x0200, &[0xA5, 0x50]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x50);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "foo");
    }

    #[test]
    fn lda_zeropage_x() {
        let bus = make_bus(0x0200, &[0xB5, 0x50]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "$50,X");
    }

    #[test]
    fn lda_zeropage_x_symbol() {
        let mut bus = make_bus(0x0200, &[0xB5, 0x50]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x50);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "foo,X");
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
    fn lda_absolute_symbol() {
        let mut bus = make_bus(0x0200, &[0xAD, 0x34, 0x12]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x1234);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Lda));
        assert_eq!(line.operand_text, "foo");
        assert_eq!(line.raw_bytes, vec![0xAD, 0x34, 0x12]);
    }

    #[test]
    fn lda_absolute_x() {
        let bus = make_bus(0x0200, &[0xBD, 0x34, 0x12]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "$1234,X");
    }

    #[test]
    fn lda_absolute_x_symbol() {
        let mut bus = make_bus(0x0200, &[0xBD, 0x34, 0x12]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x1234);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "foo,X");
    }


    #[test]
    fn lda_absolute_y() {
        let bus = make_bus(0x0200, &[0xB9, 0x34, 0x12]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "$1234,Y");
    }

    #[test]
    fn lda_absolute_y_symbol() {
        let mut bus = make_bus(0x0200, &[0xB9, 0x34, 0x12]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x1234);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "foo,Y");
    }

    #[test]
    fn lda_indirect_x() {
        let bus = make_bus(0x0200, &[0xA1, 0x20]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "($20,X)");
    }

    #[test]
    fn lda_indirect_x_symbol() {
        let mut bus = make_bus(0x0200, &[0xA1, 0x20]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x20);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "(foo,X)");
    }

    #[test]
    fn lda_indirect_y() {
        let bus = make_bus(0x0200, &[0xB1, 0x20]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "($20),Y");
    }

    #[test]
    fn lda_indirect_symbol() {
        let mut bus = make_bus(0x0200, &[0xB1, 0x20]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x20);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "(foo),Y");
    }

    #[test]
    fn lda_zeropage_indirect() {
        let bus = make_bus(0x0200, &[0xB2, 0x20]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "($20)");
    }

    #[test]
    fn lda_zeropage_indirect_symbol() {
        let mut bus = make_bus(0x0200, &[0xB2, 0x20]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x20);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "(foo)");
    }

    #[test]
    fn jmp_indirect() {
        let bus = make_bus(0x0200, &[0x6C, 0x00, 0x03]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Jmp));
        assert_eq!(line.operand_text, "($0300)");
    }

    #[test]
    fn jmp_indirect_symbol() {
        let mut bus = make_bus(0x0200, &[0x6C, 0x00, 0x03]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x300);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Jmp));
        assert_eq!(line.operand_text, "(foo)");
    }

    #[test]
    fn jmp_absolute_indirect_x() {
        let bus = make_bus(0x0200, &[0x7C, 0x00, 0x03]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "($0300,X)");
    }

    #[test]
    fn jmp_absolute_indirect_x_symbol() {
        let mut bus = make_bus(0x0200, &[0x7C, 0x00, 0x03]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x300);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert_eq!(line.operand_text, "(foo,X)");
    }

    #[test]
    fn bra_relative_backward() {
        // Displacement -2, from PC 0x0202 (after the 2-byte instruction) => target 0x0200.
        let bus = make_bus(0x0200, &[0x80, 0xFE]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Bra));
        assert_eq!(line.operand_text, "$0200");
    }

    #[test]
    fn bra_relative_backward_symbol() {
        // Displacement -2, from PC 0x0202 (after the 2-byte instruction) => target 0x0200.
        let mut bus = make_bus(0x0200, &[0x80, 0xFE]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x200);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Bra));
        assert_eq!(line.operand_text, "foo");
    }

    #[test]
    fn beq_relative_forward() {
        // Displacement +4, from PC 0x0202 (after the 2-byte instruction) => target 0x0206.
        let bus = make_bus(0x0200, &[0xF0, 0x04]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Beq));
        assert_eq!(line.operand_text, "$0206");
    }

    #[test]
    fn beq_relative_forward_symbol() {
        // Displacement +4, from PC 0x0202 (after the 2-byte instruction) => target 0x0206.
        let mut bus = make_bus(0x0200, &[0xF0, 0x04]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x206);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Beq));
        assert_eq!(line.operand_text, "foo");
    }

    #[test]
    fn bra_relative_wraps_past_top_of_address_space() {
        // Displacement +2, from PC 0xFFFF (after the 2-byte instruction) => wraps to 0x0001.
        let bus = make_bus(0xFFFD, &[0x80, 0x02]);
        let line = disasm(CpuVariant::Cmos65C02).disassemble_one(&bus, 0xFFFD);
        assert!(matches!(line.mnemonic, Mnemonic::Bra));
        assert_eq!(line.operand_text, "$0001");
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
        // Displacement +4, from PC 0x0203 (after the 3-byte instruction) => target 0x0207.
        let bus = make_bus(0x0200, &[0x0F, 0x50, 0x04]);
        let line = disasm(CpuVariant::Wdc65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Bbr0));
        assert_eq!(line.operand_text, "$50,$0207");
        assert!(line.is_valid);
    }

    #[test]
    fn bbr0_zeropage_relative_symbol() {
        // Displacement +4, from PC 0x0203 (after the 3-byte instruction) => target 0x0207.
        let mut bus = make_bus(0x0200, &[0x0F, 0x50, 0x04]);
        let symbol_table = bus.symbol_table_mut();
        symbol_table.insert("bar".to_string(), 0x50);
        symbol_table.insert("foo".to_string(), 0x207);
        let line = disasm(CpuVariant::Wdc65C02).disassemble_one(&bus, 0x0200);
        assert!(matches!(line.mnemonic, Mnemonic::Bbr0));
        assert_eq!(line.operand_text, "bar,foo");
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

    #[test]
    fn disassemble_one_with_labels() {
        // NOP (1 byte), LDA #$42 (2 bytes)
        let mut bus = make_bus(0x0200, &[0xEA]);
        bus.symbol_table_mut().insert("foo".to_string(), 0x200);
        bus.symbol_table_mut().insert("bar".to_string(), 0x200);
        let line = disasm(CpuVariant::Cmos65C02)
            .disassemble_one(&bus, 0x0200);
        assert_eq!(line.addr, 0x0200);
        assert!(line.labels.contains(&"foo".to_string()));
        assert!(line.labels.contains(&"bar".to_string()));
        assert!(matches!(line.mnemonic, Mnemonic::Nop));
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
