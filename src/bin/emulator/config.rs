use clap::Parser;
use figment::{Figment, providers::{Env, Format, Serialized, Toml}};
use serde::{Deserialize, Serialize};

// CLI args.
// This struct exists solely to capture the `--config` option before Figment runs. It must not
// derive Serde's Serialize or Deserialize.
#[derive(Parser)]
struct CliArgs {
    /// Path to a TOML configuration file
    #[clap(long = "config")]
    config: Option<std::path::PathBuf>,

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

impl AppConfig {

    pub fn load() -> Result<Self, Box<figment::Error>> {
        let cli = CliArgs::parse();
        let mut figment = Figment::new();
        if let Some(path) = cli.config {
            figment = figment.merge(Toml::file(path))
        }
        figment
            .merge(Env::prefixed("EMMA65_").map(|k| k.as_str().replace('_', "-").into()))
            .merge(Serialized::globals(&cli.app))
            .extract()
            .map_err(Box::new)
    }

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
        let toml_path = emma65::emulator::config::default::materialize_default_config(dir.path())
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
