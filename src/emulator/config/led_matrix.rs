use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::led_matrix::LedMatrix;
use crate::emulator::{AddressRange, BusConfig};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 8;


/// RGB LED matrix display adapter module.
#[derive(Clone)]
pub struct LedMatrixModule;

#[derive(Deserialize)]
pub struct LedMatrixAttributes {
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for LedMatrixModule {

    fn name(&self) -> &'static str {
        "display/matrix"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16, 
                         attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
            -> Result<BusConfig, DeviceModuleError> {
        
        let attrs = Dict::from_iter(attributes.clone());
        let config: LedMatrixAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;

        let device_id = id_allocator.lock().unwrap().next(true);
        let device = {
            let mut dev = LedMatrix::new(self.name(), AddressRange::new(address, address + (BUS_SIZE - 1)));
            if let Some(transport_spec) = transport_spec {
                // Not yet migrated to hold/drain a relay (transport relay
                // redesign plan, checklist item 9.1/9.3); leak it until then
                // — see `TransportSpec::to_transport`'s Pty arm doc comment
                // for why this is safe.
                let (transport, relay) = transport_spec
                    .to_transport_with_reporter(context.pipe_exit_reporter(device_id)).await
                    .map_err(DeviceModuleError::Transport)?;
                std::mem::forget(relay);
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