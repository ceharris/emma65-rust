use super::palette;
use super::{DeviceModule, DeviceModuleError, DisplayGeometry, ExpandedPathBuf, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::display::compositing::default_palette;
use crate::emulator::device::display::font::{FONT_BYTES, Font};
use crate::emulator::device::display::{CharDisplay, DEFAULT_COLUMNS, DEFAULT_FRAME_RATE_HZ, DEFAULT_ROWS};
use crate::emulator::transport::TransportRelay;
use crate::emulator::{AddressRange, BusConfig, IoDevice};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Type name used in registering the character display as a device.
const DEVICE_TYPE: &str = "display";

// Default device/IRQ identifier for the optional keyboard sub-range, reused verbatim from the
// deleted `KeyboardModule`.
const DEFAULT_KEYBOARD_IRQ: u32 = 7;

/// Memory-mapped character/color-cell display device module.
#[derive(Clone)]
pub struct CharDisplayModule;

#[derive(Deserialize)]
struct CharDisplayAttributes {
    columns: Option<u32>,
    rows: Option<u32>,
    palette: Option<ExpandedPathBuf>,
    font: Option<ExpandedPathBuf>,
    double_buffered: Option<bool>,
    frame_rate_hz: Option<u32>,
    transport: Option<TransportSpecFormat>,
    #[serde(rename = "keyboard-address")]
    keyboard_address: Option<u16>,
    #[serde(rename = "break")]
    break_key: Option<u8>,
    irq: Option<u32>,
}

impl DeviceModule for CharDisplayModule {

    fn name(&self) -> &'static str {
        DEVICE_TYPE
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: CharDisplayAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let columns = config.columns.unwrap_or(DEFAULT_COLUMNS);
        let rows = config.rows.unwrap_or(DEFAULT_ROWS);
        if columns == 0 || rows == 0 {
            return Err(DeviceModuleError::Config(
                "display: columns and rows must both be positive".to_string()));
        }

        let palette = match &config.palette {
            Some(path) => {
                let text = tokio::fs::read_to_string(path).await.map_err(DeviceModuleError::Io)?;
                palette::parse(&text).map_err(|e| DeviceModuleError::Config(e.to_string()))?
            }
            None => default_palette(),
        };

        let font = match &config.font {
            Some(path) => {
                let data = tokio::fs::read(path).await.map_err(DeviceModuleError::Io)?;
                Font::from_bytes(&data).map_err(|e| DeviceModuleError::Config(e.to_string()))?
            }
            None => Font::default(),
        };

        let double_buffered = config.double_buffered.unwrap_or(true);
        let frame_rate_hz = config.frame_rate_hz.unwrap_or(DEFAULT_FRAME_RATE_HZ);

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;
        // The external protocol's per-vsync bulk send (`doc/char-display-external-protocol.md`)
        // relies on `Transport::send_bytes`'s all-or-nothing contract, which only `PipeTransport`
        // provides (see Unit 1 of the SDL2 display peripheral plan) -- reject any other kind
        // rather than silently desyncing the stream on the first dropped frame.
        if let Some(spec) = &transport_spec
            && !matches!(spec, TransportSpec::Pipe { .. })
        {
            return Err(DeviceModuleError::Config(
                "display requires a pipe transport; \
                 tcp/unix/pty transports don't support the atomic bulk-send this protocol needs"
                    .to_string()));
        }

        let bus_size = 2 * (columns * rows) + 2;
        let address_range = AddressRange::new(address, address + (bus_size - 1) as u16);

        // Only IRQ-capable when a keyboard sub-range is configured (design doc §1); a plain
        // `next_available()` ID falls outside the IRQ bitmask range and is silently ignored by
        // `Bus::device_interrupt_states`/`InterruptController::poll_devices`, so a device with no
        // keyboard range keeps the cheaper, non-IRQ allocation.
        let device_id = if config.keyboard_address.is_some() {
            id_allocator.lock().unwrap()
                .for_irq(config.irq.unwrap_or(DEFAULT_KEYBOARD_IRQ))
                .map_err(DeviceModuleError::BusConfig)?
        } else {
            id_allocator.lock().unwrap().next_available()
        };

        let mut device = CharDisplay::new(
            self.name(),
            address_range,
            columns,
            rows,
            double_buffered,
            context.clock_hz,
            frame_rate_hz,
            font,
            palette,
        );

        if let Some(sender) = &context.log_sender {
            device.set_log_sender(sender.clone());
        }

        let keyboard_range = config.keyboard_address
            .map(|addr| AddressRange::new(addr, addr + 1));
        if let Some(range) = keyboard_range {
            device = device.with_keyboard_range(range);
            if let Some(break_key) = config.break_key {
                device.set_break_key(break_key);
            }
        }

        if let Some(transport_spec) = transport_spec {
            // Size the pipe's ring to comfortably fit the larger of the two messages this
            // protocol ever sends -- the one-time header (dominated by the font) and each
            // per-vsync frame -- so `send_bytes`'s atomic push never has to drop either one.
            let header_len = 4 + 1 + 4 + 4 + 4 + 2 + FONT_BYTES;
            let frame_len = 2 * (columns * rows) as usize + device.palette().len() * 3;
            let capacity = header_len.max(frame_len);

            let (transport, relay) = transport_spec
                .to_transport_with_reporter_and_capacity(
                    context.transport_reporter(device.identity()),
                    context.pipe_exit_reporter(device.identity()),
                    Some(capacity))
                .await
                .map_err(DeviceModuleError::Transport)?;
            device.attach_external_transport(transport, relay);
        }

        // Gated on `keyboard_address` being configured: an earlier `display` device with no
        // keyboard configured must not consume and discard the debugger's only keyboard slot,
        // starving a later device that does configure one.
        if keyboard_range.is_some()
            && let Some((transport, relay, reporter)) = context.keyboard_transport.as_ref()
                .and_then(|slot| slot.lock().ok()?.take())
        {
            // The reporter was constructed via `TransportReporter::pending` before this device
            // existed (the `TransportSlot` injection path builds its transport ahead of
            // `DeviceModule::instantiate`); bind it now so every clone already handed to the
            // transport's background machinery starts reporting under the right identity.
            reporter.bind(device.identity());
            device.attach_keyboard_transport(transport, TransportRelay::Byte(relay));
        }

        // Both slots (design doc §9) are consumed the same way `console_transport` is: present
        // only when a host (the debugger) wants to receive this device's output, absent (a
        // no-op here) for the plain `emma65` CLI.
        if let Some(slot) = &context.display_geometry_sink {
            *slot.lock().unwrap() = Some(DisplayGeometry {
                columns,
                rows,
                pixel_width: columns * 8,
                pixel_height: rows * 8,
                frame_rate_hz,
            });
        }
        if let Some(slot) = &context.display_frame_sink
            && let Some(sender) = slot.lock().unwrap().take()
        {
            device.attach_frame_sink(sender);
        }

        let bus_config = bus_config.device(address_range, device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)?;

        match keyboard_range {
            Some(range) => bus_config.extend_device(range, device_id)
                .map_err(DeviceModuleError::BusConfig),
            None => Ok(bus_config),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> InstantiationContext {
        InstantiationContext {
            clock_hz: None,
            error_sender: None,
            console_transport: None,
            keyboard_transport: None,
            log_sender: None,
            display_frame_sink: None,
            display_geometry_sink: None,
            led_matrix_frame_sink: None,
            led_matrix_geometry_sink: None,
        }
    }

    #[tokio::test]
    async fn instantiate_without_transport_attribute_succeeds() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let result = CharDisplayModule.instantiate(
            BusConfig::new(), 0x8000, &HashMap::new(), &context(), id_allocator).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn rejects_non_pipe_transport_spec() {
        let mut attributes = HashMap::new();
        attributes.insert("transport".to_string(), Value::from("unix:/tmp/emma65_test_display_char.sock"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = CharDisplayModule.instantiate(
            BusConfig::new(), 0x8000, &attributes, &context(), id_allocator).await;

        match result {
            Err(DeviceModuleError::Config(message)) => assert!(message.contains("pipe transport")),
            Err(other) => panic!("expected DeviceModuleError::Config, got a different error variant: {other}"),
            Ok(_) => panic!("expected DeviceModuleError::Config, got Ok"),
        }
    }

    #[tokio::test]
    async fn attaches_pipe_transport_and_sends_header_immediately() {
        let mut attributes = HashMap::new();
        attributes.insert("transport".to_string(), Value::from("pipe:/usr/bin/cat"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = CharDisplayModule.instantiate(
            BusConfig::new(), 0x8000, &attributes, &context(), id_allocator).await;

        // End-to-end smoke test with a real spawned child: confirms the computed ring capacity
        // is accepted by `PipeTransport::spawn_with_capacity` and `attach_external_transport`'s
        // immediate header send doesn't panic against a live pipe.
        assert!(result.is_ok());
    }

    // -- Keyboard sub-range config wiring: mirrors the deleted `KeyboardModule`'s own config-
    // level test suite, now exercised against `CharDisplayModule`. --

    use crate::emulator::transport::{InternalPipeTransport, Transport, TransportReporter};

    /// Builds a `TransportSlot` the same way `main.rs`/`load_session` do: a real relay-backed
    /// `InternalPipeTransport` pair, plus an unbound `TransportReporter` (device name bound
    /// later, by `instantiate`). Returns the slot, and the pair's remote end.
    fn injected_keyboard_slot() -> (super::super::TransportSlot, InternalPipeTransport) {
        let reporter = TransportReporter::pending(None);
        let ((local, relay), remote) = InternalPipeTransport::pair(reporter.clone()).unwrap();
        let slot = Arc::new(Mutex::new(Some((Box::new(local) as Box<dyn Transport>, relay, reporter))));
        (slot, remote)
    }

    fn context_with_keyboard_slot(slot: super::super::TransportSlot) -> InstantiationContext {
        InstantiationContext { keyboard_transport: Some(slot), ..context() }
    }

    #[tokio::test]
    async fn keyboard_address_maps_a_second_range_on_the_same_device() {
        let mut attributes = HashMap::new();
        attributes.insert("keyboard-address".to_string(), Value::from(0x9000u16));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let bus_config = CharDisplayModule.instantiate(
            BusConfig::new(), 0x8000, &attributes, &context(), id_allocator).await.unwrap();
        let mut bus = bus_config.build();

        let _ = bus.write(0x9001, 0x42); // keyboard latch register
        assert_eq!(bus.read(0x9001).unwrap(), 0x42);
        // Confirms one device instance backs both ranges, not two separately-registered ones.
        assert_eq!(bus.device_interrupt_states().count(), 1);
    }

    #[tokio::test]
    async fn keyboard_address_absent_does_not_consume_the_injected_slot() {
        let (slot, _remote) = injected_keyboard_slot();
        let context = context_with_keyboard_slot(Arc::clone(&slot));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let _bus_config = CharDisplayModule.instantiate(
            BusConfig::new(), 0x8000, &HashMap::new(), &context, id_allocator).await.unwrap();

        assert!(slot.lock().unwrap().is_some(), "an unconfigured keyboard sub-range must not \
            starve a later device that configures one");
    }

    #[tokio::test]
    async fn keyboard_address_present_consumes_the_injected_slot_and_applies_break_key() {
        let (slot, mut remote) = injected_keyboard_slot();
        let mut attributes = HashMap::new();
        attributes.insert("keyboard-address".to_string(), Value::from(0x9000u16));
        attributes.insert("break".to_string(), Value::from(3u8));
        let context = context_with_keyboard_slot(slot);
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let bus_config = CharDisplayModule.instantiate(
            BusConfig::new(), 0x8000, &attributes, &context, id_allocator).await.unwrap();
        let mut bus = bus_config.build();

        remote.send(0x03);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        bus.tick_devices(1);
        assert!(
            bus.device_interrupt_states().any(|s| s.irq_active),
            "expected break key to assert IRQ"
        );
    }

    #[tokio::test]
    async fn keyboard_address_uses_configured_irq() {
        let mut attributes = HashMap::new();
        attributes.insert("keyboard-address".to_string(), Value::from(0x9000u16));
        attributes.insert("irq".to_string(), Value::from(9u32));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let _bus_config = CharDisplayModule.instantiate(
            BusConfig::new(), 0x8000, &attributes, &context(), id_allocator.clone()).await.unwrap();

        // IRQ 9 is now taken -- allocating it again should fail.
        assert!(id_allocator.lock().unwrap().for_irq(9).is_err());
    }

    #[tokio::test]
    async fn no_keyboard_address_is_not_irq_capable() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let _bus_config = CharDisplayModule.instantiate(
            BusConfig::new(), 0x8000, &HashMap::new(), &context(), id_allocator.clone()).await.unwrap();

        // The default IRQ line (7, `DEFAULT_KEYBOARD_IRQ`) must remain unclaimed since this
        // device has no keyboard range and so was allocated a plain, non-IRQ device ID.
        assert!(id_allocator.lock().unwrap().for_irq(7).is_ok());
    }
}
