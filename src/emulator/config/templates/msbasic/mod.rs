//! Bundled Microsoft BASIC starter-profile template: Microsoft 6502 BASIC's ROM
//! image, its VICE labels file, and a device-layout template (RAM, ROM, VIA,
//! console). Registered as the `"msbasic"` starter-profile template in
//! [`super`](super).
use std::path::{Path, PathBuf};

use super::asset::{self, MaterializeError};

const ROM_IMAGE: &[u8] = include_bytes!("program.bin");
const LABELS: &[u8] = include_bytes!("program.lbl");
const TEMPLATE: &str = include_str!("emulator-template.toml");

/// Writes the bundled MS BASIC ROM image, VICE labels file, and a rendered
/// `emulator.toml` into `dest` (created if missing). Returns the path to
/// the written `emulator.toml`.
pub fn materialize_config(dest: &Path) -> Result<PathBuf, MaterializeError> {
    asset::materialize(dest, ROM_IMAGE, LABELS, TEMPLATE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use figment::Figment;
    use figment::providers::{Format, Toml};

    fn temp_dest(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emma65-msbasic-config-test-{name}-{:?}", std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn materialize_writes_all_three_files_with_correct_bytes() {
        let dest = temp_dest("writes-files");
        let toml_path = materialize_config(&dest).unwrap();

        assert_eq!(toml_path, dest.join("emulator.toml"));
        assert_eq!(std::fs::read(dest.join("program.bin")).unwrap(), ROM_IMAGE);
        assert_eq!(std::fs::read(dest.join("program.lbl")).unwrap(), LABELS);
        let rendered = std::fs::read_to_string(&toml_path).unwrap();
        assert!(!rendered.contains("{{"), "rendered TOML must have no leftover template tokens: {rendered}");

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn materialize_renders_a_config_that_round_trips_through_figment() {
        let dest = temp_dest("round-trip");
        let toml_path = materialize_config(&dest).unwrap();

        let config: crate::emulator::Config = Figment::new()
            .merge(Toml::file(&toml_path))
            .extract()
            .expect("bundled msbasic config failed to parse");

        assert_eq!(config.devices.as_ref().map(Vec::len), Some(4));

        let _ = std::fs::remove_dir_all(&dest);
    }
}
