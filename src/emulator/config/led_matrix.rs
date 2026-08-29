use super::{DeviceModule, DeviceModuleError, InstantiationContext, LedMatrixGeometry};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::display::DEFAULT_FRAME_RATE_HZ;
use crate::emulator::device::led_matrix::compositing::default_palette;
use crate::emulator::device::led_matrix::{LedMatrix, PIXELS_PER_MATRIX};
use crate::emulator::{AddressRange, BusConfig};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Matrix counts this device accepts (spec §3, design doc §2).
const VALID_MATRIX_COUNTS: [u32; 4] = [1, 2, 4, 8];

/// RGB LED matrix display adapter module (`display/matrix`).
///
/// Not IRQ-capable (design doc §1: "No control/status registers and no IRQ" -- swaps are always
/// synchronous), so device IDs come from [`DeviceIdAllocator::next_available`] rather than
/// [`DeviceIdAllocator::for_irq`]. There is no `transport=` attribute yet -- the external wire
/// protocol and companion process are a follow-on plan (design doc, "Explicitly out of scope").
///
/// Pixel memory and the command/data register pair are two disjoint bus ranges rather than one
/// contiguous region: pixel memory is sized from `matrix-count` and based at the device's
/// `address`, while the register pair is based at the separately configured, required
/// `register-address` (data register immediately follows at `register-address + 1`). This keeps
/// pixel memory -- likely to be placed at a 1-KiB/N-KiB-aligned boundary -- free of the
/// fragmentation two extra register bytes tacked onto its end would otherwise cause. The two
/// ranges are mapped onto the same `DeviceId` via `BusConfig::extend_device`, mirroring how
/// `CharDisplay`'s optional keyboard sub-range is wired (`config::display`).
#[derive(Clone)]
pub struct LedMatrixModule;

#[derive(Deserialize)]
struct LedMatrixAttributes {
    #[serde(rename = "matrix-count")]
    matrix_count: u32,
    #[serde(rename = "register-address")]
    register_address: u16,
    frame_rate_hz: Option<u32>,
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

        if !VALID_MATRIX_COUNTS.contains(&config.matrix_count) {
            return Err(DeviceModuleError::Config(format!(
                "display/matrix: matrix-count must be one of {VALID_MATRIX_COUNTS:?}, got {}",
                config.matrix_count)));
        }

        let frame_rate_hz = config.frame_rate_hz.unwrap_or(DEFAULT_FRAME_RATE_HZ);
        let device_id = id_allocator.lock().unwrap().next_available();

        let pixel_bytes = config.matrix_count * PIXELS_PER_MATRIX as u32;
        let pixel_range = AddressRange::new(address, address + (pixel_bytes as u16 - 1));
        let register_range = AddressRange::new(config.register_address, config.register_address + 1);

        let mut device = LedMatrix::new(
            self.name(),
            pixel_range,
            register_range,
            config.matrix_count,
            context.clock_hz,
            frame_rate_hz,
            default_palette(),
        );

        if let Some(sender) = &context.log_sender {
            device.set_log_sender(sender.clone());
        }

        // Both slots (design doc §10) are consumed the same way `display_frame_sink`/
        // `display_geometry_sink` are: present only when a host (the debugger) wants to receive
        // this device's output, absent (a no-op here) for the plain `emma65` CLI.
        if let Some(slot) = &context.led_matrix_geometry_sink {
            *slot.lock().unwrap() = Some(LedMatrixGeometry { matrices: config.matrix_count });
        }
        if let Some(slot) = &context.led_matrix_frame_sink
            && let Some(sender) = slot.lock().unwrap().take()
        {
            device.attach_frame_sink(sender);
        }

        let bus_config = bus_config.device(pixel_range, device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)?;

        bus_config.extend_device(register_range, device_id)
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
            led_matrix_frame_sink: None,
            led_matrix_geometry_sink: None,
        }
    }

    // Comfortably clear of every pixel range this file's tests build (up to 8 * 1024 = 0x2000
    // bytes starting at 0x8000), so the default doesn't accidentally overlap and mask a real
    // validation failure with an unrelated overlap error.
    const REGISTER_ADDRESS: u16 = 0xA000;

    fn attributes(matrix_count: u32) -> HashMap<String, Value> {
        let mut attributes = HashMap::new();
        attributes.insert("matrix-count".to_string(), Value::from(matrix_count));
        attributes.insert("register-address".to_string(), Value::from(REGISTER_ADDRESS));
        attributes
    }

    #[tokio::test]
    async fn instantiate_with_valid_matrix_count_succeeds() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(4), &context(), id_allocator).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn instantiate_without_matrix_count_fails() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &HashMap::new(), &context(), id_allocator).await;

        assert!(matches!(result, Err(DeviceModuleError::Config(_))));
    }

    #[tokio::test]
    async fn instantiate_with_invalid_matrix_count_fails() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(3), &context(), id_allocator).await;

        match result {
            Err(DeviceModuleError::Config(message)) => assert!(message.contains("matrix-count")),
            Err(other) => panic!("expected DeviceModuleError::Config, got a different error variant: {other}"),
            Ok(_) => panic!("expected DeviceModuleError::Config, got Ok"),
        }
    }

    #[tokio::test]
    async fn instantiate_without_register_address_fails() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let mut attributes = HashMap::new();
        attributes.insert("matrix-count".to_string(), Value::from(4u32));
        let result = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes, &context(), id_allocator).await;

        assert!(matches!(result, Err(DeviceModuleError::Config(_))));
    }

    #[tokio::test]
    async fn pixel_range_sized_from_matrix_count_and_registers_placed_separately() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(2), &context(), id_allocator).await.unwrap();
        let mut bus = bus_config.build();

        let pixel_bytes = 2 * PIXELS_PER_MATRIX as u16;
        // The last pixel byte of the second matrix must round-trip, proving the device's claimed
        // pixel range covers all `matrix_count * PIXELS_PER_MATRIX` bytes, not just the first
        // matrix's -- and that the gap up to `register-address` is not part of the device.
        bus.write(0x8000 + pixel_bytes - 1, 0x42).unwrap();
        assert_eq!(bus.read(0x8000 + pixel_bytes - 1).unwrap(), 0x42);
        // Command and data registers live at the separately configured `register-address`, not
        // immediately after pixel memory.
        assert!(bus.write(REGISTER_ADDRESS, 0).is_ok());
        assert!(bus.write(REGISTER_ADDRESS + 1, 0).is_ok());
    }

    #[tokio::test]
    async fn device_id_is_not_irq_capable() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _bus_config = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(1), &context(), id_allocator.clone()).await.unwrap();

        // A plain next_available() id falls outside the IRQ bitmask range, so every IRQ line
        // (including 0) must remain unclaimed.
        assert!(id_allocator.lock().unwrap().for_irq(0).is_ok());
    }
}
