//! Pure ALU functions consumed by the CPU instruction dispatcher.
//!
//! Every function takes explicit inputs (operands + relevant flag bits) and returns
//! a result value plus the updated N/Z/C/V flags. No CPU state is mutated here.

use crate::emulator::cpu::status::StatusRegister;

/// Result of a binary arithmetic or logic operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AluResult {
    /// The computed 8-bit result.
    pub value: u8,
    /// Updated processor status flags (only N, Z, C, V are written; other bits unchanged).
    pub status: StatusRegister,
}

// ---------------------------------------------------------------------------
// Internal flag helpers
// ---------------------------------------------------------------------------

fn set_nz(status: StatusRegister, value: u8) -> StatusRegister {
    let mut s = status;
    s.set(StatusRegister::N, value & 0x80 != 0);
    s.set(StatusRegister::Z, value == 0);
    s
}

// ---------------------------------------------------------------------------
// ADC / SBC
// ---------------------------------------------------------------------------

/// ADC in binary mode.
pub fn adc_binary(a: u8, operand: u8, status: StatusRegister) -> AluResult {
    let carry_in = status.contains(StatusRegister::C) as u16;
    let sum = (a as u16) + (operand as u16) + carry_in;
    let result = sum as u8;
    let mut s = set_nz(status, result);
    s.set(StatusRegister::C, sum > 0xFF);
    s.set(
        StatusRegister::V,
        (!(a ^ operand) & (a ^ result) & 0x80) != 0,
    );
    AluResult { value: result, status: s }
}

/// ADC in BCD (decimal) mode.
pub fn adc_bcd(a: u8, operand: u8, status: StatusRegister) -> AluResult {
    let carry_in = status.contains(StatusRegister::C) as u8;

    let mut lo = (a & 0x0F) + (operand & 0x0F) + carry_in;
    let lo_carry = if lo > 9 { lo += 6; 1u8 } else { 0u8 };

    let mut hi = (a >> 4) + (operand >> 4) + lo_carry;
    // V is based on binary-style two's-complement overflow of the high nibble
    // before BCD correction (65C02 behavior).
    let bin_result = a.wrapping_add(operand).wrapping_add(carry_in);
    let overflow = (!(a ^ operand) & (a ^ bin_result) & 0x80) != 0;

    let carry_out = hi > 9;
    if carry_out { hi += 6; }

    let result = ((hi & 0x0F) << 4) | (lo & 0x0F);
    let mut s = set_nz(status, result);
    s.set(StatusRegister::C, carry_out);
    s.set(StatusRegister::V, overflow);
    AluResult { value: result, status: s }
}

/// SBC in binary mode.
pub fn sbc_binary(a: u8, operand: u8, status: StatusRegister) -> AluResult {
    // 6502 identity: A - operand - borrow  =  A + ~operand + carry
    // (borrow = !carry, and bitwise NOT satisfies ~operand = -operand - 1,
    // so adding ~operand + 1 via the carry bit gives the subtraction).
    adc_binary(a, !operand, status)
}

/// SBC in BCD (decimal) mode.
pub fn sbc_bcd(a: u8, operand: u8, status: StatusRegister) -> AluResult {
    let borrow_in = !status.contains(StatusRegister::C) as u8;

    let mut lo = (a as i16 & 0x0F) - (operand as i16 & 0x0F) - borrow_in as i16;
    let lo_borrow = if lo < 0 { lo += 10; 1i16 } else { 0i16 };

    let mut hi = (a as i16 >> 4) - (operand as i16 >> 4) - lo_borrow;
    // V reflects binary subtraction overflow (65C02 behavior).
    let bin_result = a.wrapping_sub(operand).wrapping_sub(borrow_in);
    let overflow = ((a ^ operand) & (a ^ bin_result) & 0x80) != 0;

    let borrow_out = hi < 0;
    if borrow_out { hi += 10; }

    let result = ((hi as u8 & 0x0F) << 4) | (lo as u8 & 0x0F);
    let mut s = set_nz(status, result);
    s.set(StatusRegister::C, !borrow_out);
    s.set(StatusRegister::V, overflow);
    AluResult { value: result, status: s }
}

// ---------------------------------------------------------------------------
// Logic: AND, ORA, EOR
// ---------------------------------------------------------------------------

/// AND — bitwise AND; updates N and Z.
pub fn and(a: u8, operand: u8, status: StatusRegister) -> AluResult {
    let result = a & operand;
    AluResult { value: result, status: set_nz(status, result) }
}

