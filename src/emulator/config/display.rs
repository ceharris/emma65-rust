use super::{DeviceModule, DeviceModuleError, ExpandedPathBuf, InstantiationContext};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::display::font::Font;
use crate::emulator::device::display::{palette, CharDisplay, DEFAULT_COLUMNS, DEFAULT_FRAME_RATE_HZ, DEFAULT_ROWS};
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
            None => palette::default_palette(),
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

        let device = CharDisplay::new(
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

        bus_config.device(address_range, device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }

}
