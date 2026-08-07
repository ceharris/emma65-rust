use clap::Parser;
use emma65::emulator::Config;
use figment::providers::{Env, Format, Toml};
use figment::Figment;

// CLI args.
// This struct exists solely to capture the `--config` option before Figment runs. It must not
// derive Serde's Serialize or Deserialize.
#[derive(Parser)]
struct Args {
    /// Path to a TOML emulator configuration file
    #[clap(long = "config")]
    config: Option<std::path::PathBuf>,
}

fn main() {
    let args = Args::parse();

    let mut figment = Figment::new();
    if let Some(path) = &args.config {
        figment = figment.merge(Toml::file(path));
    }
    figment = figment.merge(Env::prefixed("EMMA65_").map(|k| k.as_str().replace('_', "-").into()));

    let config: Config = match figment.extract() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "emma65-vscode-adapter: config loaded (cpu-variant={}, clock-speed-hz={:?})",
        config.cpu_variant_spec.map_or_else(|| "default".to_string(), |v| v.to_string()),
        config.clock_speed_hz,
    );
}
