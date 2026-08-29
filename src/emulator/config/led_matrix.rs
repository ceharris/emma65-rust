use super::{DeviceModule, DeviceModuleError, InstantiationContext};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::led_matrix::LedMatrix;
use crate::emulator::device::led_matrix::compositing::Rgb565;
use crate::emulator::{AddressRange, BusConfig};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Pixel-index bytes per attached matrix (32x32), mirroring led_matrix::PIXELS_PER_MATRIX.
const PIXELS_PER_MATRIX: u32 = 1024;

// Default IRQ to assign to this device
const DEFAULT_IRQ: u32 = 6;

/// RGB LED matrix display adapter module.
///
/// **Stopgap** (`doc/memory-mapped-led-matrix-device-plan.md`, Work Unit 1): the device core this
/// module instantiates was rewritten in Work Unit 1 to a new memory-mapped register model, but
/// this module's own rewrite (`LedMatrixAttributes` with `matrices`/`frame_rate_hz`, `matrices`
/// validation, non-IRQ device ID allocation, `compositing::default_palette()`) is Work Unit 3.
/// Until then this hardcodes a single matrix and an all-black palette purely to keep the crate
/// compiling -- `display/matrix` is not meaningfully usable through configuration yet.
#[derive(Clone)]
pub struct LedMatrixModule;

#[derive(Deserialize)]
pub struct LedMatrixAttributes {
    irq: Option<u32>,
}

impl DeviceModule for LedMatrixModule {

    fn name(&self) -> &'static str {
        "display/matrix"
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: LedMatrixAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let irq = config.irq.unwrap_or(DEFAULT_IRQ);
        let device_id = id_allocator.lock().unwrap()
            .for_irq(irq)
            .map_err(DeviceModuleError::BusConfig)?;

        // Stopgap (see module doc comment): Work Unit 3 replaces this with the real `matrices=`
        // attribute and `compositing::default_palette()` (added in Work Unit 2).
        let matrices = 1u32;
        let bus_size = matrices * PIXELS_PER_MATRIX + 2;
        let palette = vec![Rgb565::new(0, 0, 0); 256];
        let address_range = AddressRange::new(address, address + (bus_size as u16 - 1));

        let device = {
            let mut dev = LedMatrix::new(
                self.name(),
                address_range,
                matrices,
                context.clock_hz,
                crate::emulator::device::display::DEFAULT_FRAME_RATE_HZ,
                palette,
            );
            if let Some(sender) = &context.log_sender {
                dev.set_log_sender(sender.clone());
            }
            dev
        };

        bus_config.device(address_range, device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }

}