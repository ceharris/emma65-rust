//! Emulator for the (WDC)65C02 microprocessor and common peripherals.

pub mod bus;
pub mod cpu;
pub mod device;
pub mod error;
pub mod exec;
pub mod transport;
pub mod config;
mod logging;
mod session;

pub use cpu::trace::{BinaryTraceReader, BinaryTraceWriter, ChannelTraceCallback, OverflowPolicy, TraceCallback, TraceKind, TraceRecord, spawn_trace_writer};
pub use logging::{LogCategory, LogLevel, LogRecord, LogSender, spawn_log_collector, spawn_log_writer};
pub(crate) use logging::log_msg;
pub use cpu::vector::{IdentityVectorResolver, VectorResolver};
pub use bus::{AddressRange, Bus, BusConfig, BusOp, InterruptController, IrqSource, RomWritePolicy, SymbolTable, UnmappedPolicy};
pub use config::{BuildError, Config, CpuVariantSpec, DeviceModule, DeviceModuleError, DeviceRegistry, DeviceSpec, ExpandedPathBuf, InstantiationContext, RamModule, RomModule, TransportSlot, TransportSpec, TransportSpecFormat};
pub use cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic};
pub use cpu::status::StatusRegister;
pub use cpu::variant::{CpuVariant, InvalidOpcodePolicy};
pub use cpu::{map_flag_name, map_register_name, Cpu, CpuBuilder, Registers};
pub use cpu::StepResult;
pub use device::{device_event_channel, log_device_event, log_device_events, DeviceEvent, DeviceId, ErrorReceiver, ErrorSender, IoDevice};
pub use device::{PtmAsciiProtocolDecoder, PtmAsciiProtocolEncoder, PtmBinaryProtocolDecoder, PtmBinaryProtocolEncoder, PtmProtocolMessage};
pub use device::{ViaAsciiProtocolDecoder, ViaAsciiProtocolEncoder, ViaBinaryProtocolDecoder, ViaBinaryProtocolEncoder, ViaProtocolMessage};
pub use error::{BusConfigError, BusError, CpuBuildError, ExecError};
pub use exec::{run, run_from, step_into, step_over_breakpoint, step_over_subroutine, step_return, ClockSpeed, CpuLiveSnapshot, RunHandle, RunStopper};
pub use session::EmulatorSession;
pub use transport::{ChannelRelay, InternalPipeTransport, PipeTransport, PtyTransport, TcpSocketTransport, Transport, TransportError, TransportEvent, TransportRelay, TransportReporter, UnixSocketTransport};
