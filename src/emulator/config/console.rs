use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{DeviceModule, DeviceModuleError, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::Console;
use crate::emulator::transport::TransportRelay;
use crate::emulator::{AddressRange, BusConfig, IoDevice};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 2;

// Default device/IRQ identifier
const DEFAULT_IRQ: u32 = 3;

/// Buffered console device module.
#[derive(Clone)]
pub struct ConsoleModule;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConsoleAttributes {
    #[serde(rename = "break", skip_serializing_if = "Option::is_none")]
    break_key: Option <u8>,
    transport: Option<TransportSpecFormat>,
    irq: Option<u32>,
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

        let irq = config.irq.unwrap_or(DEFAULT_IRQ);
        let device_id = id_allocator.lock().unwrap()
            .for_irq(irq)
            .map_err(DeviceModuleError::BusConfig)?;

        let console = {
            let mut dev = Console::new(self.name()).with_address(address);
            if let Some(transport_spec) = transport_spec {
                let (transport, relay) = transport_spec
                    .to_transport_with_reporter(context.transport_reporter(dev.identity()), context.pipe_exit_reporter(dev.identity())).await
                    .map_err(DeviceModuleError::Transport)?;
                dev.attach_transport(transport, relay);
            } else if let Some((transport, relay, reporter)) = context.console_transport.as_ref()
                    .and_then(|slot| slot.lock().ok()?.take()) {
                // The reporter was constructed via `TransportReporter::pending`
                // before this device existed (the `TransportSlot`
                // injection path builds its transport ahead of
                // `DeviceModule::instantiate`); bind it now so every clone
                // already handed to the transport's background machinery
                // starts reporting under the right identity.
                reporter.bind(dev.identity());
                dev.attach_transport(transport, TransportRelay::Byte(relay));
            }
            if let Some(break_key) = config.break_key {
                dev.set_break_key(break_key);
            }
            if let Some(sender) = &context.log_sender {
                dev.set_log_sender(sender.clone());
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
    use crate::emulator::TransportSlot;
    use crate::emulator::transport::{InternalPipeTransport, Transport, TransportReporter};
    use std::sync::{Arc, Mutex};

    /// Builds a `TransportSlot` the same way `main.rs`/`load_session` do: a
    /// real relay-backed `InternalPipeTransport` pair, plus an unbound
    /// `TransportReporter` (device name bound later, by `instantiate`).
    /// Returns the slot, the reporter (cloned, so the caller can exercise it
    /// after `instantiate` binds the original), and the pair's remote end.
    fn injected_slot() -> (TransportSlot, TransportReporter, InternalPipeTransport) {
        let reporter = TransportReporter::pending(None);
        let ((local, relay), remote) = InternalPipeTransport::pair(reporter.clone()).unwrap();
        let slot = Arc::new(Mutex::new(Some((Box::new(local) as Box<dyn Transport>, relay, reporter.clone()))));
        (slot, reporter, remote)
    }

    #[tokio::test]
    async fn instantiate_with_injected_transport() {
        let (slot, _reporter, mut remote) = injected_slot();
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
            console_transport: Some(slot),
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
        let (slot, _reporter, _remote) = injected_slot();
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
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
        let (slot, _reporter, _remote) = injected_slot();
        let mut attributes = HashMap::new();
        // pipe transport is the only variant we can create without an OS resource in a unit test
        attributes.insert(
            "transport".to_string(),
            Value::from("pipe:/usr/bin/cat"),
        );
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
            console_transport: Some(Arc::clone(&slot)),
        };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _result = ConsoleModule.instantiate(
            BusConfig::new(), 0xFFF8, &attributes, &context, id_allocator).await;

        assert!(slot.lock().unwrap().is_some(), "context transport should not be consumed when transport_spec is set");
    }

    #[tokio::test]
    async fn injected_transport_reporter_is_bound_to_device_name() {
        let (error_sender, mut error_receiver) = crate::emulator::device_event_channel();
        let reporter = TransportReporter::pending(Some(error_sender));
        let ((local, relay), _remote) = InternalPipeTransport::pair(reporter.clone()).unwrap();
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
            console_transport: Some(Arc::new(Mutex::new(Some((
                Box::new(local) as Box<dyn Transport>, relay, reporter.clone(),
            ))))),
        };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _bus_config = ConsoleModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context, id_allocator).await.unwrap();

        // Before `instantiate` runs, this reporter is unbound (the device doesn't exist yet at
        // construction) so every reporting call is a silent no-op; `instantiate` must call
        // `bind` on the copy it pulls out of the slot for this clone to start reporting too.
        reporter.report_connected(Some("test-peer".to_string()));
        match error_receiver.recv().await {
            Some(crate::emulator::DeviceEvent::TransportConnected { peer, .. }) =>
                assert_eq!(peer, Some("test-peer".to_string())),
            other => panic!("expected TransportConnected event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn instantiate_without_injected_transport_and_no_spec() {
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
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