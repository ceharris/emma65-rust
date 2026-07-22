use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::{Deserialize, Serialize};

use crate::emulator::{AddressRange, BusConfig, DeviceId};
use crate::emulator::device::Lfsr16;
use super::{DeviceModule, InstantiationContext, DeviceModuleError};

/// Number of bytes this device occupies in the bus address space.
const BUS_SIZE: u16 = 2;

/// Selects when the LFSR advances its state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum AdvanceMode {
    /// The LFSR advances once per CPU clock cycle via `tick()`.
    Continuous,
    /// The LFSR advances only when the LOW output register is read.
    Step,
}

impl Display for AdvanceMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AdvanceMode::Continuous => write!(f, "continuous"),
            AdvanceMode::Step => write!(f, "step"),
        }
    }
}

impl From<AdvanceMode> for String {
    fn from(m: AdvanceMode) -> Self {
        m.to_string()
    }
}

impl TryFrom<String> for AdvanceMode {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl FromStr for AdvanceMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "continuous" => Ok(AdvanceMode::Continuous),
            "step" => Ok(AdvanceMode::Step),
            _ => Err(format!("invalid advance mode '{s}'; expected 'continuous' or 'step'")),
        }
    }
}

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
    mode: Option<AdvanceMode>,
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
        let continuous = !matches!(config.mode, Some(AdvanceMode::Step));

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
