use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::{ProtocolMessageEncoding, Via6522};
use crate::emulator::{AddressRange, BusConfig, IoDevice, TransportRelay};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 16;

// Default IRQ to assign to this device
const DEFAULT_IRQ: u32 = 1;

/// 6522 Versatile Interface Adapter module.
#[derive(Clone)]
pub struct Via6522Module;

#[derive(Deserialize)]
pub struct Via6522Attributes {
    protocol: Option<ProtocolMessageEncoding>,
    transport: Option<TransportSpecFormat>,
    irq: Option<u32>,
}

impl DeviceModule for Via6522Module {

    fn name(&self) -> &'static str {
        "via/6522"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16, 
                         attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
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

        let irq = config.irq.unwrap_or(DEFAULT_IRQ);
        let device_id = id_allocator.lock().unwrap()
            .for_irq(irq)
            .map_err(DeviceModuleError::BusConfig)?;

        let device = {
            let mut dev = Via6522::new(self.name()).with_address(address);
            if let Some(protocol) = config.protocol {
                dev = dev.with_protocol(protocol);
            }
            if let Some(transport_spec) = transport_spec {
                let (transport, relay) = transport_spec
                    .to_transport_with_reporter(context.transport_reporter(dev.identity()), context.pipe_exit_reporter(dev.identity())).await
                    .map_err(DeviceModuleError::Transport)?;
                let tagged_relay = match relay {
                    TransportRelay::Tagged(relay) => relay,
                    TransportRelay::Byte(_) => return Err(DeviceModuleError::Config(
                        "via/6522 requires a multipoint transport (tcp/unix); \
                         point-to-point transports (pty/pipe) don't support per-client tagging"
                            .to_string())),
                };
                dev.attach_transport(transport, tagged_relay);
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