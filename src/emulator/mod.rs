/// Memory bus, address decoding, and bus tracing.
pub mod bus;
/// CPU variants, registers, status flags, and opcode decode table.
pub mod cpu;
/// IO device trait and device identification.
pub mod device;
/// Error types for execution, bus, configuration, and CPU construction failures.
pub mod error;
/// Execution model: clock speed, step results, and free-running run handle.
pub mod exec;

pub use bus::region::{AddressRange, BusOp};
pub use bus::{Bus, BusConfig, RomWritePolicy, UnmappedPolicy};
pub use cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic};
pub use cpu::status::StatusRegister;
pub use cpu::variant::{CpuVariant, InvalidOpcodePolicy};
pub use device::{DeviceId, IoDevice};
pub use error::{BusConfigError, BusError, CpuBuildError, ExecError};
pub use exec::{ClockSpeed, StepResult};
