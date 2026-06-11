/// 256-entry opcode decode table, mnemonics, and addressing modes.
pub mod opcodes;
/// Processor status register (P) as a bitflags newtype over `u8`.
pub mod status;
/// CPU variant selection and invalid-opcode policy.
pub mod variant;
