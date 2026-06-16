use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::Deserialize;

use crate::emulator::{Acia6551, AddressRange, BusConfig, DeviceId};
use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};

// Bus ID for the device.
const DEVICE_ID: DeviceId = DeviceId(2);

// Type name used in registering the device
const DEVICE_TYPE: &str = "acia/6551";

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 4;


/// 6551 Asynchronous Communications Interface Adapter module.
#[derive(Clone)]
pub struct Acia6551Module;

#[derive(Deserialize)]
pub struct Acia6551Attributes {
    with_tdre_bug: bool,
    with_overrun: bool,
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for Acia6551Module {

    fn name(&self) -> &'static str {
        DEVICE_TYPE
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: Acia6551Attributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;

        let device = {
            let mut dev = Acia6551::new()
                .with_tdre_bug(config.with_tdre_bug)
                .with_overrun(config.with_overrun);
            if let Some(hz) = context.clock_hz {
                dev = dev.with_clock_hz(hz);
            }
            if let Some(spec) = transport_spec {
                let transport = spec.to_transport().await
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
            DEVICE_ID, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }

}