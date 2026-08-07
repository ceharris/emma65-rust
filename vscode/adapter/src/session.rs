//! Builds a [`Cpu`] for a DAP `launch` request, mirroring the Tauri debugger's session
//! construction and halted-at-reset-vector startup.
use std::path::Path;

use emma65::emulator::{Config, Cpu, DeviceRegistry, DeviceSpec, IrqSource, default_config};
use figment::Figment;
use figment::providers::Serialized;
use figment::value::{Dict, Value};

const DEFAULT_ROM: &[u8] = include_bytes!("../../../src/bin/emulator/default.bin");

/// Unix-domain socket the console device listens on so the VS Code extension's
/// integrated terminal (a `vscode.Pseudoterminal`) can connect to it directly as a
/// raw byte-stream peer, independent of the DAP connection — reusing the same
/// `~/.emma/sock/` convention the default VIA/PTM device layout already uses.
///
/// Bypassing DAP for this byte stream is deliberate, not a simplification of the
/// plan: the `dap` crate (`dap = "0.4.1-alpha1"`) has no support for custom
/// request/event names at all — its `Command`/`Event`/`ResponseBody` types are
/// closed enums, and an unrecognized incoming command aborts the whole session.
/// The one extensible field it does offer, the `output` event's `category`, isn't
/// a usable substitute either: VS Code's `debugSession.ts` appends every `output`
/// event to the Debug Console regardless of category, so reusing it would leak
/// every console byte into the Debug Console panel too. Given the crate's low
/// maintenance activity (see the spike's dap-crate-bugs memory), a direct
/// transport-level connection is the more robust choice for a spike whose
/// terminal support only needs to be good enough to evaluate the overall
/// direction, not survive further dap-rs quirks.
const TERMINAL_SOCKET_SPEC: &str = "unix:~/.emma/sock/vscode-terminal";

/// Loads emulator configuration from `config_path` (TOML) merged with `EMMA65_*` environment
/// variables, falling back to the built-in default device layout (the same fallback the
/// `emma65` binary uses) when no devices are configured. Builds the session and resets the
/// CPU so it halts at the reset vector.
///
/// Also allocates an [`IrqSource`] for the extension's IRQ-assert/release commands (story
/// 9), from the session's post-configuration `DeviceIdAllocator` — guaranteed not to
/// collide with any configured device's own IRQ source, mirroring the Tauri debugger's
/// `ui_irq_source` allocation in `debugger/src-tauri/src/lib.rs`.
pub async fn build_session(config_path: Option<&Path>) -> Result<(Cpu, IrqSource), String> {
    let mut config: Config = Config::figment(config_path).extract().map_err(|e| format!("configuration error: {e}"))?;
    let _default_rom_file = config.devices.as_ref().is_none_or(|d| d.is_empty()).then(|| {
        let (default, rom_file) = default_config(DEFAULT_ROM);
        config = default;
        rom_file
    });

    wire_terminal_socket(&mut config);

    let registry = DeviceRegistry::with_builtins();
    let session = config
        .build(&registry)
        .await
        .map_err(|e| format!("failed to build emulator session: {e}"))?;

    let mut cpu = session.cpu;
    cpu.reset().map_err(|e| format!("CPU reset failed: {e}"))?;
    let mut id_allocator = session.id_allocator;
    let ui_irq_source = IrqSource::from(id_allocator.next(true));
    Ok((cpu, ui_irq_source))
}

/// Points the configured console device (if any) at [`TERMINAL_SOCKET_SPEC`], unless it
/// already specifies its own `transport` attribute — an explicit configuration choice
/// always wins over this default wiring, same precedence `ConsoleModule::instantiate`
/// already gives an injected transport versus a configured one.
fn wire_terminal_socket(config: &mut Config) {
    let Some(devices) = config.devices.as_mut() else { return };
    let Some(index) = devices.iter().position(|d| d.module_name() == "console") else { return };
    if devices[index].attributes().contains_key("transport") {
        return;
    }
    devices[index] = with_terminal_transport(&devices[index]);
}

/// Rebuilds `console`'s [`DeviceSpec`] with [`TERMINAL_SOCKET_SPEC`] added as its
/// `transport` attribute, preserving every other attribute it already had. Goes through
/// the same `Dict` + `Serialized::defaults` + `.extract()` round trip
/// `ConsoleModule::instantiate` itself uses to turn attributes into a typed config, so
/// there's no need to hand-roll attribute-value formatting here.
fn with_terminal_transport(console: &DeviceSpec) -> DeviceSpec {
    let mut attrs: Dict = Dict::from_iter(console.attributes().clone());
    attrs.insert("type".to_string(), Value::from(console.module_name()));
    attrs.insert("address".to_string(), Value::from(console.address() as u32));
    attrs.insert("transport".to_string(), Value::from(TERMINAL_SOCKET_SPEC));
    Figment::new()
        .merge(Serialized::defaults(attrs))
        .extract()
        .expect("reconstructed console DeviceSpec is well-formed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_devices(devices: Vec<&str>) -> Config {
        Config {
            cpu_variant_spec: None,
            clock_speed_hz: None,
            devices: Some(devices.into_iter().map(|s| s.parse().unwrap()).collect()),
        }
    }

    #[test]
    fn wire_terminal_socket_adds_transport_and_keeps_other_attributes() {
        let mut config = config_with_devices(vec!["console@0xfff8,break=0x3"]);
        wire_terminal_socket(&mut config);

        let console = &config.devices.unwrap()[0];
        assert_eq!(console.address(), 0xfff8);
        assert_eq!(console.attributes().get("break").unwrap().to_num().unwrap().to_u32().unwrap(), 3);
        assert_eq!(console.attributes().get("transport").unwrap().as_str().unwrap(), TERMINAL_SOCKET_SPEC);
    }

    #[test]
    fn wire_terminal_socket_leaves_explicit_transport_alone() {
        let mut config = config_with_devices(vec!["console@0xfff8,transport=pty:/tmp/console"]);
        wire_terminal_socket(&mut config);

        let console = &config.devices.unwrap()[0];
        assert_eq!(console.attributes().get("transport").unwrap().as_str().unwrap(), "pty:/tmp/console");
    }

    #[test]
    fn wire_terminal_socket_ignores_config_without_console() {
        let mut config = config_with_devices(vec!["ram@0x0000,size=1K"]);
        wire_terminal_socket(&mut config);

        let devices = config.devices.unwrap();
        assert_eq!(devices.len(), 1);
        assert!(devices[0].attributes().get("transport").is_none());
    }

    #[test]
    fn wire_terminal_socket_ignores_config_without_devices() {
        let mut config = Config { cpu_variant_spec: None, clock_speed_hz: None, devices: None };
        wire_terminal_socket(&mut config);
        assert!(config.devices.is_none());
    }
}
