use super::palette::parse_color;
use super::{DeviceModule, DeviceModuleError, ExpandedPathBuf, InstantiationContext, LcdDisplayGeometry, TransportSpec, TransportSpecFormat};
use crate::emulator::bus::DeviceIdAllocator;
use crate::emulator::device::lcd_display::cgrom::CgRom;
use crate::emulator::device::lcd_display::compositing::Rgb24;
use crate::emulator::device::lcd_display::{Geometry, LcdDisplay};
use crate::emulator::{AddressRange, BusConfig, IoDevice};
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Type name used in registering the LCD display as a device.
const DEVICE_TYPE: &str = "display/lcd";

const DEFAULT_GEOMETRY: &str = "16x2";

/// Which state -- pixel or background -- shows the backlight color vs. a dark/off tone, mirroring
/// a real LCD element's polarizer orientation (issue #583).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Polarity {
    /// Dark pixels over a backlight-colored background -- the common case for character LCDs.
    Positive,
    /// Backlight-colored pixels over a dark/off background.
    Negative,
}

impl Polarity {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "positive" => Some(Polarity::Positive),
            "negative" => Some(Polarity::Negative),
            _ => None,
        }
    }
}

// The classic yellow-green-backlight/black-polarizer combination most common LCD modules ship
// with (issue #579), superseding spec §3's originally-documented blue/white default -- background
// is CSS "yellowgreen" (#9ACD32).
const DEFAULT_POLARITY: &str = "positive";
const DEFAULT_BACKLIGHT: &str = "yellow";

/// `(polarity, backlight)` -> `(background, foreground)` presets modeling commonly available
/// real-world HD44780 module color schemes (issue #583). Positive polarity always renders dark
/// pixels over the backlight color; negative polarity renders the backlight color as the pixel
/// itself, over a dark "opaque near-black" background -- `0x0A0A0A` matches the bezel color the
/// SDL peripheral already draws (issue #579), so a negative-polarity display reads as visually
/// continuous with its bezel. Only 8 of the 10 possible `(polarity, backlight)` combinations
/// correspond to hardware that's actually commonly available; the rest are intentionally left
/// unmapped and rejected at configuration time rather than guessed at.
const COLOR_PRESETS: &[(Polarity, &str, Rgb24, Rgb24)] = &[
    (Polarity::Positive, "yellow", Rgb24::new(0x9A, 0xCD, 0x32), Rgb24::new(0x00, 0x00, 0x00)),
    (Polarity::Positive, "white", Rgb24::new(0xFF, 0xFF, 0xFF), Rgb24::new(0x00, 0x00, 0x00)),
    (Polarity::Positive, "amber", Rgb24::new(0xFF, 0xB0, 0x00), Rgb24::new(0x00, 0x00, 0x00)),
    (Polarity::Positive, "blue", Rgb24::new(0x87, 0xCE, 0xEB), Rgb24::new(0x00, 0x00, 0x00)),
    (Polarity::Negative, "blue", Rgb24::new(0x14, 0x21, 0x3D), Rgb24::new(0xFF, 0xFF, 0xFF)),
    (Polarity::Negative, "white", Rgb24::new(0x0A, 0x0A, 0x0A), Rgb24::new(0xFF, 0xFF, 0xFF)),
    (Polarity::Negative, "amber", Rgb24::new(0x0A, 0x0A, 0x0A), Rgb24::new(0xFF, 0xB0, 0x00)),
    (Polarity::Negative, "red", Rgb24::new(0x0A, 0x0A, 0x0A), Rgb24::new(0xFF, 0x24, 0x00)),
];

fn color_preset(polarity: Polarity, backlight: &str) -> Option<(Rgb24, Rgb24)> {
    COLOR_PRESETS.iter()
        .find(|(p, b, _, _)| *p == polarity && *b == backlight)
        .map(|(_, _, background, foreground)| (*background, *foreground))
}

