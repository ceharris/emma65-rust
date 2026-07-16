//! Emulator for the (WDC)65C02 microprocessor and common peripherals.

pub mod bus;
pub mod cpu;
pub mod disasm;
pub mod device;
pub mod error;
pub mod exec;
pub mod transport;
mod config;
mod session;

pub use bus::trace::{BinaryTraceWriter, BusTraceCallback, TraceRecord};
pub use bus::{AddressRange, Bus, BusConfig, BusOp, InterruptController, IrqSource, RomWritePolicy, UnmappedPolicy};
pub use config::{BuildError, Config, CpuVariantSpec, DeviceModule, DeviceModuleError, DeviceRegistry, DeviceSpec, InstantiationContext, RamModule, RomModule, TransportSlot, TransportSpec, TransportSpecFormat};
pub use cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic};
pub use cpu::status::StatusRegister;
pub use cpu::variant::{CpuVariant, InvalidOpcodePolicy};
pub use cpu::{map_flag_name, map_register_name, Cpu, CpuBuilder, Registers};
pub use device::{device_event_channel, DeviceEvent, DeviceId, ErrorReceiver, ErrorSender, IoDevice};
pub use device::{ProtocolManager, ProtocolMessageDecoder, ProtocolMessageEncoder, ProtocolMessageEncoding};
pub use device::{PtmAsciiProtocolDecoder, PtmAsciiProtocolEncoder, PtmBinaryProtocolDecoder, PtmBinaryProtocolEncoder, PtmProtocolMessage};
pub use device::{ViaAsciiProtocolDecoder, ViaAsciiProtocolEncoder, ViaBinaryProtocolDecoder, ViaBinaryProtocolEncoder, ViaProtocolMessage};
pub use disasm::{DisassembledLine, Disassembler};
pub use error::{BusConfigError, BusError, CpuBuildError, ExecError};
pub use exec::{run, run_from, step_into, step_over_breakpoint, step_over_subroutine, step_return, ClockSpeed, CpuLiveSnapshot, RunHandle, RunStopper, StepResult};
pub use session::EmulatorSession;
pub use transport::{PipeTransport, PtyTransport, TcpSocketTransport, Transport, TransportError, TransportEvent, UnixSocketTransport};
