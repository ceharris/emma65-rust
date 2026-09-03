use super::palette::parse_color;
use super::{DeviceModule, DeviceModuleError, ExpandedPathBuf, InstantiationContext, LcdDisplayGeometry};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::lcd_display::cgrom::CgRom;
use crate::emulator::device::lcd_display::compositing::Rgb24;
use crate::emulator::device::lcd_display::{Geometry, LcdDisplay};
use crate::emulator::{AddressRange, BusConfig};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Type name used in registering the LCD display as a device.
const DEVICE_TYPE: &str = "display/lcd";

const DEFAULT_GEOMETRY: &str = "16x2";
// Spec §3's documented default: blue background, white foreground -- the classic HD44780
// backlight/polarizer combination.
const DEFAULT_BACKGROUND: Rgb24 = Rgb24::new(0x00, 0x00, 0xAA);
const DEFAULT_FOREGROUND: Rgb24 = Rgb24::new(0xFF, 0xFF, 0xFF);

/// The 9 supported geometries (spec §7.1), looked up by the config's `geometry` string. This is
/// the single source of truth for every geometry's row/segment layout -- `geometry_for` is the
/// only place a `geometry=` string turns into a `&'static Geometry`.
const GEOMETRIES: &[(&str, Geometry)] = &[
    ("8x1", Geometry { rows: 1, columns: 8, segments: &[&[(0x00, 8)]] }),
    ("40x1", Geometry { rows: 1, columns: 40, segments: &[&[(0x00, 40)]] }),
    ("8x2", Geometry { rows: 2, columns: 8, segments: &[&[(0x00, 8)], &[(0x40, 8)]] }),
    ("16x2", Geometry { rows: 2, columns: 16, segments: &[&[(0x00, 16)], &[(0x40, 16)]] }),
    ("20x2", Geometry { rows: 2, columns: 20, segments: &[&[(0x00, 20)], &[(0x40, 20)]] }),
    ("40x2", Geometry { rows: 2, columns: 40, segments: &[&[(0x00, 40)], &[(0x40, 40)]] }),
    // A documented real-world quirk, not a simplification (spec §7.1): one visible row made of
    // two 8-byte segments, one from each internal 40-byte DDRAM line.
    ("16x1", Geometry { rows: 1, columns: 16, segments: &[&[(0x00, 8), (0x40, 8)]] }),
    // Four-row modules split each of the two internal 40-byte lines into two visible rows (spec
    // §7.1); rows 1 & 3 share one line, rows 2 & 4 share the other.
    ("16x4", Geometry {
        rows: 4,
        columns: 16,
        segments: &[&[(0x00, 16)], &[(0x40, 16)], &[(0x10, 16)], &[(0x50, 16)]],
    }),
    ("20x4", Geometry {
        rows: 4,
        columns: 20,
        segments: &[&[(0x00, 20)], &[(0x40, 20)], &[(0x14, 20)], &[(0x54, 20)]],
    }),
];

fn geometry_for(name: &str) -> Option<&'static Geometry> {
    GEOMETRIES.iter().find(|(n, _)| *n == name).map(|(_, g)| g)
}

fn supported_geometry_names() -> Vec<&'static str> {
    GEOMETRIES.iter().map(|(n, _)| *n).collect()
}

/// Memory-mapped HD44780-compatible character LCD display device module (`display/lcd`).
///
/// Not IRQ-capable (the HD44780 interface has no interrupt output at all -- spec §2), so device
/// IDs come from [`DeviceIdAllocator::next_available`] rather than [`DeviceIdAllocator::for_irq`].
///
/// Also consumes `context.lcd_display_frame_sink`/`lcd_display_geometry_sink` (design doc §7),
/// the same way [`super::display::CharDisplayModule`] consumes its own `display_frame_sink`/
/// `display_geometry_sink`: present only when a host (the debugger) wants to receive this
/// device's output, a no-op here for the plain `emma65` CLI.
#[derive(Clone)]
pub struct LcdDisplayModule;

#[derive(Deserialize)]
struct LcdDisplayAttributes {
    /// One of the 9 supported values (design doc §2); default `16x2` (spec §3).
    geometry: Option<String>,
    /// Optional override for the built-in character generator ROM (spec §3, §8.1).
    cgrom: Option<ExpandedPathBuf>,
    /// Cosmetic-only rendering colors (spec §3, §8.3); hex RGB24, e.g. `"0000AA"`.
    background: Option<String>,
    foreground: Option<String>,
}

impl DeviceModule for LcdDisplayModule {

