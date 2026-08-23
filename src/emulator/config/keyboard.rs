use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::{DeviceModule, DeviceModuleError, InstantiationContext};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::Keyboard;
use crate::emulator::transport::TransportRelay;
use crate::emulator::{AddressRange, BusConfig, IoDevice};

// Size of the device on the bus (in contiguous bytes of address space)
const BUS_SIZE: u16 = 2;

// Default device/IRQ identifier
const DEFAULT_IRQ: u32 = 7;

/// Memory-mapped keyboard input device module.
#[derive(Clone)]
pub struct KeyboardModule;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct KeyboardAttributes {
    #[serde(rename = "break", skip_serializing_if = "Option::is_none")]
    break_key: Option<u8>,
    irq: Option<u32>,
}

impl DeviceModule for KeyboardModule {

    fn name(&self) -> &'static str { "keyboard" }

    async fn instantiate(&self,
                         bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>,
                         context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: KeyboardAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let irq = config.irq.unwrap_or(DEFAULT_IRQ);
        let device_id = id_allocator.lock().unwrap()
            .for_irq(irq)
            .map_err(DeviceModuleError::BusConfig)?;

        let keyboard = {
            let mut dev = Keyboard::new(self.name()).with_address(address);
            if let Some((transport, relay, reporter)) = context.keyboard_transport.as_ref()
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
            device_id, Box::new(keyboard))
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

    fn context_with_keyboard_slot(slot: TransportSlot) -> InstantiationContext {
        InstantiationContext {
            clock_hz: None,
            error_sender: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
            console_transport: None,
            keyboard_transport: Some(slot),
        }
    }

    #[tokio::test]
    async fn instantiate_with_injected_transport() {
        let (slot, _reporter, mut remote) = injected_slot();
        let context = context_with_keyboard_slot(slot);
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = KeyboardModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context, id_allocator).await.unwrap();

        let mut bus = bus_config.build();
        remote.send(0x42);
        std::thread::sleep(std::time::Duration::from_millis(5));
        bus.tick_devices(1);
        assert_eq!(bus.read(0xFFF9).unwrap(), 0x42);
    }

    #[tokio::test]
    async fn injected_transport_is_consumed() {
        let (slot, _reporter, _remote) = injected_slot();
        let context = context_with_keyboard_slot(Arc::clone(&slot));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _bus_config = KeyboardModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context, id_allocator).await.unwrap();

        assert!(slot.lock().unwrap().is_none(), "transport should be taken after instantiation");
    }

    #[tokio::test]
    async fn injected_transport_reporter_is_bound_to_device_name() {
        let (error_sender, mut error_receiver) = crate::emulator::device_event_channel();
        let reporter = TransportReporter::pending(Some(error_sender));
        let ((local, relay), _remote) = InternalPipeTransport::pair(reporter.clone()).unwrap();
        let slot = Arc::new(Mutex::new(Some((
            Box::new(local) as Box<dyn Transport>, relay, reporter.clone(),
        ))));
        let context = context_with_keyboard_slot(slot);
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _bus_config = KeyboardModule.instantiate(
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
    async fn instantiate_without_injected_transport() {
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
            console_transport: None,
            keyboard_transport: None,
        };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = KeyboardModule.instantiate(
            BusConfig::new(), 0xFFF8, &HashMap::new(), &context, id_allocator).await.unwrap();

        let mut bus = bus_config.build();
        // Keyboard with no transport attached: reads simply return 0
        assert_eq!(bus.read(0xFFF9).unwrap(), 0);
    }

    #[tokio::test]
    async fn instantiate_applies_break_key() {
        let (slot, _reporter, mut remote) = injected_slot();
        let mut attributes = HashMap::new();
        attributes.insert("break".to_string(), Value::from(3u8));
        let context = context_with_keyboard_slot(slot);
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = KeyboardModule.instantiate(
            BusConfig::new(), 0xFFF8, &attributes, &context, id_allocator).await.unwrap();

        let mut bus = bus_config.build();
        remote.send(0x03);
        std::thread::sleep(std::time::Duration::from_millis(5));
        bus.tick_devices(1);
        assert!(
            bus.device_interrupt_states().any(|s| s.irq_active),
            "expected break key to assert IRQ"
        );
    }

    #[tokio::test]
    async fn instantiate_uses_configured_irq() {
        let mut attributes = HashMap::new();
        attributes.insert("irq".to_string(), Value::from(9u32));
        let context = InstantiationContext {
            clock_hz: None,
            error_sender: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
            console_transport: None,
            keyboard_transport: None,
        };
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _bus_config = KeyboardModule.instantiate(
            BusConfig::new(), 0xFFF8, &attributes, &context, id_allocator.clone()).await.unwrap();

        // IRQ 9 is now taken -- allocating it again should fail.
        assert!(id_allocator.lock().unwrap().for_irq(9).is_err());
    }

}
