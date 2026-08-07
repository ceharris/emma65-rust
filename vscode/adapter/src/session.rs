//! Builds an [`Cpu`] for a DAP `launch` request, mirroring the Tauri debugger's session
//! construction and halted-at-reset-vector startup.
use std::path::Path;

use emma65::emulator::{Config, Cpu, DeviceRegistry, default_config};
use figment::providers::{Env, Format, Toml};
use figment::Figment;

const DEFAULT_ROM: &[u8] = include_bytes!("../../../src/bin/emulator/default.bin");

/// Loads emulator configuration from `config_path` (TOML) merged with `EMMA65_*` environment
/// variables, falling back to the built-in default device layout (the same fallback the
/// `emma65` binary uses) when no devices are configured. Builds the session and resets the
/// CPU so it halts at the reset vector.
pub async fn build_session(config_path: Option<&Path>) -> Result<Cpu, String> {
    let mut figment = Figment::new();
    if let Some(path) = config_path {
        figment = figment.merge(Toml::file(path));
    }
    figment = figment.merge(Env::prefixed("EMMA65_").map(|k| k.as_str().replace('_', "-").into()));

    let mut config: Config = figment.extract().map_err(|e| format!("configuration error: {e}"))?;
    let _default_rom_file = config.devices.as_ref().is_none_or(|d| d.is_empty()).then(|| {
        let (default, rom_file) = default_config(DEFAULT_ROM);
        config = default;
        rom_file
    });

    let registry = DeviceRegistry::with_builtins();
    let session = config
        .build(&registry)
        .await
        .map_err(|e| format!("failed to build emulator session: {e}"))?;

    let mut cpu = session.cpu;
    cpu.reset().map_err(|e| format!("CPU reset failed: {e}"))?;
    Ok(cpu)
}
