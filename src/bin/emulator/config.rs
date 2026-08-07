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

/// If no devices are configured, replaces `config.emulator` with the built-in default RAM +
/// ROM + console layout (see [`emma65::emulator::default_config`]) and returns the tempfile
/// handle backing the ROM image (must be kept alive until `Config::build()` completes).
pub fn apply_default_if_unconfigured(config: &mut AppConfig, default_rom: &[u8]) -> Option<tempfile::NamedTempFile> {
    if config.emulator.devices.as_ref().is_none_or(|d| d.is_empty()) {
        let (default, f) = emma65::emulator::default_config(default_rom);
        config.emulator = default;
        Some(f)
    } else {
        None
    }
}
