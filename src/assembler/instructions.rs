//! Mnemonic name lookup and the reverse `(Mnemonic, AddressingMode) -> DecodedOp`
//! table used to pick opcodes while assembling.

use std::collections::HashMap;

use crate::emulator::cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic, decode_table};
use crate::emulator::cpu::variant::CpuVariant;

/// Maps an assembly-source mnemonic name to a [`Mnemonic`], case-insensitively.
///
/// The natural inverse of `Mnemonic`'s `Display` impl (`opcodes.rs`). `Ill` is
/// intentionally excluded — it is a placeholder for undecodable opcodes, not a
/// mnemonic a user can write.
pub fn mnemonic_from_str(name: &str) -> Option<Mnemonic> {
    use Mnemonic::*;
    let upper = name.to_ascii_uppercase();
    Some(match upper.as_str() {
        "ADC" => Adc, "AND" => And, "ASL" => Asl, "BBC" => Bbc,
        "BBR0" => Bbr0, "BBR1" => Bbr1, "BBR2" => Bbr2, "BBR3" => Bbr3,
        "BBR4" => Bbr4, "BBR5" => Bbr5, "BBR6" => Bbr6, "BBR7" => Bbr7,
        "BBS0" => Bbs0, "BBS1" => Bbs1, "BBS2" => Bbs2, "BBS3" => Bbs3,
        "BBS4" => Bbs4, "BBS5" => Bbs5, "BBS6" => Bbs6, "BBS7" => Bbs7,
        "BCC" => Bcc, "BCS" => Bcs, "BEQ" => Beq, "BIT" => Bit,
        "BMI" => Bmi, "BNE" => Bne, "BPL" => Bpl, "BRA" => Bra,
        "BRK" => Brk, "BVC" => Bvc, "BVS" => Bvs,
        "CLC" => Clc, "CLD" => Cld, "CLI" => Cli, "CLV" => Clv,
        "CMP" => Cmp, "CPX" => Cpx, "CPY" => Cpy,
        "DEC" => Dec, "DEX" => Dex, "DEY" => Dey, "EOR" => Eor,
        "INC" => Inc, "INX" => Inx, "INY" => Iny, "JMP" => Jmp, "JSR" => Jsr,
        "LDA" => Lda, "LDX" => Ldx, "LDY" => Ldy, "LSR" => Lsr,
        "NOP" => Nop, "ORA" => Ora,
        "PHA" => Pha, "PHP" => Php, "PHX" => Phx, "PHY" => Phy,
        "PLA" => Pla, "PLP" => Plp, "PLX" => Plx, "PLY" => Ply,
        "RMB0" => Rmb0, "RMB1" => Rmb1, "RMB2" => Rmb2, "RMB3" => Rmb3,
        "RMB4" => Rmb4, "RMB5" => Rmb5, "RMB6" => Rmb6, "RMB7" => Rmb7,
        "ROL" => Rol, "ROR" => Ror, "RTI" => Rti, "RTS" => Rts,
        "SBC" => Sbc, "SEC" => Sec, "SED" => Sed, "SEI" => Sei,
        "SMB0" => Smb0, "SMB1" => Smb1, "SMB2" => Smb2, "SMB3" => Smb3,
        "SMB4" => Smb4, "SMB5" => Smb5, "SMB6" => Smb6, "SMB7" => Smb7,
        "STA" => Sta, "STP" => Stp, "STX" => Stx, "STY" => Sty, "STZ" => Stz,
        "TAX" => Tax, "TAY" => Tay, "TRB" => Trb, "TSB" => Tsb,
        "TSX" => Tsx, "TXA" => Txa, "TXS" => Txs, "TYA" => Tya,
        "WAI" => Wai,
        _ => return None,
    })
}

/// The set of `(Mnemonic, AddressingMode) -> DecodedOp` entries valid for one
/// [`CpuVariant`], built once by inverting `decode_table`. This is the single
/// source of truth the assembler shares with the CPU decoder and disassembler
/// for which addressing modes a mnemonic supports and what opcode byte to emit.
pub struct InstructionTable {
    modes: HashMap<(Mnemonic, AddressingMode), DecodedOp>,
}

