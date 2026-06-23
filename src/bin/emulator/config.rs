use clap::Parser;
use figment::{Figment, providers::{Format, Toml, Env, Serialized}};
use serde::{Deserialize, Serialize};
use emma65::emulator::CpuVariantSpec;

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

const PTY_LINK_NAME: &str = ".emma/dev/ttyS0";
const DEFAULT_CLOCK_SPEED: u64 = 1_843_200;
const DEFAULT_CPU_VARIANT: CpuVariantSpec = CpuVariantSpec::Wdc6502;

/// If no devices are configured, writes the embedded default ROM to a tempfile,
/// populates `config.emulator.devices` with the default RAM + ROM + console layout,
/// and returns the tempfile handle (must be kept alive until `Config::build()` completes).
pub fn apply_default_if_unconfigured(config: &mut AppConfig, default_rom: &[u8]) -> Option<tempfile::NamedTempFile> {
    if config.emulator.devices.as_ref().is_none_or(|d| d.is_empty()) {
        eprintln!("notice: using default configuration; connect terminal to ~/{}", PTY_LINK_NAME);
        let f = tempfile::Builder::new()
            .suffix(".bin")
            .tempfile()
            .expect("failed to create tempfile for default ROM");
        std::fs::write(f.path(), default_rom)
            .expect("failed to write default ROM to tempfile");
        let rom_path = f.path().to_path_buf();
        config.emulator.cpu_variant_spec.get_or_insert(DEFAULT_CPU_VARIANT);
        config.emulator.clock_speed_hz.get_or_insert(DEFAULT_CLOCK_SPEED);
        let home = std::env::var("HOME").expect("HOME not set");
        let pty_symlink = std::path::Path::new(&home).join(PTY_LINK_NAME);
        config.emulator.devices = Some(vec![
            "ram@0x0000,size=32768,fill=0".parse().unwrap(),
            format!("rom@0x8000,size=32768,image={}", rom_path.display())
                .parse()
                .unwrap(),
            format!("console@0xfff8,transport=pty:{}", pty_symlink.display())
                .parse()
                .unwrap(),
        ]);
        Some(f)
    } else {
        None
    }
}
