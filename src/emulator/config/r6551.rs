use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::R6551;
use crate::emulator::{AddressRange, BusConfig};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 4;

// Default IRQ to assign to this device
const DEFAULT_IRQ: u32 = 5;


/// R6551 Asynchronous Communications Interface Adapter module.
#[derive(Clone)]
pub struct R6551Module;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct R6551Attributes {
    with_tdre_bug: Option<bool>,
    with_overrun: Option<bool>,
    transport: Option<TransportSpecFormat>,
    irq: Option<u32>,
}

impl DeviceModule for R6551Module {

    fn name(&self) -> &'static str {
        "acia/6551"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: R6551Attributes = figment::Figment::new()
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
            let mut dev = R6551::new(self.name())
                .with_address(address)
                .with_tdre_bug(config.with_tdre_bug.unwrap_or(false))
                .with_overrun(config.with_overrun.unwrap_or(false));
            if let Some(hz) = context.clock_hz {
                dev = dev.with_clock_hz(hz);
            }
            if let Some(spec) = transport_spec {
                let (transport, relay) = spec
                    .to_transport_with_reporter(context.pipe_exit_reporter(device_id)).await
                    .map_err(DeviceModuleError::Transport)?;
                dev.attach_transport(transport, relay);
            }
            dev
        };

        bus_config.device(
            AddressRange::new(address, address + (BUS_SIZE - 1)),
            device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }

}