impl InstructionTable {
    pub fn new(variant: CpuVariant) -> Self {
        let mut modes: HashMap<(Mnemonic, AddressingMode), DecodedOp> = HashMap::new();
        for entry in decode_table(variant).into_iter().filter(|e| e.is_valid) {
            // Several illegal/undefined opcodes decode as `Nop` under the same
            // addressing mode as other illegal opcodes (e.g. 0x03, 0xEA, and
            // 0xFB are all `(Nop, Implied)`), so this key isn't always unique.
            // The canonical `NOP` (0xEA) always wins that collision; the
            // handful of remaining duplicate `Nop` opcode slots (multi-byte
            // NOPs standing in for other illegal opcodes) aren't meaningfully
            // distinguishable from source, so an arbitrary pick among them is
            // fine — first-wins, same as every other addressing mode.
            modes.entry((entry.mnemonic, entry.mode))
                .and_modify(|existing| if entry.opcode == 0xEA { *existing = entry })
                .or_insert(entry);
        }
        Self { modes }
    }

    /// Returns the decode table entry for `mnemonic` under `mode`, if that
    /// combination is valid for this table's variant.
    pub fn get(&self, mnemonic: Mnemonic, mode: AddressingMode) -> Option<&DecodedOp> {
        self.modes.get(&(mnemonic, mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonic_from_str_round_trips_display() {
        for mnemonic in [
            Mnemonic::Lda, Mnemonic::Sta, Mnemonic::Jmp, Mnemonic::Bbr0,
            Mnemonic::Smb7, Mnemonic::Wai, Mnemonic::Stp, Mnemonic::Nop,
        ] {
            let text = format!("{mnemonic}");
            assert_eq!(mnemonic_from_str(&text), Some(mnemonic));
        }
    }

    #[test]
    fn mnemonic_from_str_is_case_insensitive() {
        assert_eq!(mnemonic_from_str("lda"), Some(Mnemonic::Lda));
        assert_eq!(mnemonic_from_str("Lda"), Some(Mnemonic::Lda));
        assert_eq!(mnemonic_from_str("LDA"), Some(Mnemonic::Lda));
    }

    #[test]
    fn mnemonic_from_str_rejects_unknown_and_ill() {
        assert_eq!(mnemonic_from_str("FOO"), None);
        assert_eq!(mnemonic_from_str("<ILL>"), None);
        assert_eq!(mnemonic_from_str(""), None);
    }

    #[test]
    fn instruction_table_looks_up_known_opcode() {
        let table = InstructionTable::new(CpuVariant::Cmos65C02);
        let entry = table.get(Mnemonic::Lda, AddressingMode::Immediate).unwrap();
        assert_eq!(entry.opcode, 0xA9);
        let entry = table.get(Mnemonic::Lda, AddressingMode::ZeroPage).unwrap();
        assert_eq!(entry.opcode, 0xA5);
        let entry = table.get(Mnemonic::Lda, AddressingMode::Absolute).unwrap();
        assert_eq!(entry.opcode, 0xAD);
    }

    #[test]
    fn instruction_table_missing_combination_is_none() {
        let table = InstructionTable::new(CpuVariant::Cmos65C02);
        // LDA has no Accumulator-mode opcode.
        assert!(table.get(Mnemonic::Lda, AddressingMode::Accumulator).is_none());
    }

    #[test]
    fn instruction_table_respects_variant_for_wdc_only_opcodes() {
        let cmos = InstructionTable::new(CpuVariant::Cmos65C02);
        let wdc = InstructionTable::new(CpuVariant::Wdc65C02);
        assert!(cmos.get(Mnemonic::Stp, AddressingMode::Implied).is_none());
        assert!(wdc.get(Mnemonic::Stp, AddressingMode::Implied).is_some());
        assert!(cmos.get(Mnemonic::Bbr0, AddressingMode::ZeroPageRelative).is_none());
        assert!(wdc.get(Mnemonic::Bbr0, AddressingMode::ZeroPageRelative).is_some());
    }

    #[test]
    fn instruction_table_cmos_additions_present_under_both_variants() {
        for variant in [CpuVariant::Cmos65C02, CpuVariant::Wdc65C02] {
            let table = InstructionTable::new(variant);
            assert!(table.get(Mnemonic::Bra, AddressingMode::Relative).is_some());
            assert!(table.get(Mnemonic::Stz, AddressingMode::ZeroPage).is_some());
        }
    }
}