/// ORA — bitwise OR; updates N and Z.
pub fn ora(a: u8, operand: u8, status: StatusRegister) -> AluResult {
    let result = a | operand;
    AluResult { value: result, status: set_nz(status, result) }
}

/// EOR — bitwise exclusive OR; updates N and Z.
pub fn eor(a: u8, operand: u8, status: StatusRegister) -> AluResult {
    let result = a ^ operand;
    AluResult { value: result, status: set_nz(status, result) }
}

// ---------------------------------------------------------------------------
// Shifts and rotates
// ---------------------------------------------------------------------------

/// ASL — arithmetic shift left; updates N, Z, C.
pub fn asl(value: u8, status: StatusRegister) -> AluResult {
    let result = value << 1;
    let mut s = set_nz(status, result);
    s.set(StatusRegister::C, value & 0x80 != 0);
    AluResult { value: result, status: s }
}

/// LSR — logical shift right; updates N (always cleared), Z, C.
pub fn lsr(value: u8, status: StatusRegister) -> AluResult {
    let result = value >> 1;
    let mut s = set_nz(status, result);
    s.set(StatusRegister::C, value & 0x01 != 0);
    AluResult { value: result, status: s }
}

/// ROL — rotate left through carry; updates N, Z, C.
pub fn rol(value: u8, status: StatusRegister) -> AluResult {
    let carry_in = status.contains(StatusRegister::C) as u8;
    let result = (value << 1) | carry_in;
    let mut s = set_nz(status, result);
    s.set(StatusRegister::C, value & 0x80 != 0);
    AluResult { value: result, status: s }
}

/// ROR — rotate right through carry; updates N, Z, C.
pub fn ror(value: u8, status: StatusRegister) -> AluResult {
    let carry_in = status.contains(StatusRegister::C) as u8;
    let result = (value >> 1) | (carry_in << 7);
    let mut s = set_nz(status, result);
    s.set(StatusRegister::C, value & 0x01 != 0);
    AluResult { value: result, status: s }
}

// ---------------------------------------------------------------------------
// INC / DEC
// ---------------------------------------------------------------------------

/// INC — increment; updates N and Z.
pub fn inc(value: u8, status: StatusRegister) -> AluResult {
    let result = value.wrapping_add(1);
    AluResult { value: result, status: set_nz(status, result) }
}

/// DEC — decrement; updates N and Z.
pub fn dec(value: u8, status: StatusRegister) -> AluResult {
    let result = value.wrapping_sub(1);
    AluResult { value: result, status: set_nz(status, result) }
}

// ---------------------------------------------------------------------------
// Compare
// ---------------------------------------------------------------------------

/// CMP / CPX / CPY — subtract without storing; updates N, Z, C.
pub fn compare(reg: u8, operand: u8, status: StatusRegister) -> StatusRegister {
    let result = reg.wrapping_sub(operand);
    let mut s = set_nz(status, result);
    s.set(StatusRegister::C, reg >= operand);
    s
}

// ---------------------------------------------------------------------------
// BIT
// ---------------------------------------------------------------------------

/// BIT (zero-page / absolute) — sets Z from (A & mem), N from bit 7 of mem, V from bit 6 of mem.
pub fn bit_mem(a: u8, mem: u8, status: StatusRegister) -> StatusRegister {
    let mut s = status;
    s.set(StatusRegister::Z, (a & mem) == 0);
    s.set(StatusRegister::N, mem & 0x80 != 0);
    s.set(StatusRegister::V, mem & 0x40 != 0);
    s
}

/// BIT (immediate, 65C02) — sets Z from (A & imm) only; N and V are unaffected.
pub fn bit_imm(a: u8, imm: u8, status: StatusRegister) -> StatusRegister {
    let mut s = status;
    s.set(StatusRegister::Z, (a & imm) == 0);
    s
}

// ---------------------------------------------------------------------------
// TRB / TSB (65C02)
// ---------------------------------------------------------------------------

/// TSB — test and set bits; sets Z from (A & mem), returns mem | A.
pub fn tsb(a: u8, mem: u8, status: StatusRegister) -> AluResult {
    let mut s = status;
    s.set(StatusRegister::Z, (a & mem) == 0);
    AluResult { value: mem | a, status: s }
}

