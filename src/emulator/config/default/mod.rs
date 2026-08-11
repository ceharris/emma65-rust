//! Bundled default device configuration: a ROM image, its VICE label file,
//! and a device-layout template, embedded in the library so both the
//! `emma65` binary's zero-config fallback and the debugger's `default`
//! profile share one source of truth.
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use super::path::portable_path;

const ROM_IMAGE: &[u8] = include_bytes!("program.bin");
const LABELS: &[u8] = include_bytes!("program.lbl");
const TEMPLATE: &str = include_str!("emulator.toml.template");

/// An error that occurred while materializing the bundled default
/// configuration, naming the file that could not be written.
#[derive(Debug)]
pub struct MaterializeError {
    /// The file that could not be written.
    path: PathBuf,
    /// The underlying I/O failure.
    source: std::io::Error,
}

impl Display for MaterializeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to write '{}': {}", self.path.display(), self.source)
    }
}

impl std::error::Error for MaterializeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn write_file(path: &Path, contents: &[u8]) -> Result<(), MaterializeError> {
    std::fs::write(path, contents).map_err(|source| MaterializeError { path: path.to_path_buf(), source })
}

/// Writes the bundled default ROM image, VICE labels file, and a rendered
/// `emulator.toml` into `dest` (created if missing). Returns the path to
/// the written `emulator.toml`. Used both for the debugger's persistent
/// `default` profile directory and a CLI-launch tempdir — one source of
/// truth for the bundled device layout.
pub fn materialize_default_config(dest: &Path) -> Result<PathBuf, MaterializeError> {
    std::fs::create_dir_all(dest).map_err(|source| MaterializeError { path: dest.to_path_buf(), source })?;

    let rom_path = dest.join("program.bin");
    write_file(&rom_path, ROM_IMAGE)?;

    let labels_path = dest.join("program.lbl");
    write_file(&labels_path, LABELS)?;

    let rendered = TEMPLATE
        .replace("{{ROM_IMAGE}}", &portable_path(&rom_path))
        .replace("{{LABELS}}", &portable_path(&labels_path));
    let toml_path = dest.join("emulator.toml");
    write_file(&toml_path, rendered.as_bytes())?;

    Ok(toml_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::HOME_ENV_LOCK;
    use figment::Figment;
    use figment::providers::{Format, Toml};

    fn temp_dest(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emma65-default-config-test-{name}-{:?}", std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn materialize_writes_all_three_files_with_correct_bytes() {
        let dest = temp_dest("writes-files");
        let toml_path = materialize_default_config(&dest).unwrap();

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
        let toml_path = materialize_default_config(&dest).unwrap();

        let config: crate::emulator::Config = Figment::new()
            .merge(Toml::file(&toml_path))
            .extract()
            .expect("bundled default config failed to parse");

        assert_eq!(config.devices.as_ref().map(Vec::len), Some(8));

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn portable_path_rendering_uses_tilde_shorthand_under_home() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let dest = temp_dest("under-home");
        // SAFETY: HOME_ENV_LOCK excludes every other test using it, across modules.
        unsafe { std::env::set_var("HOME", dest.parent().unwrap()) };

        let toml_path = materialize_default_config(&dest).unwrap();
        let rendered = std::fs::read_to_string(&toml_path).unwrap();
        let dest_name = dest.file_name().unwrap().to_str().unwrap();
        assert!(rendered.contains(&format!("~/{dest_name}/program.bin")), "expected tilde-shorthand path: {rendered}");
        assert!(rendered.contains(&format!("~/{dest_name}/program.lbl")), "expected tilde-shorthand path: {rendered}");

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn portable_path_rendering_uses_absolute_path_outside_home() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let dest = temp_dest("outside-home");
        // SAFETY: HOME_ENV_LOCK excludes every other test using it, across modules.
        unsafe { std::env::set_var("HOME", "/nonexistent-home-for-this-test") };

        let toml_path = materialize_default_config(&dest).unwrap();
        let rendered = std::fs::read_to_string(&toml_path).unwrap();
        assert!(rendered.contains(&format!("\"{}\"", dest.join("program.bin").display())), "expected absolute path: {rendered}");
        assert!(rendered.contains(&format!("\"{}\"", dest.join("program.lbl").display())), "expected absolute path: {rendered}");

        let _ = std::fs::remove_dir_all(&dest);
    }
}
