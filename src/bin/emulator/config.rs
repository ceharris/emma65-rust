use clap::Parser;
use figment::{Figment, providers::{Format, Toml, Env, Serialized}};
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