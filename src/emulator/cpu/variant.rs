/// Selects the CPU instruction set variant.
///
/// `Wdc65C02` adds 34 opcodes: STP, WAI, BBR0–7, BBS0–7, RMB0–7, SMB0–7.
/// In `Cmos65C02` mode those opcodes are treated per `InvalidOpcodePolicy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVariant {
    Cmos65C02,
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