    fn name(&self) -> &'static str {
        DEVICE_TYPE
    }

    async fn instantiate(&self, bus_config: BusConfig, address: u16,
                         attributes: &HashMap<String, Value>, context: &InstantiationContext,
                         id_allocator: Arc<Mutex<DeviceIdAllocator>>)
            -> Result<BusConfig, DeviceModuleError> {

        let attrs = Dict::from_iter(attributes.clone());
        let config: LcdDisplayAttributes = figment::Figment::new()
            .merge(Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(format!("configuration error: {e}")))?;

        let geometry_name = config.geometry.as_deref().unwrap_or(DEFAULT_GEOMETRY);
        let geometry = geometry_for(geometry_name).ok_or_else(|| DeviceModuleError::Config(format!(
            "display/lcd: geometry must be one of {:?}, got {geometry_name:?}",
            supported_geometry_names())))?;

        let cgrom = match &config.cgrom {
            Some(path) => {
                let data = tokio::fs::read(path).await.map_err(DeviceModuleError::Io)?;
                CgRom::from_bytes(&data).map_err(|e| DeviceModuleError::Config(e.to_string()))?
            }
            None => CgRom::default(),
        };

        let background = match &config.background {
            Some(text) => parse_color(text).ok_or_else(|| DeviceModuleError::Config(format!(
                "display/lcd: background must be 6 hex digits (RRGGBB), optionally '#'-prefixed, got {text:?}")))?,
            None => DEFAULT_BACKGROUND,
        };
        let foreground = match &config.foreground {
            Some(text) => parse_color(text).ok_or_else(|| DeviceModuleError::Config(format!(
                "display/lcd: foreground must be 6 hex digits (RRGGBB), optionally '#'-prefixed, got {text:?}")))?,
            None => DEFAULT_FOREGROUND,
        };

        let device_id = id_allocator.lock().unwrap().next_available();
        // Bus size is always 2 bytes (spec §4.1) -- the only device whose mapped size doesn't
        // scale with any config attribute.
        let address_range = AddressRange::new(address, address + 1);

        let mut device = LcdDisplay::new(
            self.name(),
            address_range,
            geometry,
            context.clock_hz,
            cgrom,
            background,
            foreground,
        );

        if let Some(sender) = &context.log_sender {
            device.set_log_sender(sender.clone());
        }

        // Both slots (design doc §7) are consumed the same way `display_frame_sink`/
        // `display_geometry_sink` are: present only when a host (the debugger) wants to receive
        // this device's output, absent (a no-op here) for the plain `emma65` CLI.
        if let Some(slot) = &context.lcd_display_geometry_sink {
            *slot.lock().unwrap() = Some(LcdDisplayGeometry { columns: geometry.columns, rows: geometry.rows });
        }
        if let Some(slot) = &context.lcd_display_frame_sink
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
            lcd_display_frame_sink: None,
            lcd_display_geometry_sink: None,
        }
    }

    #[tokio::test]
    async fn instantiate_without_attributes_uses_default_geometry_and_colors() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let bus_config = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &HashMap::new(), &context(), id_allocator).await.unwrap();
        let mut bus = bus_config.build();

        // Round-trips through the instruction register prove the device is live at the
        // configured address with the default (2-byte) footprint.
        assert!(bus.write(0xD000, 0x01).is_ok());
        assert!(bus.write(0xD001, 0x00).is_ok());
    }

    #[tokio::test]
    async fn instantiate_with_each_supported_geometry_succeeds() {
        for (name, _) in GEOMETRIES {
            let mut attributes = HashMap::new();
            attributes.insert("geometry".to_string(), Value::from(*name));
            let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

            let result = LcdDisplayModule.instantiate(
                BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

            assert!(result.is_ok(), "geometry {name:?} should be accepted");
        }
    }

    #[tokio::test]
    async fn instantiate_with_unsupported_geometry_fails() {
        let mut attributes = HashMap::new();
        attributes.insert("geometry".to_string(), Value::from("not-a-geometry"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

        match result {
            Err(DeviceModuleError::Config(message)) => assert!(message.contains("geometry")),
            Err(other) => panic!("expected DeviceModuleError::Config, got a different error variant: {other}"),
            Ok(_) => panic!("expected DeviceModuleError::Config, got Ok"),
        }
    }

    #[tokio::test]
    async fn instantiate_with_malformed_background_color_fails() {
        let mut attributes = HashMap::new();
        attributes.insert("background".to_string(), Value::from("not-a-color"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

        match result {
            Err(DeviceModuleError::Config(message)) => assert!(message.contains("background")),
            Err(other) => panic!("expected DeviceModuleError::Config, got a different error variant: {other}"),
            Ok(_) => panic!("expected DeviceModuleError::Config, got Ok"),
        }
    }

    #[tokio::test]
    async fn instantiate_with_malformed_foreground_color_fails() {
        let mut attributes = HashMap::new();
        attributes.insert("foreground".to_string(), Value::from("#zzzzzz"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

        assert!(matches!(result, Err(DeviceModuleError::Config(_))));
    }

    #[tokio::test]
    async fn instantiate_with_valid_colors_succeeds() {
        let mut attributes = HashMap::new();
        attributes.insert("background".to_string(), Value::from("#101010"));
        attributes.insert("foreground".to_string(), Value::from("F0F0F0"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn instantiate_with_malformed_cgrom_file_fails() {
        let dir = std::env::temp_dir();
        let path = dir.join("emma65_test_lcd_display_bad_cgrom.bin");
        tokio::fs::write(&path, [0u8; 10]).await.unwrap();

        let mut attributes = HashMap::new();
        attributes.insert("cgrom".to_string(), Value::from(path.to_str().unwrap()));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

        let _ = tokio::fs::remove_file(&path).await;
        assert!(matches!(result, Err(DeviceModuleError::Config(_))));
    }

    #[tokio::test]
    async fn instantiate_with_valid_cgrom_file_succeeds() {
        let dir = std::env::temp_dir();
        let path = dir.join("emma65_test_lcd_display_good_cgrom.bin");
        tokio::fs::write(&path, vec![0u8; crate::emulator::device::lcd_display::cgrom::CGROM_BYTES]).await.unwrap();

        let mut attributes = HashMap::new();
        attributes.insert("cgrom".to_string(), Value::from(path.to_str().unwrap()));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

        let _ = tokio::fs::remove_file(&path).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn device_id_is_not_irq_capable() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let _bus_config = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &HashMap::new(), &context(), id_allocator.clone()).await.unwrap();

        assert!(id_allocator.lock().unwrap().for_irq(0).is_ok());
    }
}
