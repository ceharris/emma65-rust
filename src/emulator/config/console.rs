use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::Console;
use crate::emulator::{AddressRange, BusConfig};

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

    async fn instantiate(&self,
                         bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>,
                         context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
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

        let device_id = id_allocator.lock().unwrap().next(true);

        let console = {
            let mut dev = Console::new(self.name()).with_address(address);
            if let Some(transport_spec) = transport_spec {
                // Not yet migrated to hold/drain a relay (transport relay
                // redesign plan, checklist item 9.1); leak it until then —
                // see `TransportSpec::to_transport`'s Pty arm doc comment
                // for why this is safe.
                let (transport, relay) = transport_spec
                    .to_transport_with_reporter(context.pipe_exit_reporter(device_id)).await
                    .map_err(DeviceModuleError::Transport)?;
                std::mem::forget(relay);
                dev.attach_transport(transport);
            } else if let Some(injected) = context.console_transport.as_ref()
                    .and_then(|slot| slot.lock().ok()?.take()) {
                dev.attach_transport(injected);
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
    use super::*;
    use crate::emulator::transport::{InternalPipeTransport, Transport};
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn instantiate_with_injected_transport() {
        let (local, mut remote) = InternalPipeTransport::pair().unwrap();
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            console_transport: Some(Arc::new(Mutex::new(Some(Box::new(local))))),
        };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = ConsoleModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context, id_allocator).await.unwrap();

        let mut bus = bus_config.build();
        bus.write(0xFFF8, 0x41).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x41));
    }

    #[tokio::test]
    async fn injected_transport_is_consumed() {
        let (local, _remote) = InternalPipeTransport::pair().unwrap();
        let slot = Arc::new(Mutex::new(Some(Box::new(local) as Box<dyn crate::emulator::transport::Transport>)));
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            console_transport: Some(Arc::clone(&slot)),
        };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _bus_config = ConsoleModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context, id_allocator).await.unwrap();

        assert!(slot.lock().unwrap().is_none(), "transport should be taken after instantiation");
    }

    #[tokio::test]
    async fn injected_transport_ignored_when_transport_spec_is_set() {
        // When a transport= attribute is configured, the context transport must not be consumed,
        // so that the caller can detect whether stdio will be used (e.g. to enter raw mode).
        let (local, _remote) = InternalPipeTransport::pair().unwrap();
        let slot = Arc::new(Mutex::new(Some(Box::new(local) as Box<dyn crate::emulator::transport::Transport>)));
        let mut attributes = HashMap::new();
        // pipe transport is the only variant we can create without an OS resource in a unit test
        attributes.insert(
            "transport".to_string(),
            Value::from("pipe:/usr/bin/cat"),
        );
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            console_transport: Some(Arc::clone(&slot)),
        };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _result = ConsoleModule.instantiate(
            BusConfig::new(), 0xFFF8, &attributes, &context, id_allocator).await;

        assert!(slot.lock().unwrap().is_some(), "context transport should not be consumed when transport_spec is set");
    }

    #[tokio::test]
    async fn instantiate_without_injected_transport_and_no_spec() {
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            console_transport: None,
        };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = ConsoleModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context, id_allocator).await.unwrap();

        let mut bus = bus_config.build();
        // Console with no transport: write is silent, read returns 0
        bus.write(0xFFF8, 0x42).unwrap();
        assert_eq!(bus.read(0xFFF9).unwrap(), 0);
    }

}