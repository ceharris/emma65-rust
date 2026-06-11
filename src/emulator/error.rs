use thiserror::Error;
use crate::emulator::bus::region::{AddressRange, BusOp};
use crate::emulator::device::DeviceId;

#[derive(Debug, Error)]
pub enum ExecError {
    #[error("unmapped address ${addr:04X} on {op:?}")]
    UnmappedAddress { addr: u16, op: BusOp },
    #[error("write to ROM at ${addr:04X} (value ${value:02X})")]
    RomWrite { addr: u16, value: u8 },
    #[error("invalid opcode ${opcode:02X} at ${addr:04X}")]
    InvalidOpcode { addr: u16, opcode: u8 },
}

#[derive(Debug, Error)]
pub enum BusError {
    #[error("unmapped address ${addr:04X}")]
    Unmapped { addr: u16 },
    #[error("write to ROM at ${addr:04X}")]
    RomWrite { addr: u16 },
}

#[derive(Debug, Error)]
pub enum BusConfigError {
    #[error("ambiguous overlap at {range:?}: two regions of identical size covering the same address")]
    AmbiguousOverlap { range: AddressRange },
    #[error("ROM data length {data_len} does not match range {range:?} (expected {expected})")]
    RomSizeMismatch { range: AddressRange, data_len: usize, expected: usize },
    #[error("duplicate device ID {0:?}")]
    DuplicateDeviceId(DeviceId),
}

#[derive(Debug, Error)]
pub enum CpuBuildError {
    #[error("bus configuration error: {0}")]
    BusConfig(#[from] BusConfigError),
}
