use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::Deserialize;

use crate::emulator::{AddressRange, BusConfig, DeviceId, Via6522};
use super::{DeviceModule, InstantiationContext, DeviceModuleError, TransportSpec, TransportSpecFormat};

// Type name used in registering the device
const DEVICE_TYPE: &str = "via/6522";

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 16;


/// 6522 Versatile Interface Adapter module.
#[derive(Clone)]
pub struct Via6522Module;

#[derive(Deserialize)]
pub struct Via6522Attributes {
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for Via6522Module {

    fn name(&self) -> &'static str {
        DEVICE_TYPE
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16, 
                         attributes: &HashMap<String, Value>, context: &InstantiationContext)
            -> Result<BusConfig, DeviceModuleError> {
        
        let attrs = Dict::from_iter(attributes.clone());
        let config: Via6522Attributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;

        let device_id = DeviceId(address as u32);
        let device = {
            let mut dev = Via6522::new();
            if let Some(transport_spec) = transport_spec {
                let transport = transport_spec
                    .to_transport().await
                    .map_err(DeviceModuleError::Transport)?;
                dev.attach_transport(transport);
            }
            if let Some(sender) = &context.error_sender {
                dev.set_error_sender(sender.clone(), device_id);
            }
            dev
        };

        bus_config.device(
            AddressRange::new(address, address + (BUS_SIZE - 1)),
            device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }

}