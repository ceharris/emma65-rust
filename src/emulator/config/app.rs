use std::fmt::{Display, Formatter};
use std::str::FromStr;
use clap::Parser;
use serde::{Deserialize, Serialize};
use crate::emulator::{BusConfig, ClockSpeed, Cpu, CpuBuildError, CpuVariant, EmulatorSession};
use crate::emulator::device::device_event_channel;
use super::CpuVariantSpec::{Cmos6502, Wdc6502};
use super::{DeviceSpec, DeviceModuleError, DeviceRegistry, InstantiationContext};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CpuVariantSpec {
    Cmos6502,
    Wdc6502,
}

impl CpuVariantSpec {

    fn to_cpu_variant(&self) -> CpuVariant {
        match self {
            Cmos6502 => CpuVariant::Cmos65C02,
            Wdc6502 => CpuVariant::Wdc65C02,
        }
    }

}

impl FromStr for CpuVariantSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let upper_s = s.to_ascii_uppercase();
        let us = upper_s.as_str();
        match us {
            "65C02" | "C02" => Ok(Cmos6502),
            "WDC65C02" | "WDC02" => Ok(Wdc6502),
            _ => Err(format!("Invalid CPU variant '{s}'")),
        }
    }

}

/// An error that occurs during app configuration or startup.
#[derive(Debug)]
pub enum StartupError {
    /// An error that occurred while creating and configuring the CPU.
    Cpu(CpuBuildError),
    /// An error that occurred while instantiating a device module.
    Device { module_name: String, address: u16, source: DeviceModuleError },
}

impl Display for StartupError {

    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            StartupError::Cpu(e) => write!(f, "CPU configuration error: {e}"),
            StartupError::Device { module_name, address, source } =>
                write!(f, "failed to configure device '{module_name}' at address {address:#06x}: {source}"),
        }
    }

}

#[derive(Debug, Clone, Parser, Serialize, Deserialize)]
#[clap(name = "Emulator")]
#[serde(rename_all = "kebab-case")]
pub struct AppConfig {

    /// Selected CPU variant (e.g. 65C02, WDC65C02).
    pub cpu_variant_spec: Option<CpuVariantSpec>,

    /// Clock speed to simulate via throttling.
    pub clock_speed_hz: Option<u64>,

    /// Device config specifications.
    #[clap(long = "device", num_args = 1..)]
    pub devices: Option<Vec<DeviceSpec>>,

}

impl AppConfig {

    pub async fn build(&self, registry: &DeviceRegistry) -> Result<EmulatorSession, StartupError>{
        let (error_sender, error_receiver) = device_event_channel();
        let context = InstantiationContext {
            clock_hz: self.clock_speed_hz,
            error_sender: Some(error_sender),
        };
        let mut bus_config = BusConfig::new();
        for spec in self.devices.iter().flatten() {
            bus_config = registry.instantiate(spec.module_name(), bus_config, spec.address(), spec.attributes(), &context)
                .await
                .map_err(|e| StartupError::Device {
                    module_name: spec.module_name().to_string(),
                    address: spec.address(),
                    source: e,
                })?;
        }
        let variant = self.cpu_variant_spec.as_ref().map_or(CpuVariant::Cmos65C02, CpuVariantSpec::to_cpu_variant);
        let bus = bus_config.build();
        let cpu = Cpu::builder(variant)
            .clock_speed(self.clock_speed_hz.map_or(ClockSpeed::unlimited(), ClockSpeed::hz))
            .bus(bus)
            .build()
            .map_err(StartupError::Cpu)?;
        Ok(EmulatorSession {
            cpu, error_receiver
        })
    }

}