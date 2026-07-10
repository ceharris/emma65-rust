use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::Deserialize;

use crate::emulator::{AddressRange, BusConfig, DeviceId};
use crate::emulator::device::Mc6850;
use super::{DeviceModule, InstantiationContext, DeviceModuleError, TransportSpec, TransportSpecFormat};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 2;


/// MC6850 Asynchronous Communications Interface Adapter module.
#[derive(Clone)]
pub struct Mc6850Module;

#[derive(Deserialize)]
pub struct Mc6850Attributes {
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for Mc6850Module {

    fn name(&self) -> &'static str {
        "acia/6850"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16, 
                         attributes: &HashMap<String, Value>, context: &InstantiationContext)
            -> Result<BusConfig, DeviceModuleError> {
        
        let attrs = Dict::from_iter(attributes.clone());
        let config: Mc6850Attributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;

        let device_id = DeviceId(address as u32);
        let device = {
            let mut dev = Mc6850::new(self.name()).with_address(address);
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