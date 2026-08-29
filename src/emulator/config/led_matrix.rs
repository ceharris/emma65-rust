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
#[derive(Clone)]
pub struct LedMatrixModule;

#[derive(Deserialize)]
struct LedMatrixAttributes {
    matrices: u32,
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

        if !VALID_MATRIX_COUNTS.contains(&config.matrices) {
            return Err(DeviceModuleError::Config(format!(
                "display/matrix: matrices must be one of {VALID_MATRIX_COUNTS:?}, got {}",
                config.matrices)));
        }

        let frame_rate_hz = config.frame_rate_hz.unwrap_or(DEFAULT_FRAME_RATE_HZ);
        let device_id = id_allocator.lock().unwrap().next_available();

        let bus_size = config.matrices * PIXELS_PER_MATRIX as u32 + 2;
        let address_range = AddressRange::new(address, address + (bus_size as u16 - 1));

        let mut device = LedMatrix::new(
            self.name(),
            address_range,
            config.matrices,
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
            *slot.lock().unwrap() = Some(LedMatrixGeometry { matrices: config.matrices });
        }
        if let Some(slot) = &context.led_matrix_frame_sink
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
            led_matrix_frame_sink: None,
            led_matrix_geometry_sink: None,
        }
    }

    fn attributes(matrices: u32) -> HashMap<String, Value> {
        let mut attributes = HashMap::new();
        attributes.insert("matrices".to_string(), Value::from(matrices));
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
    async fn instantiate_without_matrices_fails() {
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
            Err(DeviceModuleError::Config(message)) => assert!(message.contains("matrices")),
            Err(other) => panic!("expected DeviceModuleError::Config, got a different error variant: {other}"),
            Ok(_) => panic!("expected DeviceModuleError::Config, got Ok"),
        }
    }

    #[tokio::test]
    async fn address_range_sized_from_matrix_count() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = LedMatrixModule.instantiate(
            BusConfig::new(), 0x8000, &attributes(2), &context(), id_allocator).await.unwrap();
        let mut bus = bus_config.build();

        let pixel_bytes = 2 * PIXELS_PER_MATRIX as u16;
        // The last pixel byte of the second matrix must round-trip, proving the device's claimed
        // range covers all `matrices * PIXELS_PER_MATRIX` bytes, not just the first matrix's.
        bus.write(0x8000 + pixel_bytes - 1, 0x42).unwrap();
        assert_eq!(bus.read(0x8000 + pixel_bytes - 1).unwrap(), 0x42);
        // The data register, the last byte of the device's range, must also be part of it.
        assert!(bus.write(0x8000 + pixel_bytes + 1, 0).is_ok());
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
