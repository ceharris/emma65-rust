use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::Deserialize;

use crate::emulator::{AddressRange, BusConfig, DeviceId};
use crate::emulator::device::Lfsr16;
use super::{DeviceModule, InstantiationContext, DeviceModuleError};

/// Number of bytes this device occupies in the bus address space.
const BUS_SIZE: u16 = 2;

/// Device module that registers a 16-bit Galois LFSR pseudo-random number generator.
///
/// Accepted device spec attributes:
///
/// | Attribute | Type | Default | Description |
/// |-----------|------|---------|-------------|
/// | `taps` | u16 | `0xB400` | Galois tap mask |
/// | `mode` | string | `"continuous"` | `"continuous"` or `"step"` |
#[derive(Clone)]
pub struct LfsrModule;

#[derive(Deserialize)]
struct LfsrAttributes {
    taps: Option<u16>,
    mode: Option<String>,
}

impl DeviceModule for LfsrModule {

    fn name(&self) -> &'static str {
        "lfsr"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, _context: &InstantiationContext)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: LfsrAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let taps = config.taps.unwrap_or(0xB400);
        let continuous = config.mode.as_deref() != Some("step");

        let device_id = DeviceId(address as u32);
        let device = Lfsr16::new(self.name())
            .with_address(address)
            .with_taps(taps)
            .with_continuous(continuous);

        bus_config.device(
            AddressRange::new(address, address + (BUS_SIZE - 1)),
            device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }
}