/// TRB — test and reset bits; sets Z from (A & mem), returns mem & !A.
pub fn trb(a: u8, mem: u8, status: StatusRegister) -> AluResult {
    let mut s = status;
    s.set(StatusRegister::Z, (a & mem) == 0);
    AluResult { value: mem & !a, status: s }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn s(byte: u8) -> StatusRegister {
        StatusRegister::from_byte(byte)
    }

    const N: u8 = 0x80;
    const V: u8 = 0x40;
    const Z: u8 = 0x02;
    const C: u8 = 0x01;

    // --- ADC binary ---

    #[test]
    fn adc_binary_no_carry() {
        let r = adc_binary(0x10, 0x20, s(0));
        assert_eq!(r.value, 0x30);
        assert!(!r.status.contains(StatusRegister::C));
        assert!(!r.status.contains(StatusRegister::Z));
        assert!(!r.status.contains(StatusRegister::N));
        assert!(!r.status.contains(StatusRegister::V));
    }

    #[test]
    fn adc_binary_carry_in() {
        let r = adc_binary(0x10, 0x20, s(C));
        assert_eq!(r.value, 0x31);
    }

    #[test]
    fn adc_binary_carry_out() {
        let r = adc_binary(0xFF, 0x01, s(0));
        assert_eq!(r.value, 0x00);
        assert!(r.status.contains(StatusRegister::C));
        assert!(r.status.contains(StatusRegister::Z));
    }

    #[test]
    fn adc_binary_overflow_positive() {
        // 0x50 + 0x50 = 0xA0 — two positives yield a negative: overflow
        let r = adc_binary(0x50, 0x50, s(0));
        assert_eq!(r.value, 0xA0);
        assert!(r.status.contains(StatusRegister::V));
        assert!(r.status.contains(StatusRegister::N));
    }

    #[test]
    fn adc_binary_overflow_negative() {
        // 0xD0 + 0x90 = 0x60 — two negatives yield a positive: overflow
        let r = adc_binary(0xD0, 0x90, s(0));
        assert_eq!(r.value, 0x60);
        assert!(r.status.contains(StatusRegister::V));
        assert!(r.status.contains(StatusRegister::C));
    }

    #[test]
    fn adc_binary_no_overflow_mixed_signs() {
        let r = adc_binary(0x50, 0xD0, s(0));
        assert!(!r.status.contains(StatusRegister::V));
    }

    #[test]
    fn adc_binary_zero_flag() {
        let r = adc_binary(0x80, 0x80, s(0));
        assert_eq!(r.value, 0x00);
        assert!(r.status.contains(StatusRegister::Z));
        assert!(r.status.contains(StatusRegister::C));
    }

    // --- SBC binary ---

    #[test]
    fn sbc_binary_no_borrow() {
        // C=1 means no borrow
        let r = sbc_binary(0x50, 0x10, s(C));
        assert_eq!(r.value, 0x40);
        assert!(r.status.contains(StatusRegister::C));
        assert!(!r.status.contains(StatusRegister::V));
    }

    #[test]
    fn sbc_binary_borrow_out() {
        // C=1 (no borrow in), 0x10 - 0x20 borrows
        let r = sbc_binary(0x10, 0x20, s(C));
        assert_eq!(r.value, 0xF0);
        assert!(!r.status.contains(StatusRegister::C)); // borrow out
        assert!(r.status.contains(StatusRegister::N));
    }

    #[test]
    fn sbc_binary_overflow_positive_minus_negative() {
        // 0x50 - 0xB0 = 0xA0: positive - negative = negative → overflow
        let r = sbc_binary(0x50, 0xB0, s(C));
        assert_eq!(r.value, 0xA0);
        assert!(r.status.contains(StatusRegister::V));
    }

    #[test]
    fn sbc_binary_overflow_negative_minus_positive() {
        // 0xD0 - 0x70 = 0x60: negative - positive = positive → overflow
        let r = sbc_binary(0xD0, 0x70, s(C));
        assert_eq!(r.value, 0x60);
        assert!(r.status.contains(StatusRegister::V));
    }

    #[test]
    fn sbc_binary_zero_result() {
        let r = sbc_binary(0x42, 0x42, s(C));
        assert_eq!(r.value, 0x00);
        assert!(r.status.contains(StatusRegister::Z));
        assert!(r.status.contains(StatusRegister::C));
    }

    // --- ADC BCD ---

    #[test]
    fn adc_bcd_simple() {
        let r = adc_bcd(0x15, 0x27, s(0));
        assert_eq!(r.value, 0x42);
        assert!(!r.status.contains(StatusRegister::C));
    }

    #[test]
    fn adc_bcd_carry_out() {
        let r = adc_bcd(0x99, 0x01, s(0));
        assert_eq!(r.value, 0x00);
        assert!(r.status.contains(StatusRegister::C));
        assert!(r.status.contains(StatusRegister::Z));
    }

    #[test]
    fn adc_bcd_low_nibble_carry() {
        // 0x09 + 0x01 = 0x10 in BCD
        let r = adc_bcd(0x09, 0x01, s(0));
        assert_eq!(r.value, 0x10);
        assert!(!r.status.contains(StatusRegister::C));
    }

    #[test]
    fn adc_bcd_with_carry_in() {
        let r = adc_bcd(0x58, 0x46, s(C));
        assert_eq!(r.value, 0x05);
        assert!(r.status.contains(StatusRegister::C));
    }

    // --- SBC BCD ---

    #[test]
    fn sbc_bcd_simple() {
        let r = sbc_bcd(0x46, 0x12, s(C));
        assert_eq!(r.value, 0x34);
        assert!(r.status.contains(StatusRegister::C));
    }

    #[test]
    fn sbc_bcd_borrow_out() {
        let r = sbc_bcd(0x00, 0x01, s(C));
        assert_eq!(r.value, 0x99);
        assert!(!r.status.contains(StatusRegister::C));
    }

    #[test]
    fn sbc_bcd_zero_result() {
        let r = sbc_bcd(0x42, 0x42, s(C));
        assert_eq!(r.value, 0x00);
        assert!(r.status.contains(StatusRegister::Z));
        assert!(r.status.contains(StatusRegister::C));
    }

    // --- Logic ---

    #[test]
    fn and_clears_bits() {
        let r = and(0xFF, 0x0F, s(0));
        assert_eq!(r.value, 0x0F);
        assert!(!r.status.contains(StatusRegister::N));
        assert!(!r.status.contains(StatusRegister::Z));
    }

    #[test]
    fn and_zero_result() {
        let r = and(0xF0, 0x0F, s(0));
        assert_eq!(r.value, 0x00);
        assert!(r.status.contains(StatusRegister::Z));
    }

    #[test]
    fn ora_sets_bits() {
        let r = ora(0x0F, 0xF0, s(0));
        assert_eq!(r.value, 0xFF);
        assert!(r.status.contains(StatusRegister::N));
    }

    #[test]
    fn eor_toggles_bits() {
        let r = eor(0xFF, 0x0F, s(0));
        assert_eq!(r.value, 0xF0);
        assert!(r.status.contains(StatusRegister::N));
    }

    // --- Shifts/rotates ---

    #[test]
    fn asl_shifts_and_carries() {
        let r = asl(0x81, s(0));
        assert_eq!(r.value, 0x02);
        assert!(r.status.contains(StatusRegister::C));
        assert!(!r.status.contains(StatusRegister::N));
    }

    #[test]
    fn asl_zero_result() {
        let r = asl(0x80, s(0));
        assert_eq!(r.value, 0x00);
        assert!(r.status.contains(StatusRegister::C));
        assert!(r.status.contains(StatusRegister::Z));
    }

    #[test]
    fn lsr_shifts_and_carries() {
        let r = lsr(0x03, s(0));
        assert_eq!(r.value, 0x01);
        assert!(r.status.contains(StatusRegister::C));
        assert!(!r.status.contains(StatusRegister::N));
    }

    #[test]
    fn lsr_clears_n_flag() {
        let r = lsr(0x80, s(0));
        assert_eq!(r.value, 0x40);
        assert!(!r.status.contains(StatusRegister::N));
        assert!(!r.status.contains(StatusRegister::C));
    }

    #[test]
    fn rol_rotates_carry_in() {
        let r = rol(0x80, s(C));
        assert_eq!(r.value, 0x01);
        assert!(r.status.contains(StatusRegister::C));
    }

    #[test]
    fn rol_no_carry_in() {
        let r = rol(0x40, s(0));
        assert_eq!(r.value, 0x80);
        assert!(!r.status.contains(StatusRegister::C));
        assert!(r.status.contains(StatusRegister::N));
    }

    #[test]
    fn ror_rotates_carry_in() {
        let r = ror(0x01, s(C));
        assert_eq!(r.value, 0x80);
        assert!(r.status.contains(StatusRegister::C));
        assert!(r.status.contains(StatusRegister::N));
    }

    #[test]
    fn ror_no_carry_in() {
        let r = ror(0x02, s(0));
        assert_eq!(r.value, 0x01);
        assert!(!r.status.contains(StatusRegister::C));
    }

    // --- INC / DEC ---

    #[test]
    fn inc_wraps() {
        let r = inc(0xFF, s(0));
        assert_eq!(r.value, 0x00);
        assert!(r.status.contains(StatusRegister::Z));
    }

    #[test]
    fn inc_sets_n() {
        let r = inc(0x7F, s(0));
        assert_eq!(r.value, 0x80);
        assert!(r.status.contains(StatusRegister::N));
    }

    #[test]
    fn dec_wraps() {
        let r = dec(0x00, s(0));
        assert_eq!(r.value, 0xFF);
        assert!(r.status.contains(StatusRegister::N));
    }

    #[test]
    fn dec_sets_z() {
        let r = dec(0x01, s(0));
        assert_eq!(r.value, 0x00);
        assert!(r.status.contains(StatusRegister::Z));
    }

    // --- Compare ---

    #[test]
    fn compare_equal() {
        let s = compare(0x42, 0x42, s(0));
        assert!(s.contains(StatusRegister::Z));
        assert!(s.contains(StatusRegister::C));
        assert!(!s.contains(StatusRegister::N));
    }

    #[test]
    fn compare_greater() {
        let s = compare(0x50, 0x10, s(0));
        assert!(!s.contains(StatusRegister::Z));
        assert!(s.contains(StatusRegister::C));
        assert!(!s.contains(StatusRegister::N));
    }

    #[test]
    fn compare_less() {
        let s = compare(0x10, 0x50, s(0));
        assert!(!s.contains(StatusRegister::Z));
        assert!(!s.contains(StatusRegister::C));
        assert!(s.contains(StatusRegister::N));
    }

    #[test]
    fn compare_negative_flag_from_difference() {
        // 0x01 - 0x02 = 0xFF → N set, C clear
        let s = compare(0x01, 0x02, s(0));
        assert!(s.contains(StatusRegister::N));
        assert!(!s.contains(StatusRegister::C));
    }

    // --- BIT ---

    #[test]
    fn bit_mem_zero_flag() {
        // 0x0F & 0x80 == 0 → Z; mem bit 7 set → N; mem bit 6 clear → no V
        let s = bit_mem(0x0F, 0x80, s(0));
        assert!(s.contains(StatusRegister::Z));
        assert!(s.contains(StatusRegister::N));
        assert!(!s.contains(StatusRegister::V));
    }

    #[test]
    fn bit_mem_nv_from_memory() {
        let s = bit_mem(0xFF, 0xC0, s(0));
        assert!(!s.contains(StatusRegister::Z));
        assert!(s.contains(StatusRegister::N));
        assert!(s.contains(StatusRegister::V));
    }

    #[test]
    fn bit_imm_only_z() {
        // BIT immediate: N and V must not change
        let initial = s(N | V);
        let result = bit_imm(0x0F, 0xF0, initial);
        assert!(result.contains(StatusRegister::Z));
        assert!(result.contains(StatusRegister::N)); // unchanged
        assert!(result.contains(StatusRegister::V)); // unchanged
    }

    #[test]
    fn bit_imm_no_match_clears_z() {
        let result = bit_imm(0xFF, 0xFF, s(0));
        assert!(!result.contains(StatusRegister::Z));
    }

    // --- TSB / TRB ---

    #[test]
    fn tsb_sets_bits_and_z_flag() {
        let r = tsb(0x0F, 0xF0, s(0));
        assert_eq!(r.value, 0xFF);
        assert!(r.status.contains(StatusRegister::Z)); // (A & mem) == 0x00
    }

    #[test]
    fn tsb_clears_z_when_overlap() {
        let r = tsb(0x0F, 0x0F, s(0));
        assert_eq!(r.value, 0x0F);
        assert!(!r.status.contains(StatusRegister::Z));
    }

    #[test]
    fn trb_clears_bits_and_z_clear_when_overlap() {
        // (A & mem) = 0x0F & 0xFF = 0x0F ≠ 0 → Z clear
        let r = trb(0x0F, 0xFF, s(0));
        assert_eq!(r.value, 0xF0);
        assert!(!r.status.contains(StatusRegister::Z));
    }

    #[test]
    fn trb_z_set_when_no_overlap() {
        let r = trb(0xF0, 0x0F, s(0));
        assert_eq!(r.value, 0x0F);
        assert!(r.status.contains(StatusRegister::Z));
    }
}
