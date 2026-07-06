//! Debugger UI theme selection: persisted preference and Tauri commands.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, State};

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

/// Persisted debugger UI preferences.
///
/// Deliberately scoped to the theme alone — see issue #68 and the plan doc's
/// "Deferred Items" section, which excludes broader session/settings persistence.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UiConfig {
    /// The user's selected theme mode.
    #[serde(default)]
    pub theme: ThemeMode,
}

/// Managed state wrapping the current [`UiConfig`].
pub struct UiConfigState(pub Mutex<UiConfig>);

/// Returns `~/.emma/debugger/default/`, the directory holding per-profile config files.
pub fn debugger_config_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable is not set".to_string())?;
    Ok(Path::new(&home).join(".emma/debugger/default"))
}

/// Reads `ui.toml` from `dir`, falling back to defaults if missing or invalid.
fn load_ui_config_from(dir: &Path) -> UiConfig {
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

/// Loads the persisted [`UiConfig`] from `~/.emma/debugger/default/ui.toml`,
/// falling back to defaults if the file is missing, unreadable, or malformed.
pub fn load_ui_config() -> UiConfig {
    match debugger_config_dir() {
        Ok(dir) => load_ui_config_from(&dir),
        Err(_) => UiConfig::default(),
    }
}

/// Returns the currently active theme mode.
#[tauri::command]
pub fn get_theme(state: State<UiConfigState>) -> ThemeMode {
    state.0.lock().unwrap().theme
}

/// Updates the theme mode, persists it to `ui.toml`, and notifies all windows.
#[tauri::command]
pub fn set_theme(mode: ThemeMode, state: State<UiConfigState>, app: AppHandle) -> Result<(), String> {
    let config = {
        let mut guard = state.0.lock().unwrap();
        guard.theme = mode;
        guard.clone()
    };
    let dir = debugger_config_dir()?;
    save_ui_config_to(&dir, &config)?;
    let _ = app.emit("theme-changed", mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_all_theme_modes() {
        for mode in [ThemeMode::Auto, ThemeMode::Dark, ThemeMode::Light] {
            let config = UiConfig { theme: mode };
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
    fn save_and_load_round_trip_via_tempdir() {
        let dir = std::env::temp_dir().join(format!("emma65-theme-test-{:?}", std::thread::current().id()));
        let config = UiConfig { theme: ThemeMode::Light };
        save_ui_config_to(&dir, &config).unwrap();
        let loaded = load_ui_config_from(&dir);
        assert_eq!(loaded.theme, ThemeMode::Light);
        let _ = fs::remove_dir_all(&dir);
    }
}
