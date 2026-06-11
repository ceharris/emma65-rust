use crate::emulator::cpu::variant::CpuVariant;

/// All instruction mnemonics for the 65C02 family, including WDC-only additions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mnemonic {
    Adc, And, Asl, Bbc, Bbr0, Bbr1, Bbr2, Bbr3, Bbr4, Bbr5, Bbr6, Bbr7,
    Bbs0, Bbs1, Bbs2, Bbs3, Bbs4, Bbs5, Bbs6, Bbs7,
    Bcc, Bcs, Beq, Bit, Bmi, Bne, Bpl, Bra, Brk, Bvc, Bvs,
    Clc, Cld, Cli, Clv, Cmp, Cpx, Cpy,
    Dec, Dex, Dey, Eor,
    Inc, Inx, Iny, Jmp, Jsr,
    Lda, Ldx, Ldy, Lsr,
    Nop, Ora, Pha, Php, Phx, Phy, Pla, Plp, Plx, Ply,
    Rmb0, Rmb1, Rmb2, Rmb3, Rmb4, Rmb5, Rmb6, Rmb7,
    Rol, Ror, Rti, Rts,
    Sbc, Sec, Sed, Sei,
    Smb0, Smb1, Smb2, Smb3, Smb4, Smb5, Smb6, Smb7,
    Sta, Stp, Stx, Sty, Stz,
    Tax, Tay, Trb, Tsb, Tsx, Txa, Txs, Tya,
    Wai,
    /// Placeholder for truly undefined/illegal opcodes.
    Ill,
}

/// All addressing modes supported by the 65C02 family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressingMode {
    Implied,
    Accumulator,
    Immediate,
    ZeroPage,
    ZeroPageX,
    ZeroPageY,
    Absolute,
    AbsoluteX,
    AbsoluteY,
    Indirect,
    IndirectX,
    IndirectY,
    ZeroPageIndirect,
    AbsoluteIndirectX,
    Relative,
    ZeroPageRelative,
}

/// A decoded instruction entry from the 256-entry table.
#[derive(Debug, Clone, Copy)]
pub struct DecodedOp {
    /// The raw opcode byte (0x00–0xFF).
    pub opcode: u8,
    /// The instruction mnemonic.
    pub mnemonic: Mnemonic,
    /// The addressing mode used by this opcode.
    pub mode: AddressingMode,
    /// Total instruction length in bytes (1, 2, or 3).
    pub byte_len: u8,
    /// Base cycle count before page-crossing penalties.
    pub base_cycles: u8,
    /// False for opcodes that are invalid in the active `CpuVariant` (treated per policy).
    pub is_valid: bool,
}

impl DecodedOp {
    const fn new(
        opcode: u8,
        mnemonic: Mnemonic,
        mode: AddressingMode,
        byte_len: u8,
        base_cycles: u8,
    ) -> Self {
        Self { opcode, mnemonic, mode, byte_len, base_cycles, is_valid: true }
    }

    const fn wdc_only(
        opcode: u8,
        mnemonic: Mnemonic,
        mode: AddressingMode,
        byte_len: u8,
        base_cycles: u8,
    ) -> Self {
        Self { opcode, mnemonic, mode, byte_len, base_cycles, is_valid: false }
    }
}

use Mnemonic::*;
use AddressingMode::*;

/// Returns the decode table for the given variant. WDC-only opcodes have `is_valid = false`
/// under `Cmos65C02`.
pub fn decode_table(variant: CpuVariant) -> [DecodedOp; 256] {
    let mut table = base_table();
    if variant == CpuVariant::Wdc65C02 {
        for entry in table.iter_mut() {
            if matches!(entry.mnemonic,
                Stp | Wai |
                Bbr0 | Bbr1 | Bbr2 | Bbr3 | Bbr4 | Bbr5 | Bbr6 | Bbr7 |
                Bbs0 | Bbs1 | Bbs2 | Bbs3 | Bbs4 | Bbs5 | Bbs6 | Bbs7 |
                Rmb0 | Rmb1 | Rmb2 | Rmb3 | Rmb4 | Rmb5 | Rmb6 | Rmb7 |
                Smb0 | Smb1 | Smb2 | Smb3 | Smb4 | Smb5 | Smb6 | Smb7
            ) {
                entry.is_valid = true;
            }
        }
    }
    table
}

