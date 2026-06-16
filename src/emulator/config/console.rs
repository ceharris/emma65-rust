use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::Deserialize;

use crate::emulator::{AddressRange, BusConfig, BusConfigError, Console, DeviceId};

use super::{DeviceModule, TransportSpec};

// Bus ID for the console device.
const CONSOLE_DEVICE_ID: DeviceId = DeviceId(0);

// Size of the console device on the bus (in contiguous bytes of address space)
const CONSOLE_BUS_SIZE: u16 = 2;

/// Console device module.
pub struct ConsoleModule;

#[derive(Deserialize)]
pub struct ConsoleAttributes {
    transport: Option<TransportSpec>,
}

impl DeviceModule for ConsoleModule {

    fn name(&self) -> &'static str {
        "console"
    }

    fn instantiate(&self, bus_config: BusConfig,
                   address: u16, attributes: &HashMap<String, Value>) -> Result<BusConfig, BusConfigError> {
        let attrs = Dict::from_iter(attributes.clone());
        let config: ConsoleAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| BusConfigError::InvalidConfigParams { message: format!("{e}")})?;

        let console = Console::new();

        Ok(bus_config.device(
            AddressRange::new(address, address + (CONSOLE_BUS_SIZE - 1)),
            CONSOLE_DEVICE_ID,
            Box::new(console)).expect("failed to attach console device"))
    }

}