//! Debugger UI preferences: theme selection, exit confirmation, and the
//! last-used file chooser directory — persisted state and Tauri commands.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, PhysicalPosition, PhysicalSize, State, WebviewWindow};

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

/// xterm.js's own built-in default for `ITerminalOptions.scrollback`, reused
/// here as this config's default so an absent/fresh `ui.toml` reproduces the
/// same behavior the terminal already had before `terminal_scrollback`
/// existed.
fn default_terminal_scrollback() -> u32 {
    1000
}

/// A window's last-known size, screen position, and maximized/fullscreen
/// state, captured by `capture_window_geometry` and re-applied by
/// `apply_window_geometry`.
///
/// `x`/`y` are the *outer* position (including window-manager decorations,
/// matching `set_position`'s `set_outer_position` semantics) while
/// `width`/`height` are the *inner* content-area size (matching
/// `set_size`'s `set_inner_size` semantics) — mixing outer position with
/// inner size is intentional and required, not an inconsistency: using
/// `outer_size` here instead grew the window by one titlebar's height on
/// every detach/reattach cycle, since each captured outer size got
/// re-applied as the new *inner* size — caught in #419's own UAT.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowGeometry {
    /// Outer X position, in physical pixels.
    pub x: i32,
    /// Outer Y position, in physical pixels.
    pub y: i32,
    /// Inner (content area) width, in physical pixels.
    pub width: u32,
    /// Inner (content area) height, in physical pixels.
    pub height: u32,
    /// Whether the window was maximized.
    pub maximized: bool,
    /// Whether the window was in OS-level fullscreen.
    pub fullscreen: bool,
}

/// Persisted debugger UI preferences that aren't scoped to any profile — see
/// issue #68 for the original theme-only version, issue #349 for the
/// exit-confirmation addition, issue #357 for the file dialog directory,
/// issue #385 for the terminal scrollback line count, and issue #419 for the
/// window geometry fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// Number of lines the terminal (docked or detached — see issue #385)
    /// keeps in its scrollback buffer beyond its visible rows. Not yet
    /// exposed via any UI control — edit `ui.toml` directly to change it,
    /// per UAT feedback on #385; a future story may add a proper preference
    /// control.
    #[serde(default = "default_terminal_scrollback")]
    pub terminal_scrollback: u32,
    /// Last-known size, position, and maximized/fullscreen state of the main
    /// window; `None` before it's ever been captured, in which case
    /// `tauri.conf.json`'s configured default applies — see issue #419.
    #[serde(default)]
    pub main_window_geometry: Option<WindowGeometry>,
    /// Same as `main_window_geometry`, but for the detached-Terminal window
    /// (`terminal::TERMINAL_DETACHED_WINDOW_LABEL`). Only updated while that
    /// window is actually detached (visible) — see issue #419.
    #[serde(default)]
    pub terminal_window_geometry: Option<WindowGeometry>,
    /// Terminal font family, by CSS font-family name. `None` means "use the
    /// platform default monospace font" — there's no hardcoded fallback name
    /// here because only the webview knows what fonts are actually installed
    /// on the host; the frontend resolves the fallback. Not yet exposed via
    /// any UI control — edit `ui.toml` directly to change it — see issue #462.
    #[serde(default)]
    pub terminal_font_family: Option<String>,
    /// Terminal font size, in pixels. `None` means "use the platform
    /// default". Not yet exposed via any UI control — edit `ui.toml` directly
    /// to change it — see issue #462.
    #[serde(default)]
    pub terminal_font_size: Option<u32>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            skip_exit_confirmation: false,
            last_file_dialog_dir: None,
            terminal_scrollback: default_terminal_scrollback(),
            main_window_geometry: None,
            terminal_window_geometry: None,
            terminal_font_family: None,
            terminal_font_size: None,
        }
    }
}

/// The terminal font family and size, as returned by `get_terminal_font`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TerminalFont {
    pub family: Option<String>,
    pub size: Option<u32>,
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

/// Reads `window`'s current outer position, inner size, and
/// maximized/fullscreen state — matching what `apply_window_geometry`'s
/// `set_position`/`set_size` calls actually move/resize (see
/// `WindowGeometry`'s doc comment). Returns `None` if the underlying
/// platform call fails (e.g. the window has already been destroyed), in
/// which case the caller should leave whatever geometry was already
/// persisted alone rather than overwrite it with a default.
fn capture_window_geometry(window: &WebviewWindow) -> Option<WindowGeometry> {
    let position = window.outer_position().ok()?;
    let size = window.inner_size().ok()?;
    Some(WindowGeometry {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        maximized: window.is_maximized().unwrap_or(false),
        fullscreen: window.is_fullscreen().unwrap_or(false),
    })
}

