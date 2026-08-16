use clap::Parser;
use figment::{Figment, providers::{Env, Format, Serialized, Toml}};
use serde::{Deserialize, Serialize};

// CLI args.
// This struct exists solely to capture the `--config` option before Figment runs. It must not
// derive Serde's Serialize or Deserialize.
#[derive(Parser)]
struct CliArgs {
    /// Path to a TOML configuration file
    #[clap(long = "config", conflicts_with = "profile")]
    config: Option<std::path::PathBuf>,

    /// Name of a bundled starter-profile template to run (see
    /// `emma65::emulator::config::templates::TEMPLATES`), materialized into
    /// a temporary directory for this run. Mutually exclusive with `--config`.
    #[clap(long = "profile", conflicts_with = "config")]
    profile: Option<String>,

    #[clap(flatten)]
    app: AppConfig,
}

#[derive(Debug, Clone, Parser, Serialize, Deserialize)]
#[clap(name = "emma65")]
#[serde(rename_all = "kebab-case")]
/// Configuration attributes for the standalone emulator utility
pub struct AppConfig {
    /// Embeds all emulator config files (cpu-variant, clock-speed-hz, device, etc).
    #[clap(flatten)]
    #[serde(flatten)]
    pub emulator: emma65::emulator::Config,

    /// Path to write a binary CPU execution trace to.
    #[clap(long = "trace-file")]
    pub trace_file: Option<emma65::emulator::ExpandedPathBuf>,

    /// Path to write structured device/CPU log messages to.
    #[clap(long = "log-file")]
    pub log_file: Option<emma65::emulator::ExpandedPathBuf>,
}

/// The parsed `AppConfig` plus the raw `--profile` value, if given. The
/// profile isn't applied yet at this point — that happens via
/// [`apply_named_profile`] once `main` is ready to hold the resulting
/// tempdir guard alive for the rest of the run.
pub struct LoadedConfig {
    /// The fully merged configuration (CLI args, env vars, and `--config`'s
    /// TOML file, if given).
    pub app: AppConfig,
    /// The raw `--profile <id>` value, if given; not yet merged into `app`.
    pub profile: Option<String>,
}

impl AppConfig {

    pub fn load() -> Result<LoadedConfig, Box<figment::Error>> {
        let cli = CliArgs::parse();
        let mut figment = Figment::new();
        if let Some(path) = &cli.config {
            figment = figment.merge(Toml::file(path))
        }
        let app = figment
            .merge(Env::prefixed("EMMA65_").map(|k| k.as_str().replace('_', "-").into()))
            .merge(Serialized::globals(&cli.app))
            .extract()
            .map_err(Box::new)?;
        Ok(LoadedConfig { app, profile: cli.profile })
    }

}

/// If `--profile <id>` was given, materializes that bundled starter-profile
/// template into a fresh tempdir, loads it through the same
/// `Figment`/`Toml::file()` path used for `--config`, and merges its devices
/// into `config` — preserving any `cpu_variant_spec`/`clock_speed_hz`
/// already set from CLI/env. Returns the tempdir handle (must be kept alive
/// until `Config::build()` completes).
///
/// Unlike [`apply_default_if_unconfigured`]'s `.expect()`s — safe only
/// because they always materialize the fixed, always-valid bundled default —
/// this takes a user-supplied `id` and returns a proper `Err` rather than
/// panicking when it names no registered template.
pub fn apply_named_profile(config: &mut AppConfig, id: &str) -> Result<tempfile::TempDir, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("failed to create tempdir for profile '{id}': {e}"))?;
    let toml_path = emma65::emulator::config::templates::materialize(id, dir.path())
        .map_err(|e| format!("failed to materialize profile '{id}': {e}"))?;
    let template: emma65::emulator::Config = Figment::new()
        .merge(Toml::file(&toml_path))
        .extract()
        .map_err(|e| format!("bundled profile '{id}' failed to parse: {e}"))?;
    config.emulator.cpu_variant_spec = config.emulator.cpu_variant_spec.take().or(template.cpu_variant_spec);
    config.emulator.clock_speed_hz = config.emulator.clock_speed_hz.take().or(template.clock_speed_hz);
    config.emulator.devices = template.devices;
    Ok(dir)
}

/// If no devices are configured, materializes the bundled default
/// configuration (ROM image, VICE labels, and `emulator.toml`) into a fresh
/// tempdir, loads it through the same `Figment`/`Toml::file()` path used for
/// a user-supplied `--config`, and merges its devices into `config` —
/// preserving any `cpu_variant_spec`/`clock_speed_hz` already set from
/// CLI/env/`--config`. Returns the tempdir handle (must be kept alive until
/// `Config::build()` completes).
pub fn apply_default_if_unconfigured(config: &mut AppConfig) -> Option<tempfile::TempDir> {
    if config.emulator.devices.as_ref().is_none_or(|d| d.is_empty()) {
        let dir = tempfile::tempdir().expect("failed to create tempdir for default config");
        let toml_path = emma65::emulator::config::default::materialize_config(dir.path())
            .expect("failed to materialize default config");
        let default: emma65::emulator::Config = Figment::new()
            .merge(Toml::file(&toml_path))
            .extract()
            .expect("bundled default config failed to parse");
        config.emulator.cpu_variant_spec = config.emulator.cpu_variant_spec.take().or(default.cpu_variant_spec);
        config.emulator.clock_speed_hz = config.emulator.clock_speed_hz.take().or(default.clock_speed_hz);
        config.emulator.devices = default.devices;
        Some(dir)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_app_config() -> AppConfig {
        AppConfig {
            emulator: emma65::emulator::Config { cpu_variant_spec: None, clock_speed_hz: None, devices: None },
            trace_file: None,
            log_file: None,
        }
    }

    #[test]
    fn apply_named_profile_default_merges_seven_devices() {
        let mut config = empty_app_config();
        let _dir = apply_named_profile(&mut config, "default").unwrap();
        assert_eq!(config.emulator.devices.as_ref().map(Vec::len), Some(7));
    }

    #[test]
    fn apply_named_profile_msbasic_merges_four_devices() {
        let mut config = empty_app_config();
        let _dir = apply_named_profile(&mut config, "msbasic").unwrap();
        assert_eq!(config.emulator.devices.as_ref().map(Vec::len), Some(4));
    }

    #[test]
    fn apply_named_profile_unknown_id_returns_error_not_panic() {
        let mut config = empty_app_config();
        let err = apply_named_profile(&mut config, "nope").unwrap_err();
        assert!(err.contains("nope"), "expected the bad id in the error message: {err}");
    }

    #[test]
    fn cli_args_reject_config_and_profile_together() {
        let result = CliArgs::try_parse_from(["emma65", "--config", "/tmp/x.toml", "--profile", "default"]);
        assert!(result.is_err(), "--config and --profile should be mutually exclusive");
    }
}
