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
/// IRQ and NMI interrupt controller.
pub mod interrupt;
/// Transport abstraction and implementations for device IO.
pub mod transport;
/// Device and app configuration.
pub mod config;

pub use bus::region::{AddressRange, BusOp};
pub use bus::{Bus, BusConfig, RomWritePolicy, UnmappedPolicy};
pub use bus::trace::{BinaryTraceWriter, BusTraceCallback, TraceRecord};
pub use cpu::{Cpu, CpuBuilder, Registers, map_register_name, map_flag_name};
pub use cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic};
pub use cpu::status::StatusRegister;
pub use cpu::variant::{CpuVariant, InvalidOpcodePolicy};
pub use device::{DeviceId, DeviceEvent, ErrorSender, ErrorReceiver, IoDevice, device_event_channel};
pub use device::{Acia6551, Console, Mc6850, Via6522};
pub use device::{ViaProtocolDecoder, ViaProtocolEncoder, ViaProtocolFormat, ViaProtocolMessage};
pub use disasm::{Disassembler, DisassembledLine};
pub use error::{BusConfigError, BusError, CpuBuildError, ExecError};
pub use exec::{ClockSpeed, RunHandle, StepResult, run};
pub use interrupt::{InterruptController, IrqSource};
pub use transport::{Transport, TransportError, PipeTransport, TcpTransport, UnixSocketTransport, PtyTransport};

pub use config::{TransportSpec, DeviceModule, ConsoleModule, ConsoleAttributes};