use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::Mc6850;
use crate::emulator::{AddressRange, BusConfig, IoDevice};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 2;

// Default IRQ to assign to this device
const DEFAULT_IRQ: u32 = 4;

/// MC6850 Asynchronous Communications Interface Adapter module.
#[derive(Clone)]
pub struct Mc6850Module;

#[derive(Deserialize)]
pub struct Mc6850Attributes {
    transport: Option<TransportSpecFormat>,
    irq: Option<u32>,
}

impl DeviceModule for Mc6850Module {

    fn name(&self) -> &'static str {
        "acia/6850"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16, 
                         attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
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

        let irq = config.irq.unwrap_or(DEFAULT_IRQ);
        let device_id = id_allocator.lock().unwrap()
            .for_irq(irq)
            .map_err(DeviceModuleError::BusConfig)?;

        let device = {
            let mut dev = Mc6850::new(self.name()).with_address(address);
            if let Some(transport_spec) = transport_spec {
                let (transport, relay) = transport_spec
                    .to_transport_with_reporter(context.transport_reporter(dev.identity()), context.pipe_exit_reporter(dev.identity())).await
                    .map_err(DeviceModuleError::Transport)?;
                dev.attach_transport(transport, relay);
            }
            if let Some(sender) = &context.log_sender {
                dev.set_log_sender(sender.clone());
            }
            dev
        };

        bus_config.device(
            AddressRange::new(address, address + (BUS_SIZE - 1)),
            device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }

}