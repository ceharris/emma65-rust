use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::Deserialize;

use crate::emulator::{AddressRange, BusConfig, DeviceId};
use crate::emulator::device::R6551;
use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 4;


/// R6551 Asynchronous Communications Interface Adapter module.
#[derive(Clone)]
pub struct R6551Module;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct R6551Attributes {
    with_tdre_bug: Option<bool>,
    with_overrun: Option<bool>,
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for R6551Module {

    fn name(&self) -> &'static str {
        "acia/6551"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext)
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

        let device_id = DeviceId(address as u32);
        let device = {
            let mut dev = R6551::new(self.name())
                .with_address(address)
                .with_tdre_bug(config.with_tdre_bug.unwrap_or(false))
                .with_overrun(config.with_overrun.unwrap_or(false));
            if let Some(hz) = context.clock_hz {
                dev = dev.with_clock_hz(hz);
            }
            if let Some(spec) = transport_spec {
                let transport = spec
                    .to_transport_with_reporter(context.pipe_exit_reporter(device_id)).await
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