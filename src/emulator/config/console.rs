use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::Deserialize;

use crate::emulator::{AddressRange, BusConfig, Console, DeviceId};
use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};

// Bus ID for the device.
const DEVICE_ID: DeviceId = DeviceId(0);

// Type name used in registering the device
const DEVICE_TYPE: &str = "console";

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 2;

/// Console device module.
#[derive(Clone)]
pub struct ConsoleModule;

#[derive(Deserialize)]
pub struct ConsoleAttributes {
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for ConsoleModule {

    fn name(&self) -> &'static str {
        DEVICE_TYPE
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: ConsoleAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;

        let console = {
            let mut dev = Console::new();
            if let Some(transport_spec) = transport_spec {
                let transport = transport_spec
                    .to_transport().await
                    .map_err(DeviceModuleError::Transport)?;
                dev.attach_transport(transport);
            }
            if let Some(sender) = &context.error_sender {
                dev.set_error_sender(sender.clone(), DEVICE_ID);
            }
            dev
        };

        bus_config.device(
            AddressRange::new(address, address + (BUS_SIZE - 1)),
            DEVICE_ID, Box::new(console))
            .map_err(DeviceModuleError::BusConfig)
    }

}