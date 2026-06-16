use clap::Parser;
use serde::{Deserialize, Serialize};
use super::DeviceSpec;

#[derive(Debug, Clone, Parser, Serialize, Deserialize)]
#[clap(name = "Emulator")]
#[serde(rename_all = "kebab-case")]
pub struct AppConfig {
    pub clock_speed_hz: Option<u64>,

    #[clap(long = "device", num_args = 1..)]
    pub devices: Option<Vec<DeviceSpec>>,

}