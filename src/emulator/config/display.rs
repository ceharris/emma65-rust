use super::palette;
use super::{DeviceModule, DeviceModuleError, DisplayGeometry, ExpandedPathBuf, InstantiationContext, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::display::compositing::default_palette;
use crate::emulator::device::display::font::{FONT_BYTES, Font};
use crate::emulator::device::display::{CharDisplay, DEFAULT_COLUMNS, DEFAULT_FRAME_RATE_HZ, DEFAULT_ROWS};
use crate::emulator::{AddressRange, BusConfig, IoDevice};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Type name used in registering the character display as a device.
const DEVICE_TYPE: &str = "display/char";

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
                "display/char: columns and rows must both be positive".to_string()));
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
                "display/char requires a pipe transport; \
                 tcp/unix/pty transports don't support the atomic bulk-send this protocol needs"
                    .to_string()));
        }

        let bus_size = 2 * (columns * rows) + 2;
        let address_range = AddressRange::new(address, address + (bus_size - 1) as u16);

        // Not IRQ-capable (design doc §1): plain allocation, no interrupt line reserved.
        let device_id = id_allocator.lock().unwrap().next_available();

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

        if let Some(transport_spec) = transport_spec {
            // Size the pipe's ring to comfortably fit the larger of the two messages this
            // protocol ever sends -- the one-time header (dominated by the font) and each
            // per-vsync frame -- so `send_bytes`'s atomic push never has to drop either one.
            let header_len = 4 + 1 + 4 + 4 + 4 + 2 + FONT_BYTES;
            let frame_len = 2 * (columns * rows) as usize + device.palette().len() * 3;
            let capacity = header_len.max(frame_len);

            let (transport, _relay) = transport_spec
                .to_transport_with_reporter_and_capacity(
                    context.transport_reporter(device.identity()),
                    context.pipe_exit_reporter(device.identity()),
                    Some(capacity))
                .await
                .map_err(DeviceModuleError::Transport)?;
            device.attach_external_transport(transport);
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

        bus_config.device(address_range, device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
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
}
