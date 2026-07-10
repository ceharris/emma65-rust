//! CPU variant selection and invalid-opcode policy.

/// Selects the CPU instruction set variant.
///
/// `Wdc65C02` adds 34 opcodes: STP, WAI, BBR0–7, BBS0–7, RMB0–7, SMB0–7.
/// In `Cmos65C02` mode those opcodes are treated per `InvalidOpcodePolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVariant {
    /// Standard 65C02 CMOS variant. Does not include STP, WAI, BBR/BBS, or RMB/SMB.
    Cmos65C02,
    /// WDC 65C02 variant, which adds STP, WAI, BBR0–7, BBS0–7, RMB0–7, and SMB0–7.
    Wdc65C02,
}

/// Controls how unrecognized or variant-invalid opcodes are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidOpcodePolicy {
    /// Treat invalid opcodes as NOPs, advancing PC by the correct byte length.
    Nop,
    /// Return `StepResult::Error` on invalid opcodes.
    Error,
}