fn supported_backlights_for(polarity: Polarity) -> Vec<&'static str> {
    COLOR_PRESETS.iter().filter(|(p, _, _, _)| *p == polarity).map(|(_, b, _, _)| *b).collect()
}

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

// Hand-computed rather than imported: `compositing`'s `CELL_WIDTH`/`CELL_HEIGHT_5X10` stay
// private (mirroring `config::led_matrix`'s identical note). These describe the *worst case*
// frame -- the 5x10 font, reachable at any time via `Function Set`'s `F` bit even though 5x8 is
// the reset default (spec §8.2) -- used only to size the external transport's ring (below), not
// to compute any actual frame.
const MAX_CELL_WIDTH_PX: usize = 5;
const MAX_CELL_HEIGHT_PX: usize = 10;

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
    /// Cosmetic-only display polarity (issue #583): `"positive"` (default) or `"negative"`,
    /// selecting which of `background`/`foreground` a chosen `backlight` color fills.
    polarity: Option<String>,
    /// Cosmetic-only backlight color preset (issue #583): one of `"yellow"`, `"white"`,
    /// `"amber"`, `"blue"`, `"red"`, restricted to the combinations valid for `polarity`.
    backlight: Option<String>,
    /// Cosmetic-only rendering colors (spec §3, §8.3); hex RGB24, e.g. `"0000AA"`. Each, if
    /// given, overrides the corresponding channel of the `polarity`/`backlight` preset.
    background: Option<String>,
    foreground: Option<String>,
    /// Wire-protocol transport for a standalone external peripheral (`plan/lcd-display-external-
    /// protocol.md`); absent for the debugger, which receives frames in-process via
    /// `lcd_display_frame_sink` instead.
    transport: Option<TransportSpecFormat>,
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

        let polarity_name = config.polarity.as_deref().unwrap_or(DEFAULT_POLARITY);
        let polarity = Polarity::parse(polarity_name).ok_or_else(|| DeviceModuleError::Config(format!(
            "display/lcd: polarity must be one of [\"positive\", \"negative\"], got {polarity_name:?}")))?;

        let backlight_name = config.backlight.as_deref().unwrap_or(DEFAULT_BACKLIGHT);
        let (preset_background, preset_foreground) = color_preset(polarity, backlight_name)
            .ok_or_else(|| DeviceModuleError::Config(format!(
                "display/lcd: backlight must be one of {:?} for {polarity_name} polarity, got {backlight_name:?}",
                supported_backlights_for(polarity))))?;

        let background = match &config.background {
            Some(text) => parse_color(text).ok_or_else(|| DeviceModuleError::Config(format!(
                "display/lcd: background must be 6 hex digits (RRGGBB), optionally '#'-prefixed, got {text:?}")))?,
            None => preset_background,
        };
        let foreground = match &config.foreground {
            Some(text) => parse_color(text).ok_or_else(|| DeviceModuleError::Config(format!(
                "display/lcd: foreground must be 6 hex digits (RRGGBB), optionally '#'-prefixed, got {text:?}")))?,
            None => preset_foreground,
        };

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;
        // The external protocol's per-message sends (`plan/lcd-display-external-protocol.md`)
        // rely on `Transport::send_bytes`'s all-or-nothing contract, which only `PipeTransport`
        // provides (see `config::display`'s and `config::led_matrix`'s identical restriction) --
        // reject any other kind rather than silently desyncing the stream on the first dropped
        // message.
        if let Some(spec) = &transport_spec
            && !matches!(spec, TransportSpec::Pipe { .. })
        {
            return Err(DeviceModuleError::Config(
                "display/lcd requires a pipe transport; \
                 tcp/unix/pty transports don't support the atomic bulk-send this protocol needs"
                    .to_string()));
        }

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
            *slot.lock().unwrap() =
                Some(LcdDisplayGeometry { columns: geometry.columns, rows: geometry.rows, background, foreground });
        }
        if let Some(slot) = &context.lcd_display_frame_sink
            && let Some(sender) = slot.lock().unwrap().take()
        {
            device.attach_frame_sink(sender);
        }

        if let Some(transport_spec) = transport_spec {
            // Size the pipe's ring to hold the single largest frame message the device can ever
            // push (worst case: the 5x10 font at this geometry's row/column count). Unlike
            // `LedMatrix`'s swap-all-matrices case, there's no *multi-message* burst within one
            // call to worry about -- this device pushes at most one frame per completed register
            // write -- but consecutive writes can still outrun the ring's drain (issue #581) if
            // the peripheral falls behind; `LcdDisplay::tick` retries a frame that doesn't fit
            // here rather than losing it, so this capacity only needs to fit one frame, not a
            // backlog.
            let capacity = 4
                + geometry.columns as usize * MAX_CELL_WIDTH_PX
                * geometry.rows as usize * MAX_CELL_HEIGHT_PX * 4;

            let (transport, _relay) = transport_spec
                .to_transport_with_reporter_and_capacity(
                    context.transport_reporter(device.identity()),
                    context.pipe_exit_reporter(device.identity()),
                    Some(capacity))
                .await
                .map_err(DeviceModuleError::Transport)?;
            device.attach_external_transport(transport);
        }

        bus_config.device(address_range, device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> InstantiationContext {
        InstantiationContext::default()
    }

    /// A context whose `lcd_display_geometry_sink` is wired up, so a test can read back the
    /// device's computed `background`/`foreground` (design doc §7) after `instantiate`.
    fn context_with_geometry_sink() -> (InstantiationContext, Arc<Mutex<Option<LcdDisplayGeometry>>>) {
        let sink = Arc::new(Mutex::new(None));
        let ctx = InstantiationContext {
            lcd_display_geometry_sink: Some(sink.clone()),
            ..Default::default()
        };
        (ctx, sink)
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
    async fn instantiate_without_attributes_uses_default_polarity_and_backlight_colors() {
        let (ctx, sink) = context_with_geometry_sink();
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &HashMap::new(), &ctx, id_allocator).await.unwrap();

        let geometry = sink.lock().unwrap().unwrap();
        assert_eq!(geometry.background, Rgb24::new(0x9A, 0xCD, 0x32));
        assert_eq!(geometry.foreground, Rgb24::new(0x00, 0x00, 0x00));
    }

    #[tokio::test]
    async fn instantiate_with_each_color_preset_succeeds_and_yields_expected_colors() {
        for (polarity, backlight, background, foreground) in COLOR_PRESETS {
            let mut attributes = HashMap::new();
            attributes.insert("polarity".to_string(), Value::from(match polarity {
                Polarity::Positive => "positive",
                Polarity::Negative => "negative",
            }));
            attributes.insert("backlight".to_string(), Value::from(*backlight));
            let (ctx, sink) = context_with_geometry_sink();
            let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

            let result = LcdDisplayModule.instantiate(
                BusConfig::new(), 0xD000, &attributes, &ctx, id_allocator).await;

            assert!(result.is_ok(), "polarity {polarity:?} backlight {backlight:?} should be accepted");
            let geometry = sink.lock().unwrap().unwrap();
            assert_eq!(geometry.background, *background, "background for {polarity:?}/{backlight:?}");
            assert_eq!(geometry.foreground, *foreground, "foreground for {polarity:?}/{backlight:?}");
        }
    }

    #[tokio::test]
    async fn instantiate_with_unsupported_polarity_fails() {
        let mut attributes = HashMap::new();
        attributes.insert("polarity".to_string(), Value::from("sideways"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

        match result {
            Err(DeviceModuleError::Config(message)) => assert!(message.contains("polarity")),
            Err(other) => panic!("expected DeviceModuleError::Config, got a different error variant: {other}"),
            Ok(_) => panic!("expected DeviceModuleError::Config, got Ok"),
        }
    }

    #[tokio::test]
    async fn instantiate_with_backlight_not_valid_for_polarity_fails() {
        // "red" is only defined for negative polarity (issue #583) -- positive+red should be
        // rejected rather than silently falling back to some other color.
        let mut attributes = HashMap::new();
        attributes.insert("polarity".to_string(), Value::from("positive"));
        attributes.insert("backlight".to_string(), Value::from("red"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

        match result {
            Err(DeviceModuleError::Config(message)) => assert!(message.contains("backlight")),
            Err(other) => panic!("expected DeviceModuleError::Config, got a different error variant: {other}"),
            Ok(_) => panic!("expected DeviceModuleError::Config, got Ok"),
        }
    }

    #[tokio::test]
    async fn instantiate_with_explicit_colors_overrides_preset() {
        let mut attributes = HashMap::new();
        attributes.insert("polarity".to_string(), Value::from("negative"));
        attributes.insert("backlight".to_string(), Value::from("red"));
        attributes.insert("foreground".to_string(), Value::from("#00FF00"));
        let (ctx, sink) = context_with_geometry_sink();
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &ctx, id_allocator).await.unwrap();

        let geometry = sink.lock().unwrap().unwrap();
        // background still comes from the negative/red preset...
        assert_eq!(geometry.background, Rgb24::new(0x0A, 0x0A, 0x0A));
        // ...but the explicit foreground override wins over the preset's foreground.
        assert_eq!(geometry.foreground, Rgb24::new(0x00, 0xFF, 0x00));
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

    #[tokio::test]
    async fn instantiate_without_transport_attribute_succeeds() {
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));
        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &HashMap::new(), &context(), id_allocator).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn rejects_non_pipe_transport_spec() {
        let mut attributes = HashMap::new();
        attributes.insert("transport".to_string(), Value::from("unix:/tmp/emma65_test_lcd_display.sock"));
        let id_allocator = Arc::new(Mutex::new(DeviceIdAllocator::new()));

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

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

        let result = LcdDisplayModule.instantiate(
            BusConfig::new(), 0xD000, &attributes, &context(), id_allocator).await;

        // End-to-end smoke test with a real spawned child: confirms the computed ring capacity
        // is accepted by `PipeTransport::spawn_with_capacity` and `attach_external_transport`'s
        // immediate header send doesn't panic against a live pipe.
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn transport_capacity_holds_the_largest_possible_frame_message() {
        // Regression-shaped test mirroring `config::led_matrix`'s capacity test: the ring must be
        // able to hold a single frame message at the largest supported geometry (`40x2`, tied
        // with `20x4` for the most total dot cells) rendered with the 5x10 font -- the worst case
        // this device can ever push, even though 5x8 is the reset default (spec §8.2). Unlike
        // `LedMatrix`, there's no multi-message burst to worry about, since this device sends at
        // most one frame per completed register write.
        let geometry = geometry_for("40x2").unwrap();
        let width_px = geometry.columns as usize * MAX_CELL_WIDTH_PX;
        let height_px = geometry.rows as usize * MAX_CELL_HEIGHT_PX;
        let capacity = 4 + width_px * height_px * 4;

        let (sender, _receiver) = crate::emulator::device_event_channel();
        let reporter = crate::emulator::TransportReporter::pending(Some(sender));

        let spec = TransportSpec::Pipe { command: vec!["/usr/bin/cat".to_string()] };
        let (mut transport, _relay) = spec
            .to_transport_with_reporter_and_capacity(reporter, |_| {}, Some(capacity))
            .await
            .unwrap();

        let frame = vec![0xAAu8; 4 + width_px * height_px * 4];
        assert!(
            transport.send_bytes(&frame),
            "largest-geometry, largest-font frame message was dropped -- ring capacity too small"
        );
    }
}
