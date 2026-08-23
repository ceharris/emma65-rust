use super::palette;
use super::{DeviceModule, DeviceModuleError, DisplayGeometry, ExpandedPathBuf, InstantiationContext};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::display::compositing::default_palette;
use crate::emulator::device::display::font::Font;
use crate::emulator::device::display::{CharDisplay, DEFAULT_COLUMNS, DEFAULT_FRAME_RATE_HZ, DEFAULT_ROWS};
use crate::emulator::{AddressRange, BusConfig};
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
