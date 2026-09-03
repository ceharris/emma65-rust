use super::CpuVariantSpec::{Cmos6502, Wdc6502};
use super::{DeviceModuleError, DeviceRegistry, DeviceSpec, InstantiationContext};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::device_event_channel;
use crate::emulator::{BusConfig, ClockSpeed, Cpu, CpuBuildError, CpuVariant, EmulatorSession, ErrorReceiver};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
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

impl Display for CpuVariantSpec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Cmos6502 => write!(f, "65C02"),
            Wdc6502 => write!(f, "WDC65C02"),
        }
    }
}

impl From<CpuVariantSpec> for String {
    fn from(v: CpuVariantSpec) -> Self {
        v.to_string()
    }
}

impl TryFrom<String> for CpuVariantSpec {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
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

/// An error that occurs during emulator configuration or startup.
#[derive(Debug)]
pub enum BuildError {
    /// An error that occurred while creating and configuring the CPU.
    Cpu(CpuBuildError),
    /// An error that occurred while instantiating a device module.
    Device { module_name: String, address: u16, source: DeviceModuleError },
}

impl Display for BuildError {

    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Cpu(e) => write!(f, "CPU configuration error: {e}"),
            BuildError::Device { module_name, address, source } =>
                write!(f, "failed to configure device '{module_name}' at address {address:#06x}: {source}"),
        }
    }

}

#[derive(Debug, Clone, Parser, Serialize, Deserialize)]
#[clap(name = "emulator")]
#[serde(rename_all = "kebab-case")]
/// Configuration attributes for the emulator.
pub struct Config {

    /// Selected CPU variant (e.g. 65C02, WDC65C02).
    #[serde(rename = "cpu-variant", skip_serializing_if = "Option::is_none")]
    #[clap(long = "cpu-variant")]
    pub cpu_variant_spec: Option<CpuVariantSpec>,

    /// Clock speed to simulate via throttling.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[clap(long = "clock-speed-hz")]
    pub clock_speed_hz: Option<u64>,

    /// Device config specifications.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[clap(long = "device", num_args = 1..)]
    pub devices: Option<Vec<DeviceSpec>>,

}

impl Config {

    /// Builds an [`EmulatorSession`] using a default [`InstantiationContext`].
    pub async fn build(&self, registry: &DeviceRegistry) -> Result<EmulatorSession, BuildError> {
        let (error_sender, error_receiver) = device_event_channel();
        let context = InstantiationContext {
            clock_hz: self.clock_speed_hz,
            error_sender: Some(error_sender),
            ..Default::default()
        };
        self.build_devices(registry, context, error_receiver).await
    }

    /// Builds an [`EmulatorSession`] using the provided [`InstantiationContext`].
    ///
    /// Use this when the caller needs to inject a pre-created console transport or
    /// supply a custom error sender before building. If `context.error_sender` is
    /// `None`, a new error channel is created and its receiver is stored in the session.
    pub async fn build_with_context(&self, registry: &DeviceRegistry, context: InstantiationContext) -> Result<EmulatorSession, BuildError> {
        let (error_sender, error_receiver) = device_event_channel();
        let context = InstantiationContext {
            error_sender: context.error_sender.or(Some(error_sender)),
            ..context
        };
        self.build_devices(registry, context, error_receiver).await
    }

    async fn build_devices(&self, registry: &DeviceRegistry, context: InstantiationContext, error_receiver: ErrorReceiver) -> Result<EmulatorSession, BuildError> {
        let mut bus_config = BusConfig::new();
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        for spec in self.devices.iter().flatten() {
            bus_config = registry.instantiate(spec.module_name(), bus_config, spec.address(), spec.attributes(), &context, id_allocator.clone())
                .await
                .map_err(|e| BuildError::Device {
                    module_name: spec.module_name().to_string(),
                    address: spec.address(),
                    source: e,
                })?;
        }
        let variant = self.cpu_variant_spec.as_ref().map_or(CpuVariant::Cmos65C02, CpuVariantSpec::to_cpu_variant);
        let vector_resolver = bus_config.take_vector_resolver();
        let bus = bus_config.build();
        let mut builder = Cpu::builder(variant)
            .clock_speed(self.clock_speed_hz.map_or(ClockSpeed::unlimited(), ClockSpeed::hz))
            .bus(bus);
        if let Some(resolver) = vector_resolver {
            builder = builder.vector_resolver(resolver);
        }
        let cpu = builder.build().map_err(BuildError::Cpu)?;
        let id_allocator = *id_allocator.lock().unwrap();
        Ok(EmulatorSession {
            cpu, error_receiver, id_allocator
        })
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::bus::DeviceIdAllocator;
    use crate::emulator::{BusConfigError, DeviceModule, IdentityVectorResolver};
    use figment::value::Value;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A test-only device module that installs an `IdentityVectorResolver` on the
    /// `BusConfig` it's handed, used to exercise the "at most one PIC" rule.
    #[derive(Clone)]
    struct ResolverInstallerModule;

    impl DeviceModule for ResolverInstallerModule {
        fn name(&self) -> &'static str {
            "resolver-installer"
        }

        async fn instantiate(&self, bus_config: BusConfig, _address: u16,
                             _attributes: &HashMap<String, Value>, _context: &InstantiationContext,
                             _id_allocator: Arc<Mutex<DeviceIdAllocator>>)
                -> Result<BusConfig, DeviceModuleError> {
            bus_config.vector_resolver(Box::new(IdentityVectorResolver))
                .map_err(DeviceModuleError::BusConfig)
        }
    }

    #[tokio::test]
    async fn build_fails_when_two_device_specs_install_a_vector_resolver() {
        let mut registry = DeviceRegistry::new();
        registry.register(ResolverInstallerModule);
        let config = Config {
            cpu_variant_spec: None,
            clock_speed_hz: None,
            devices: Some(vec![
                "resolver-installer@0x1000".parse().unwrap(),
                "resolver-installer@0x2000".parse().unwrap(),
            ]),
        };
        let err = config.build(&registry).await.err().unwrap();
        assert!(matches!(
            err,
            BuildError::Device { source: DeviceModuleError::BusConfig(BusConfigError::DuplicateVectorResolver), .. }
        ));
    }

    #[tokio::test]
    async fn build_installs_resolver_from_a_single_device_spec() {
        let mut registry = DeviceRegistry::new();
        registry.register(ResolverInstallerModule);
        let config = Config {
            cpu_variant_spec: None,
            clock_speed_hz: None,
            devices: Some(vec!["resolver-installer@0x1000".parse().unwrap()]),
        };
        assert!(config.build(&registry).await.is_ok());
    }
}
