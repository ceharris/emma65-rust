use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::{Mc6840, ProtocolMessageEncoding};
use crate::emulator::{AddressRange, BusConfig};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 8;


/// MC6840 Programmable Timer Module (PTM)
#[derive(Clone)]
pub struct Mc6840Module;

#[derive(Deserialize)]
pub struct Mc6840Attributes {
    protocol: Option<ProtocolMessageEncoding>,
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for Mc6840Module {

    fn name(&self) -> &'static str {
        "ptm/6840"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16, 
                         attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
            -> Result<BusConfig, DeviceModuleError> {
        
        let attrs = Dict::from_iter(attributes.clone());
        let config: Mc6840Attributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;

        let device_id = id_allocator.lock().unwrap().next(true);
        let device = {
            let mut dev = Mc6840::new(self.name()).with_address(address);
            if let Some(protocol) = config.protocol {
                dev = dev.with_protocol(protocol);
            }
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