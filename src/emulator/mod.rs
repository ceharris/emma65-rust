/// Memory bus, address decoding, and bus tracing.
pub mod bus;
/// CPU variants, registers, status flags, and opcode decode table.
pub mod cpu;
/// Disassembler: decodes bus memory into human-readable instruction listings.
pub mod disasm;
/// IO device trait, device identification, and async device event channel.
pub mod device;
/// Error types for execution, bus, configuration, and CPU construction failures.
pub mod error;
/// Execution model: clock speed, step results, and free-running run handle.
pub mod exec;
/// Transport abstraction and implementations for device IO.
pub mod transport;
mod config;
mod session;

pub use bus::{AddressRange, Bus, BusOp, BusConfig, InterruptController, IrqSource, RomWritePolicy, UnmappedPolicy};
pub use bus::trace::{BinaryTraceWriter, BusTraceCallback, TraceRecord};
pub use cpu::{map_flag_name, map_register_name, Cpu, CpuBuilder, Registers};
pub use cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic};
pub use cpu::status::StatusRegister;
pub use cpu::variant::{CpuVariant, InvalidOpcodePolicy};
pub use device::{device_event_channel, DeviceEvent, DeviceId, ErrorReceiver, ErrorSender, IoDevice};
pub use device::{Acia6551, Console, Mc6850, Via6522};
pub use device::{ViaProtocolDecoder, ViaProtocolEncoder, ViaProtocolFormat, ViaProtocolMessage};
pub use disasm::{DisassembledLine, Disassembler};
pub use error::{BusConfigError, BusError, CpuBuildError, ExecError};
pub use exec::{run, step_over, step_return, ClockSpeed, RunHandle, RunStopper, StepResult};
pub use transport::{PipeTransport, PtyTransport, TcpTransport, Transport, TransportError, UnixSocketTransport};
pub use session::EmulatorSession;
pub use config::{BuildError, Config, CpuVariantSpec, DeviceModule, DeviceModuleError, DeviceRegistry, DeviceSpec, InstantiationContext, RamModule, RomModule, TransportSlot, TransportSpec, TransportSpecFormat};
