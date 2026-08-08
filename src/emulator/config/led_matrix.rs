use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::led_matrix::LedMatrix;
use crate::emulator::{AddressRange, BusConfig, TransportRelay};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 8;

// Default IRQ to assign to this device
const DEFAULT_IRQ: u32 = 6;


/// RGB LED matrix display adapter module.
#[derive(Clone)]
pub struct LedMatrixModule;

#[derive(Deserialize)]
pub struct LedMatrixAttributes {
    transport: Option<TransportSpecFormat>,
    irq: Option<u32>,
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

        let irq = config.irq.unwrap_or(DEFAULT_IRQ);
        let device_id = id_allocator.lock().unwrap()
            .for_irq(irq)
            .map_err(DeviceModuleError::BusConfig)?;

        let device = {
            let mut dev = LedMatrix::new(self.name(), AddressRange::new(address, address + (BUS_SIZE - 1)));
            if let Some(transport_spec) = transport_spec {
                let (transport, relay) = transport_spec
                    .to_transport_with_reporter(context.pipe_exit_reporter(device_id)).await
                    .map_err(DeviceModuleError::Transport)?;
                let tagged_relay = match relay {
                    TransportRelay::Tagged(relay) => relay,
                    TransportRelay::Byte(_) => return Err(DeviceModuleError::Config(
                        "display/matrix requires a multipoint transport (tcp/unix); \
                         point-to-point transports (pty/pipe) don't support per-client tagging"
                            .to_string())),
                };
                dev.attach_transport(transport, tagged_relay);
            }
            dev
        };

        bus_config.device(
            AddressRange::new(address, address + (BUS_SIZE - 1)),
            device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }

}