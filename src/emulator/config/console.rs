use std::collections::HashMap;
use figment::value::{Dict, Value};
use figment::providers::Serialized;
use serde::Deserialize;

use crate::emulator::{AddressRange, BusConfig, DeviceId};
use crate::emulator::device::console::Console;
use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 2;

/// Buffered console device module.
#[derive(Clone)]
pub struct ConsoleModule;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConsoleAttributes {
    #[serde(rename = "break", skip_serializing_if = "Option::is_none")]
    break_key: Option <u8>,
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for ConsoleModule {

    fn name(&self) -> &'static str { "console" }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: ConsoleAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;

        let device_id = DeviceId(address as u32);

        let console = {
            let mut dev = Console::new(self.name()).with_address(address);
            let injected = context.console_transport.as_ref()
                .and_then(|slot| slot.lock().ok()?.take());
            if let Some(transport) = injected {
                dev.attach_transport(transport);
            } else if let Some(transport_spec) = transport_spec {
                let transport = transport_spec
                    .to_transport().await
                    .map_err(DeviceModuleError::Transport)?;
                dev.attach_transport(transport);
            }
            if let Some(sender) = &context.error_sender {
                dev.set_error_sender(sender.clone(), device_id);
            }
            if let Some(break_key) = config.break_key {
                dev.set_break_key(break_key);
            }
            dev
        };

        bus_config.device(
            AddressRange::new(address, address + (BUS_SIZE - 1)),
            device_id, Box::new(console))
            .map_err(DeviceModuleError::BusConfig)
    }

}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use super::*;
    use crate::emulator::transport::{PipeTransport, Transport};

    #[tokio::test]
    async fn instantiate_with_injected_transport() {
        let (local, mut remote) = PipeTransport::pair().unwrap();
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            console_transport: Some(Arc::new(Mutex::new(Some(Box::new(local))))),
        };
        let bus_config = ConsoleModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context,
        ).await.unwrap();

        let mut bus = bus_config.build();
        bus.write(0xFFF8, 0x41).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x41));
    }

    #[tokio::test]
    async fn injected_transport_is_consumed() {
        let (local, _remote) = PipeTransport::pair().unwrap();
        let slot = Arc::new(Mutex::new(Some(Box::new(local) as Box<dyn crate::emulator::transport::Transport>)));
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            console_transport: Some(Arc::clone(&slot)),
        };
        let _bus_config = ConsoleModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context,
        ).await.unwrap();

        assert!(slot.lock().unwrap().is_none(), "transport should be taken after instantiation");
    }

    #[tokio::test]
    async fn instantiate_without_injected_transport_and_no_spec() {
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            console_transport: None,
        };
        let bus_config = ConsoleModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context,
        ).await.unwrap();

        let mut bus = bus_config.build();
        // Console with no transport: write is silent, read returns 0
        bus.write(0xFFF8, 0x42).unwrap();
        assert_eq!(bus.read(0xFFF9).unwrap(), 0);
    }

}