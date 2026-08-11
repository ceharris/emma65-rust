//! Debugger UI preferences: theme selection, exit confirmation, and the
//! last-used file chooser directory — persisted state and Tauri commands.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, State};

use crate::profile;

/// Selected debugger theme mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Follow the OS/webview `prefers-color-scheme` setting; reacts live to OS changes.
    #[default]
    Auto,
    /// Always use the dark palette, regardless of the OS setting.
    Dark,
    /// Always use the light palette, regardless of the OS setting.
    Light,
}

/// Persisted debugger UI preferences that aren't scoped to any profile — see
/// issue #68 for the original theme-only version, issue #349 for the
/// exit-confirmation addition, and issue #357 for the file dialog directory.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UiConfig {
    /// The user's selected theme mode.
    #[serde(default)]
    pub theme: ThemeMode,
    /// Skips the exit confirmation dialog (File > Exit, Ctrl+Q, or closing
    /// the main window) when true. Set via that dialog's "Don't ask again"
    /// checkbox; there is intentionally no UI to revert it — see issue #349.
    #[serde(default)]
    pub skip_exit_confirmation: bool,
    /// Directory the native file chooser should start in for the Load
    /// Memory, Save Memory, and Record Trace actions, updated after each
    /// successful selection; `None` lets the OS pick its own default — see
    /// issue #357.
    #[serde(default)]
    pub last_file_dialog_dir: Option<String>,
}

/// Managed state wrapping the current [`UiConfig`].
pub struct UiConfigState(pub Mutex<UiConfig>);

/// Reads `ui.toml` from `dir`, falling back to defaults if missing or invalid.
pub fn load_ui_config_from(dir: &Path) -> UiConfig {
    fs::read_to_string(dir.join("ui.toml"))
        .ok()
        .and_then(|contents| toml::from_str(&contents).ok())
        .unwrap_or_default()
}

/// Writes `config` to `ui.toml` under `dir`, creating the directory if it doesn't exist.
fn save_ui_config_to(dir: &Path, config: &UiConfig) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("Failed to create config directory: {e}"))?;
    let contents = toml::to_string(config).map_err(|e| format!("Failed to serialize UI config: {e}"))?;
    fs::write(dir.join("ui.toml"), contents).map_err(|e| format!("Failed to write UI config: {e}"))
}

/// Returns the currently active theme mode.
#[tauri::command]
pub fn get_theme(state: State<UiConfigState>) -> ThemeMode {
    state.0.lock().unwrap().theme
}

/// Updates the theme mode, persists it to `~/.emma/debugger/config/ui.toml`,
/// and notifies all windows.
#[tauri::command]
pub fn set_theme(mode: ThemeMode, state: State<UiConfigState>, app: AppHandle) -> Result<(), String> {
    let config = {
        let mut guard = state.0.lock().unwrap();
        guard.theme = mode;
        guard.clone()
    };
    save_ui_config_to(&profile::config_dir()?, &config)?;
    let _ = app.emit("theme-changed", mode);
    Ok(())
}

/// Persists `skip` as the "Don't ask again" exit-confirmation preference.
///
/// Called from `lib::confirm_exit` once the user commits the exit
/// confirmation dialog, regardless of the checkbox's state — an unchecked
/// box still needs to overwrite a stale `true` left over from a manual edit
/// of `ui.toml`.
pub fn set_skip_exit_confirmation(skip: bool, state: &UiConfigState) -> Result<(), String> {
    let config = {
        let mut guard = state.0.lock().unwrap();
        guard.skip_exit_confirmation = skip;
        guard.clone()
    };
    save_ui_config_to(&profile::config_dir()?, &config)
}

/// Returns the directory the file chooser should start in for the Load
/// Memory, Save Memory, and Record Trace actions, or `None` if no selection
/// has been made yet.
#[tauri::command]
pub fn get_last_file_dialog_dir(state: State<UiConfigState>) -> Option<String> {
    state.0.lock().unwrap().last_file_dialog_dir.clone()
}

/// Records the directory containing `path` as the file chooser's starting
/// directory, persisting it to `~/.emma/debugger/config/ui.toml`. Called
/// after a successful Load Memory, Save Memory, or Record Trace file
/// selection.
#[tauri::command]
pub fn set_last_file_dialog_dir(path: String, state: State<UiConfigState>) -> Result<(), String> {
    let Some(dir) = parent_dir_string(&path) else {
        return Ok(());
    };
    let config = {
        let mut guard = state.0.lock().unwrap();
        guard.last_file_dialog_dir = Some(dir);
        guard.clone()
    };
    save_ui_config_to(&profile::config_dir()?, &config)
}

/// Returns the parent directory of `path` as an owned `String`, or `None`
/// if `path` has no parent (e.g. it's a bare filename or the root).
fn parent_dir_string(path: &str) -> Option<String> {
    Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_all_theme_modes() {
        for mode in [ThemeMode::Auto, ThemeMode::Dark, ThemeMode::Light] {
            let config = UiConfig { theme: mode, ..Default::default() };
            let serialized = toml::to_string(&config).unwrap();
            let deserialized: UiConfig = toml::from_str(&serialized).unwrap();
            assert_eq!(deserialized.theme, mode);
        }
    }

    #[test]
    fn defaults_to_auto_when_theme_field_missing() {
        let config: UiConfig = toml::from_str("").unwrap();
        assert_eq!(config.theme, ThemeMode::Auto);
    }

    #[test]
    fn defaults_skip_exit_confirmation_to_false_when_missing() {
        let config: UiConfig = toml::from_str("").unwrap();
        assert!(!config.skip_exit_confirmation);
    }

    #[test]
    fn round_trips_skip_exit_confirmation() {
        let config = UiConfig { skip_exit_confirmation: true, ..Default::default() };
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: UiConfig = toml::from_str(&serialized).unwrap();
        assert!(deserialized.skip_exit_confirmation);
    }

    #[test]
    fn defaults_last_file_dialog_dir_to_none_when_missing() {
        let config: UiConfig = toml::from_str("").unwrap();
        assert_eq!(config.last_file_dialog_dir, None);
    }

    #[test]
    fn round_trips_last_file_dialog_dir() {
        let config = UiConfig { last_file_dialog_dir: Some("/home/user/roms".to_string()), ..Default::default() };
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: UiConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.last_file_dialog_dir, Some("/home/user/roms".to_string()));
    }

    #[test]
    fn save_and_load_round_trip_via_tempdir() {
        let dir = std::env::temp_dir().join(format!("emma65-preferences-test-{:?}", std::thread::current().id()));
        let config = UiConfig { theme: ThemeMode::Light, ..Default::default() };
        save_ui_config_to(&dir, &config).unwrap();
        let loaded = load_ui_config_from(&dir);
        assert_eq!(loaded.theme, ThemeMode::Light);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parent_dir_string_returns_containing_directory() {
        assert_eq!(parent_dir_string("/some/dir/memory.bin"), Some("/some/dir".to_string()));
    }

    #[test]
    fn parent_dir_string_none_for_bare_filename() {
        assert_eq!(parent_dir_string("memory.bin"), None);
    }
}