/// Returns the base decode table with WDC-only opcodes marked `is_valid = false`.
const fn base_table() -> [DecodedOp; 256] {
    // Initialize all slots to ILL / Implied / 1 byte / 2 cycles / invalid
    let ill = DecodedOp { opcode: 0, mnemonic: Ill, mode: Implied, byte_len: 1, base_cycles: 2, is_valid: false };
    let mut t = [ill; 256];

    // Macro-free: list every opcode explicitly.
    // Format: opcode, mnemonic, mode, bytes, cycles

    // 0x00–0x0F
    t[0x00] = DecodedOp::new(0x00, Brk, Implied,          2, 7);
    t[0x01] = DecodedOp::new(0x01, Ora, IndirectX,         2, 6);
    t[0x04] = DecodedOp::new(0x04, Tsb, ZeroPage,          2, 5);
    t[0x05] = DecodedOp::new(0x05, Ora, ZeroPage,          2, 3);
    t[0x06] = DecodedOp::new(0x06, Asl, ZeroPage,          2, 5);
    t[0x07] = DecodedOp::wdc_only(0x07, Rmb0, ZeroPage,    2, 5);
    t[0x08] = DecodedOp::new(0x08, Php, Implied,           1, 3);
    t[0x09] = DecodedOp::new(0x09, Ora, Immediate,         2, 2);
    t[0x0A] = DecodedOp::new(0x0A, Asl, Accumulator,       1, 2);
    t[0x0C] = DecodedOp::new(0x0C, Tsb, Absolute,          3, 6);
    t[0x0D] = DecodedOp::new(0x0D, Ora, Absolute,          3, 4);
    t[0x0E] = DecodedOp::new(0x0E, Asl, Absolute,          3, 6);
    t[0x0F] = DecodedOp::wdc_only(0x0F, Bbr0, ZeroPageRelative, 3, 5);

    // 0x10–0x1F
    t[0x10] = DecodedOp::new(0x10, Bpl, Relative,          2, 2);
    t[0x11] = DecodedOp::new(0x11, Ora, IndirectY,         2, 5);
    t[0x12] = DecodedOp::new(0x12, Ora, ZeroPageIndirect,  2, 5);
    t[0x14] = DecodedOp::new(0x14, Trb, ZeroPage,          2, 5);
    t[0x15] = DecodedOp::new(0x15, Ora, ZeroPageX,         2, 4);
    t[0x16] = DecodedOp::new(0x16, Asl, ZeroPageX,         2, 6);
    t[0x17] = DecodedOp::wdc_only(0x17, Rmb1, ZeroPage,    2, 5);
    t[0x18] = DecodedOp::new(0x18, Clc, Implied,           1, 2);
    t[0x19] = DecodedOp::new(0x19, Ora, AbsoluteY,         3, 4);
    t[0x1A] = DecodedOp::new(0x1A, Inc, Accumulator,       1, 2);
    t[0x1C] = DecodedOp::new(0x1C, Trb, Absolute,          3, 6);
    t[0x1D] = DecodedOp::new(0x1D, Ora, AbsoluteX,         3, 4);
    t[0x1E] = DecodedOp::new(0x1E, Asl, AbsoluteX,         3, 7);
    t[0x1F] = DecodedOp::wdc_only(0x1F, Bbr1, ZeroPageRelative, 3, 5);

    // 0x20–0x2F
    t[0x20] = DecodedOp::new(0x20, Jsr, Absolute,          3, 6);
    t[0x21] = DecodedOp::new(0x21, And, IndirectX,         2, 6);
    t[0x24] = DecodedOp::new(0x24, Bit, ZeroPage,          2, 3);
    t[0x25] = DecodedOp::new(0x25, And, ZeroPage,          2, 3);
    t[0x26] = DecodedOp::new(0x26, Rol, ZeroPage,          2, 5);
    t[0x27] = DecodedOp::wdc_only(0x27, Rmb2, ZeroPage,    2, 5);
    t[0x28] = DecodedOp::new(0x28, Plp, Implied,           1, 4);
    t[0x29] = DecodedOp::new(0x29, And, Immediate,         2, 2);
    t[0x2A] = DecodedOp::new(0x2A, Rol, Accumulator,       1, 2);
    t[0x2C] = DecodedOp::new(0x2C, Bit, Absolute,          3, 4);
    t[0x2D] = DecodedOp::new(0x2D, And, Absolute,          3, 4);
    t[0x2E] = DecodedOp::new(0x2E, Rol, Absolute,          3, 6);
    t[0x2F] = DecodedOp::wdc_only(0x2F, Bbr2, ZeroPageRelative, 3, 5);

    // 0x30–0x3F
    t[0x30] = DecodedOp::new(0x30, Bmi, Relative,          2, 2);
    t[0x31] = DecodedOp::new(0x31, And, IndirectY,         2, 5);
    t[0x32] = DecodedOp::new(0x32, And, ZeroPageIndirect,  2, 5);
    t[0x34] = DecodedOp::new(0x34, Bit, ZeroPageX,         2, 4);
    t[0x35] = DecodedOp::new(0x35, And, ZeroPageX,         2, 4);
    t[0x36] = DecodedOp::new(0x36, Rol, ZeroPageX,         2, 6);
    t[0x37] = DecodedOp::wdc_only(0x37, Rmb3, ZeroPage,    2, 5);
    t[0x38] = DecodedOp::new(0x38, Sec, Implied,           1, 2);
    t[0x39] = DecodedOp::new(0x39, And, AbsoluteY,         3, 4);
    t[0x3A] = DecodedOp::new(0x3A, Dec, Accumulator,       1, 2);
    t[0x3C] = DecodedOp::new(0x3C, Bit, AbsoluteX,         3, 4);
    t[0x3D] = DecodedOp::new(0x3D, And, AbsoluteX,         3, 4);
    t[0x3E] = DecodedOp::new(0x3E, Rol, AbsoluteX,         3, 7);
    t[0x3F] = DecodedOp::wdc_only(0x3F, Bbr3, ZeroPageRelative, 3, 5);

    // 0x40–0x4F
    t[0x40] = DecodedOp::new(0x40, Rti, Implied,           1, 6);
    t[0x41] = DecodedOp::new(0x41, Eor, IndirectX,         2, 6);
    t[0x45] = DecodedOp::new(0x45, Eor, ZeroPage,          2, 3);
    t[0x46] = DecodedOp::new(0x46, Lsr, ZeroPage,          2, 5);
    t[0x47] = DecodedOp::wdc_only(0x47, Rmb4, ZeroPage,    2, 5);
    t[0x48] = DecodedOp::new(0x48, Pha, Implied,           1, 3);
    t[0x49] = DecodedOp::new(0x49, Eor, Immediate,         2, 2);
    t[0x4A] = DecodedOp::new(0x4A, Lsr, Accumulator,       1, 2);
    t[0x4C] = DecodedOp::new(0x4C, Jmp, Absolute,          3, 3);
    t[0x4D] = DecodedOp::new(0x4D, Eor, Absolute,          3, 4);
    t[0x4E] = DecodedOp::new(0x4E, Lsr, Absolute,          3, 6);
    t[0x4F] = DecodedOp::wdc_only(0x4F, Bbr4, ZeroPageRelative, 3, 5);

    // 0x50–0x5F
    t[0x50] = DecodedOp::new(0x50, Bvc, Relative,          2, 2);
    t[0x51] = DecodedOp::new(0x51, Eor, IndirectY,         2, 5);
    t[0x52] = DecodedOp::new(0x52, Eor, ZeroPageIndirect,  2, 5);
    t[0x55] = DecodedOp::new(0x55, Eor, ZeroPageX,         2, 4);
    t[0x56] = DecodedOp::new(0x56, Lsr, ZeroPageX,         2, 6);
    t[0x57] = DecodedOp::wdc_only(0x57, Rmb5, ZeroPage,    2, 5);
    t[0x58] = DecodedOp::new(0x58, Cli, Implied,           1, 2);
    t[0x59] = DecodedOp::new(0x59, Eor, AbsoluteY,         3, 4);
    t[0x5A] = DecodedOp::new(0x5A, Phy, Implied,           1, 3);
    t[0x5D] = DecodedOp::new(0x5D, Eor, AbsoluteX,         3, 4);
    t[0x5E] = DecodedOp::new(0x5E, Lsr, AbsoluteX,         3, 7);
    t[0x5F] = DecodedOp::wdc_only(0x5F, Bbr5, ZeroPageRelative, 3, 5);

    // 0x60–0x6F
    t[0x60] = DecodedOp::new(0x60, Rts, Implied,           1, 6);
    t[0x61] = DecodedOp::new(0x61, Adc, IndirectX,         2, 6);
    t[0x64] = DecodedOp::new(0x64, Stz, ZeroPage,          2, 3);
    t[0x65] = DecodedOp::new(0x65, Adc, ZeroPage,          2, 3);
    t[0x66] = DecodedOp::new(0x66, Ror, ZeroPage,          2, 5);
    t[0x67] = DecodedOp::wdc_only(0x67, Rmb6, ZeroPage,    2, 5);
    t[0x68] = DecodedOp::new(0x68, Pla, Implied,           1, 4);
    t[0x69] = DecodedOp::new(0x69, Adc, Immediate,         2, 2);
    t[0x6A] = DecodedOp::new(0x6A, Ror, Accumulator,       1, 2);
    t[0x6C] = DecodedOp::new(0x6C, Jmp, Indirect,          3, 6);
    t[0x6D] = DecodedOp::new(0x6D, Adc, Absolute,          3, 4);
    t[0x6E] = DecodedOp::new(0x6E, Ror, Absolute,          3, 6);
    t[0x6F] = DecodedOp::wdc_only(0x6F, Bbr6, ZeroPageRelative, 3, 5);

    // 0x70–0x7F
    t[0x70] = DecodedOp::new(0x70, Bvs, Relative,          2, 2);
    t[0x71] = DecodedOp::new(0x71, Adc, IndirectY,         2, 5);
    t[0x72] = DecodedOp::new(0x72, Adc, ZeroPageIndirect,  2, 5);
    t[0x74] = DecodedOp::new(0x74, Stz, ZeroPageX,         2, 4);
    t[0x75] = DecodedOp::new(0x75, Adc, ZeroPageX,         2, 4);
    t[0x76] = DecodedOp::new(0x76, Ror, ZeroPageX,         2, 6);
    t[0x77] = DecodedOp::wdc_only(0x77, Rmb7, ZeroPage,    2, 5);
    t[0x78] = DecodedOp::new(0x78, Sei, Implied,           1, 2);
    t[0x79] = DecodedOp::new(0x79, Adc, AbsoluteY,         3, 4);
    t[0x7A] = DecodedOp::new(0x7A, Ply, Implied,           1, 4);
    t[0x7C] = DecodedOp::new(0x7C, Jmp, AbsoluteIndirectX, 3, 6);
    t[0x7D] = DecodedOp::new(0x7D, Adc, AbsoluteX,         3, 4);
    t[0x7E] = DecodedOp::new(0x7E, Ror, AbsoluteX,         3, 7);
    t[0x7F] = DecodedOp::wdc_only(0x7F, Bbr7, ZeroPageRelative, 3, 5);

    // 0x80–0x8F
    t[0x80] = DecodedOp::new(0x80, Bra, Relative,          2, 3);
    t[0x81] = DecodedOp::new(0x81, Sta, IndirectX,         2, 6);
    t[0x84] = DecodedOp::new(0x84, Sty, ZeroPage,          2, 3);
    t[0x85] = DecodedOp::new(0x85, Sta, ZeroPage,          2, 3);
    t[0x86] = DecodedOp::new(0x86, Stx, ZeroPage,          2, 3);
    t[0x87] = DecodedOp::wdc_only(0x87, Smb0, ZeroPage,    2, 5);
    t[0x88] = DecodedOp::new(0x88, Dey, Implied,           1, 2);
    t[0x89] = DecodedOp::new(0x89, Bit, Immediate,         2, 2);
    t[0x8A] = DecodedOp::new(0x8A, Txa, Implied,           1, 2);
    t[0x8C] = DecodedOp::new(0x8C, Sty, Absolute,          3, 4);
    t[0x8D] = DecodedOp::new(0x8D, Sta, Absolute,          3, 4);
    t[0x8E] = DecodedOp::new(0x8E, Stx, Absolute,          3, 4);
    t[0x8F] = DecodedOp::wdc_only(0x8F, Bbs0, ZeroPageRelative, 3, 5);

    // 0x90–0x9F
    t[0x90] = DecodedOp::new(0x90, Bcc, Relative,          2, 2);
    t[0x91] = DecodedOp::new(0x91, Sta, IndirectY,         2, 6);
    t[0x92] = DecodedOp::new(0x92, Sta, ZeroPageIndirect,  2, 5);
    t[0x94] = DecodedOp::new(0x94, Sty, ZeroPageX,         2, 4);
    t[0x95] = DecodedOp::new(0x95, Sta, ZeroPageX,         2, 4);
    t[0x96] = DecodedOp::new(0x96, Stx, ZeroPageY,         2, 4);
    t[0x97] = DecodedOp::wdc_only(0x97, Smb1, ZeroPage,    2, 5);
    t[0x98] = DecodedOp::new(0x98, Tya, Implied,           1, 2);
    t[0x99] = DecodedOp::new(0x99, Sta, AbsoluteY,         3, 5);
    t[0x9A] = DecodedOp::new(0x9A, Txs, Implied,           1, 2);
    t[0x9C] = DecodedOp::new(0x9C, Stz, Absolute,          3, 4);
    t[0x9D] = DecodedOp::new(0x9D, Sta, AbsoluteX,         3, 5);
    t[0x9E] = DecodedOp::new(0x9E, Stz, AbsoluteX,         3, 5);
    t[0x9F] = DecodedOp::wdc_only(0x9F, Bbs1, ZeroPageRelative, 3, 5);

    // 0xA0–0xAF
    t[0xA0] = DecodedOp::new(0xA0, Ldy, Immediate,         2, 2);
    t[0xA1] = DecodedOp::new(0xA1, Lda, IndirectX,         2, 6);
    t[0xA2] = DecodedOp::new(0xA2, Ldx, Immediate,         2, 2);
    t[0xA4] = DecodedOp::new(0xA4, Ldy, ZeroPage,          2, 3);
    t[0xA5] = DecodedOp::new(0xA5, Lda, ZeroPage,          2, 3);
    t[0xA6] = DecodedOp::new(0xA6, Ldx, ZeroPage,          2, 3);
    t[0xA7] = DecodedOp::wdc_only(0xA7, Smb2, ZeroPage,    2, 5);
    t[0xA8] = DecodedOp::new(0xA8, Tay, Implied,           1, 2);
    t[0xA9] = DecodedOp::new(0xA9, Lda, Immediate,         2, 2);
    t[0xAA] = DecodedOp::new(0xAA, Tax, Implied,           1, 2);
    t[0xAC] = DecodedOp::new(0xAC, Ldy, Absolute,          3, 4);
    t[0xAD] = DecodedOp::new(0xAD, Lda, Absolute,          3, 4);
    t[0xAE] = DecodedOp::new(0xAE, Ldx, Absolute,          3, 4);
    t[0xAF] = DecodedOp::wdc_only(0xAF, Bbs2, ZeroPageRelative, 3, 5);

    // 0xB0–0xBF
    t[0xB0] = DecodedOp::new(0xB0, Bcs, Relative,          2, 2);
    t[0xB1] = DecodedOp::new(0xB1, Lda, IndirectY,         2, 5);
    t[0xB2] = DecodedOp::new(0xB2, Lda, ZeroPageIndirect,  2, 5);
    t[0xB4] = DecodedOp::new(0xB4, Ldy, ZeroPageX,         2, 4);
    t[0xB5] = DecodedOp::new(0xB5, Lda, ZeroPageX,         2, 4);
    t[0xB6] = DecodedOp::new(0xB6, Ldx, ZeroPageY,         2, 4);
    t[0xB7] = DecodedOp::wdc_only(0xB7, Smb3, ZeroPage,    2, 5);
    t[0xB8] = DecodedOp::new(0xB8, Clv, Implied,           1, 2);
    t[0xB9] = DecodedOp::new(0xB9, Lda, AbsoluteY,         3, 4);
    t[0xBA] = DecodedOp::new(0xBA, Tsx, Implied,           1, 2);
    t[0xBC] = DecodedOp::new(0xBC, Ldy, AbsoluteX,         3, 4);
    t[0xBD] = DecodedOp::new(0xBD, Lda, AbsoluteX,         3, 4);
    t[0xBE] = DecodedOp::new(0xBE, Ldx, AbsoluteY,         3, 4);
    t[0xBF] = DecodedOp::wdc_only(0xBF, Bbs3, ZeroPageRelative, 3, 5);

    // 0xC0–0xCF
    t[0xC0] = DecodedOp::new(0xC0, Cpy, Immediate,         2, 2);
    t[0xC1] = DecodedOp::new(0xC1, Cmp, IndirectX,         2, 6);
    t[0xC4] = DecodedOp::new(0xC4, Cpy, ZeroPage,          2, 3);
    t[0xC5] = DecodedOp::new(0xC5, Cmp, ZeroPage,          2, 3);
    t[0xC6] = DecodedOp::new(0xC6, Dec, ZeroPage,          2, 5);
    t[0xC7] = DecodedOp::wdc_only(0xC7, Smb4, ZeroPage,    2, 5);
    t[0xC8] = DecodedOp::new(0xC8, Iny, Implied,           1, 2);
    t[0xC9] = DecodedOp::new(0xC9, Cmp, Immediate,         2, 2);
    t[0xCA] = DecodedOp::new(0xCA, Dex, Implied,           1, 2);
    t[0xCB] = DecodedOp::wdc_only(0xCB, Wai, Implied,      1, 3);
    t[0xCC] = DecodedOp::new(0xCC, Cpy, Absolute,          3, 4);
    t[0xCD] = DecodedOp::new(0xCD, Cmp, Absolute,          3, 4);
    t[0xCE] = DecodedOp::new(0xCE, Dec, Absolute,          3, 6);
    t[0xCF] = DecodedOp::wdc_only(0xCF, Bbs4, ZeroPageRelative, 3, 5);

    // 0xD0–0xDF
    t[0xD0] = DecodedOp::new(0xD0, Bne, Relative,          2, 2);
    t[0xD1] = DecodedOp::new(0xD1, Cmp, IndirectY,         2, 5);
    t[0xD2] = DecodedOp::new(0xD2, Cmp, ZeroPageIndirect,  2, 5);
    t[0xD5] = DecodedOp::new(0xD5, Cmp, ZeroPageX,         2, 4);
    t[0xD6] = DecodedOp::new(0xD6, Dec, ZeroPageX,         2, 6);
    t[0xD7] = DecodedOp::wdc_only(0xD7, Smb5, ZeroPage,    2, 5);
    t[0xD8] = DecodedOp::new(0xD8, Cld, Implied,           1, 2);
    t[0xD9] = DecodedOp::new(0xD9, Cmp, AbsoluteY,         3, 4);
    t[0xDA] = DecodedOp::new(0xDA, Phx, Implied,           1, 3);
    t[0xDB] = DecodedOp::wdc_only(0xDB, Stp, Implied,      1, 3);
    t[0xDD] = DecodedOp::new(0xDD, Cmp, AbsoluteX,         3, 4);
    t[0xDE] = DecodedOp::new(0xDE, Dec, AbsoluteX,         3, 7);
    t[0xDF] = DecodedOp::wdc_only(0xDF, Bbs5, ZeroPageRelative, 3, 5);

    // 0xE0–0xEF
    t[0xE0] = DecodedOp::new(0xE0, Cpx, Immediate,         2, 2);
    t[0xE1] = DecodedOp::new(0xE1, Sbc, IndirectX,         2, 6);
    t[0xE4] = DecodedOp::new(0xE4, Cpx, ZeroPage,          2, 3);
    t[0xE5] = DecodedOp::new(0xE5, Sbc, ZeroPage,          2, 3);
    t[0xE6] = DecodedOp::new(0xE6, Inc, ZeroPage,          2, 5);
    t[0xE7] = DecodedOp::wdc_only(0xE7, Smb6, ZeroPage,    2, 5);
    t[0xE8] = DecodedOp::new(0xE8, Inx, Implied,           1, 2);
    t[0xE9] = DecodedOp::new(0xE9, Sbc, Immediate,         2, 2);
    t[0xEA] = DecodedOp::new(0xEA, Nop, Implied,           1, 2);
    t[0xEC] = DecodedOp::new(0xEC, Cpx, Absolute,          3, 4);
    t[0xED] = DecodedOp::new(0xED, Sbc, Absolute,          3, 4);
    t[0xEE] = DecodedOp::new(0xEE, Inc, Absolute,          3, 6);
    t[0xEF] = DecodedOp::wdc_only(0xEF, Bbs6, ZeroPageRelative, 3, 5);

    // 0xF0–0xFF
    t[0xF0] = DecodedOp::new(0xF0, Beq, Relative,          2, 2);
    t[0xF1] = DecodedOp::new(0xF1, Sbc, IndirectY,         2, 5);
    t[0xF2] = DecodedOp::new(0xF2, Sbc, ZeroPageIndirect,  2, 5);
    t[0xF5] = DecodedOp::new(0xF5, Sbc, ZeroPageX,         2, 4);
    t[0xF6] = DecodedOp::new(0xF6, Inc, ZeroPageX,         2, 6);
    t[0xF7] = DecodedOp::wdc_only(0xF7, Smb7, ZeroPage,    2, 5);
    t[0xF8] = DecodedOp::new(0xF8, Sed, Implied,           1, 2);
    t[0xF9] = DecodedOp::new(0xF9, Sbc, AbsoluteY,         3, 4);
    t[0xFA] = DecodedOp::new(0xFA, Plx, Implied,           1, 4);
    t[0xFD] = DecodedOp::new(0xFD, Sbc, AbsoluteX,         3, 4);
    t[0xFE] = DecodedOp::new(0xFE, Inc, AbsoluteX,         3, 7);
    t[0xFF] = DecodedOp::wdc_only(0xFF, Bbs7, ZeroPageRelative, 3, 5);

    // Fix opcodes not set in the loop (their opcode field stays 0 from ill init)
    // Set opcode field correctly for all valid entries
    let mut i = 0usize;
    while i < 256 {
        t[i].opcode = i as u8;
        i += 1;
    }

    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmos_table() -> [DecodedOp; 256] {
        decode_table(CpuVariant::Cmos65C02)
    }

    fn wdc_table() -> [DecodedOp; 256] {
        decode_table(CpuVariant::Wdc65C02)
    }

    #[test]
    fn every_opcode_field_matches_index() {
        let t = cmos_table();
        for i in 0..256usize {
            assert_eq!(t[i].opcode, i as u8, "opcode field mismatch at index {i:#04X}");
        }
    }

    #[test]
    fn byte_len_is_1_2_or_3() {
        let t = cmos_table();
        for entry in &t {
            assert!(
                entry.byte_len >= 1 && entry.byte_len <= 3,
                "byte_len {} out of range at opcode {:#04X}", entry.byte_len, entry.opcode
            );
        }
    }

    #[test]
    fn base_cycles_nonzero() {
        let t = cmos_table();
        for entry in &t {
            assert!(entry.base_cycles >= 1,
                "base_cycles 0 at opcode {:#04X}", entry.opcode);
        }
    }

    // Spot-check a representative cross-section of opcodes
    #[test]
    fn spot_check_known_opcodes() {
        let t = cmos_table();

        let brk = &t[0x00];
        assert_eq!(brk.mnemonic, Brk);
        assert_eq!(brk.mode, Implied);
        assert_eq!(brk.byte_len, 2);
        assert_eq!(brk.base_cycles, 7);
        assert!(brk.is_valid);

        let nop = &t[0xEA];
        assert_eq!(nop.mnemonic, Nop);
        assert_eq!(nop.mode, Implied);
        assert_eq!(nop.byte_len, 1);
        assert_eq!(nop.base_cycles, 2);
        assert!(nop.is_valid);

        let lda_imm = &t[0xA9];
        assert_eq!(lda_imm.mnemonic, Lda);
        assert_eq!(lda_imm.mode, Immediate);
        assert_eq!(lda_imm.byte_len, 2);

        let jmp_abs = &t[0x4C];
        assert_eq!(jmp_abs.mnemonic, Jmp);
        assert_eq!(jmp_abs.mode, Absolute);
        assert_eq!(jmp_abs.byte_len, 3);
        assert_eq!(jmp_abs.base_cycles, 3);

        let jmp_ind = &t[0x6C];
        assert_eq!(jmp_ind.mnemonic, Jmp);
        assert_eq!(jmp_ind.mode, Indirect);
        assert_eq!(jmp_ind.byte_len, 3);
        assert_eq!(jmp_ind.base_cycles, 6);

        let jsr = &t[0x20];
        assert_eq!(jsr.mnemonic, Jsr);
        assert_eq!(jsr.base_cycles, 6);

        let rts = &t[0x60];
        assert_eq!(rts.mnemonic, Rts);
        assert_eq!(rts.base_cycles, 6);
    }

    #[test]
    fn wdc_only_opcodes_invalid_under_cmos() {
        let t = cmos_table();
        // STP (0xDB), WAI (0xCB)
        assert!(!t[0xDB].is_valid);
        assert!(!t[0xCB].is_valid);
        // BBR0 (0x0F), BBS0 (0x8F), RMB0 (0x07), SMB0 (0x87)
        assert!(!t[0x0F].is_valid);
        assert!(!t[0x8F].is_valid);
        assert!(!t[0x07].is_valid);
        assert!(!t[0x87].is_valid);
    }

    #[test]
    fn wdc_only_opcodes_valid_under_wdc() {
        let t = wdc_table();
        assert!(t[0xDB].is_valid); // STP
        assert!(t[0xCB].is_valid); // WAI
        assert!(t[0x0F].is_valid); // BBR0
        assert!(t[0x8F].is_valid); // BBS0
        assert!(t[0x07].is_valid); // RMB0
        assert!(t[0x87].is_valid); // SMB0
        // All BBR/BBS/RMB/SMB
        for (opcode, mnemonic) in [
            (0x0F, Bbr0), (0x1F, Bbr1), (0x2F, Bbr2), (0x3F, Bbr3),
            (0x4F, Bbr4), (0x5F, Bbr5), (0x6F, Bbr6), (0x7F, Bbr7),
            (0x8F, Bbs0), (0x9F, Bbs1), (0xAF, Bbs2), (0xBF, Bbs3),
            (0xCF, Bbs4), (0xDF, Bbs5), (0xEF, Bbs6), (0xFF, Bbs7),
            (0x07, Rmb0), (0x17, Rmb1), (0x27, Rmb2), (0x37, Rmb3),
            (0x47, Rmb4), (0x57, Rmb5), (0x67, Rmb6), (0x77, Rmb7),
            (0x87, Smb0), (0x97, Smb1), (0xA7, Smb2), (0xB7, Smb3),
            (0xC7, Smb4), (0xD7, Smb5), (0xE7, Smb6), (0xF7, Smb7),
        ] {
            let e = &t[opcode as usize];
            assert_eq!(e.mnemonic, mnemonic, "mnemonic mismatch at {opcode:#04X}");
            assert!(e.is_valid, "expected valid at {opcode:#04X}");
        }
    }

    #[test]
    fn cmos_65c02_additions_are_valid() {
        let t = cmos_table();
        // BRA, STZ, PHX, PHY, PLX, PLY, JMP (abs,x), INC/DEC accumulator
        assert!(t[0x80].is_valid); // BRA
        assert!(t[0x64].is_valid); // STZ zp
        assert!(t[0x9C].is_valid); // STZ abs
        assert!(t[0xDA].is_valid); // PHX
        assert!(t[0x5A].is_valid); // PHY
        assert!(t[0xFA].is_valid); // PLX
        assert!(t[0x7A].is_valid); // PLY
        assert!(t[0x7C].is_valid); // JMP (abs,x)
        assert!(t[0x1A].is_valid); // INC A
        assert!(t[0x3A].is_valid); // DEC A
    }

    #[test]
    fn ill_opcode_slots_are_invalid() {
        let t = cmos_table();
        // A few slots known to be illegal on the 65C02
        assert!(!t[0x02].is_valid);
        assert!(!t[0x03].is_valid);
        assert!(!t[0x22].is_valid);
        assert!(!t[0x44].is_valid);
    }

    #[test]
    fn zeropage_relative_mode_for_bbr_bbs() {
        let t = wdc_table();
        assert_eq!(t[0x0F].mode, ZeroPageRelative);
        assert_eq!(t[0x8F].mode, ZeroPageRelative);
        assert_eq!(t[0x0F].byte_len, 3);
    }
}