/// Applies `geometry` to `window`: position and size first, then
/// maximized/fullscreen state (applied last so they aren't immediately
/// undone by the position/size calls). Each call's error is ignored
/// independently — a platform that rejects one of these (e.g. Wayland's
/// refusal to reposition top-level windows) should still end up with as much
/// of the saved geometry as it can honor, rather than none of it.
pub fn apply_window_geometry(window: &WebviewWindow, geometry: &WindowGeometry) {
    let _ = window.set_position(PhysicalPosition::new(geometry.x, geometry.y));
    let _ = window.set_size(PhysicalSize::new(geometry.width, geometry.height));
    if geometry.maximized {
        let _ = window.maximize();
    }
    if geometry.fullscreen {
        let _ = window.set_fullscreen(true);
    }
}

/// Captures `window`'s current geometry and persists it to `ui.toml`,
/// writing it into whichever of `UiConfig`'s `main_window_geometry`/
/// `terminal_window_geometry` field `setter` targets. A no-op (not an error)
/// if the geometry can't be captured, so a window mid-teardown at exit just
/// keeps its last-known persisted geometry instead of losing it.
pub fn save_window_geometry(
    window: &WebviewWindow,
    state: &UiConfigState,
    setter: impl FnOnce(&mut UiConfig, WindowGeometry),
) -> Result<(), String> {
    let Some(geometry) = capture_window_geometry(window) else { return Ok(()) };
    let config = {
        let mut guard = state.0.lock().unwrap();
        setter(&mut guard, geometry);
        guard.clone()
    };
    save_ui_config_to(&profile::config_dir()?, &config)
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

/// Returns the terminal's configured scrollback line count — see
/// `UiConfig::terminal_scrollback`'s doc comment for how to change it.
#[tauri::command]
pub fn get_terminal_scrollback(state: State<UiConfigState>) -> u32 {
    state.0.lock().unwrap().terminal_scrollback
}

/// Returns the terminal's configured font family and size — see
/// `UiConfig::terminal_font_family`/`terminal_font_size`'s doc comments for
/// how to change them. Either or both may be `None`, in which case the
/// frontend resolves its own platform-default fallback.
#[tauri::command]
pub fn get_terminal_font(state: State<UiConfigState>) -> TerminalFont {
    let guard = state.0.lock().unwrap();
    TerminalFont { family: guard.terminal_font_family.clone(), size: guard.terminal_font_size }
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
    fn defaults_window_geometry_fields_to_none_when_missing() {
        let config: UiConfig = toml::from_str("").unwrap();
        assert_eq!(config.main_window_geometry, None);
        assert_eq!(config.terminal_window_geometry, None);
    }

    #[test]
    fn round_trips_window_geometry() {
        let geometry = WindowGeometry { x: 100, y: 50, width: 1650, height: 1000, maximized: false, fullscreen: true };
        let config = UiConfig { main_window_geometry: Some(geometry), terminal_window_geometry: Some(geometry), ..Default::default() };
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: UiConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.main_window_geometry, Some(geometry));
        assert_eq!(deserialized.terminal_window_geometry, Some(geometry));
    }

    #[test]
    fn defaults_terminal_scrollback_to_1000_when_missing() {
        let config: UiConfig = toml::from_str("").unwrap();
        assert_eq!(config.terminal_scrollback, 1000);
    }

    #[test]
    fn round_trips_terminal_scrollback() {
        let config = UiConfig { terminal_scrollback: 5000, ..Default::default() };
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: UiConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.terminal_scrollback, 5000);
    }

    #[test]
    fn defaults_terminal_font_fields_to_none_when_missing() {
        let config: UiConfig = toml::from_str("").unwrap();
        assert_eq!(config.terminal_font_family, None);
        assert_eq!(config.terminal_font_size, None);
    }

    #[test]
    fn round_trips_terminal_font_fields() {
        let config = UiConfig {
            terminal_font_family: Some("Fira Code".to_string()),
            terminal_font_size: Some(16),
            ..Default::default()
        };
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: UiConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.terminal_font_family, Some("Fira Code".to_string()));
        assert_eq!(deserialized.terminal_font_size, Some(16));
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
