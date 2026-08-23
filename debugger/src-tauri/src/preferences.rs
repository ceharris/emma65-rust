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

// Defaults mirror the pixel widths `symbols.scss` used before columns became
// user-resizable (issue #489) — an absent/fresh `ui.toml` should reproduce
// the same initial layout the panel always had.
fn default_symbols_name_width() -> u32 {
    140
}
fn default_symbols_address_width() -> u32 {
    68
}
fn default_symbols_source_width() -> u32 {
    160
}

/// User-resizable column widths for the Symbols panel's Name/Address/Source
/// columns, in CSS pixels — see issue #489. The trailing Aliases column
/// always fills whatever space remains, so it has no persisted width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SymbolsColumnWidths {
    #[serde(default = "default_symbols_name_width")]
    pub name: u32,
    #[serde(default = "default_symbols_address_width")]
    pub address: u32,
    #[serde(default = "default_symbols_source_width")]
    pub source: u32,
}

impl Default for SymbolsColumnWidths {
    fn default() -> Self {
        Self { name: default_symbols_name_width(), address: default_symbols_address_width(), source: default_symbols_source_width() }
    }
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
/// issue #385 for the terminal scrollback line count, issue #419 for the
/// window geometry fields, issue #467 for the structured terminal
/// preferences block, and issue #489 for the Symbols panel column widths.
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
    /// Same as `terminal_window_geometry`, but for the detached-Display window
    /// (`display::DISPLAY_DETACHED_WINDOW_LABEL`) — see the memory-mapped
    /// display device plan, Work Unit 4.
    #[serde(default)]
    pub display_window_geometry: Option<WindowGeometry>,
    /// Structured terminal preferences (text/cursor/compatibility) — see
    /// issue #467. Replaces the formerly flat `terminal_font_family`/
    /// `terminal_font_size`/`terminal_scrollback` fields; a `ui.toml`
    /// written before #467 simply has these values reset to their defaults
    /// the first time it's loaded, since this is single-user local state.
    #[serde(default)]
    pub terminal_preferences: TerminalPreferences,
    /// User-resizable Name/Address/Source column widths for the Symbols
    /// panel — see issue #489.
    #[serde(default)]
    pub symbols_column_widths: SymbolsColumnWidths,
}

/// Active-cursor shape, mapping directly onto xterm.js's `cursorStyle`
/// option — see issue #467.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// Inactive-cursor shape, mapping directly onto xterm.js's
/// `cursorInactiveStyle` option — see issue #467.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorInactiveShape {
    #[default]
    Outline,
    Block,
    Underline,
    Bar,
    None,
}

/// What a compatibility-remapped key (Backspace or Delete) sends: the ASCII
/// Backspace control code, the ASCII Delete control code, or the ANSI
/// Delete Character (DCH) escape sequence — see issue #467.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalKeyAction {
    Bs,
    Del,
    Dch,
}

fn default_backspace_key() -> TerminalKeyAction {
    TerminalKeyAction::Del
}

fn default_delete_key() -> TerminalKeyAction {
    TerminalKeyAction::Dch
}

/// Text-tab terminal preferences: font, the 16-color ANSI palette, and
/// scrollback length — see issue #467.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalTextPreferences {
    /// Terminal font family, by CSS font-family name. `None` means "use the
    /// platform default monospace font" — there's no hardcoded fallback name
    /// here because only the webview knows what fonts are actually installed
    /// on the host; the frontend resolves the fallback.
    #[serde(default)]
    pub font_family: Option<String>,
    /// Terminal font size, in pixels. `None` means "use the platform
    /// default".
    #[serde(default)]
    pub font_size: Option<u32>,
    /// Number of lines the terminal (docked or detached — see issue #385)
    /// keeps in its scrollback buffer beyond its visible rows.
    #[serde(default = "default_terminal_scrollback")]
    pub scrollback: u32,
    /// Foreground color (hex). `None` follows the current light/dark theme.
    #[serde(default)]
    pub foreground: Option<String>,
    /// Background color (hex). `None` follows the current light/dark theme.
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub black: Option<String>,
    #[serde(default)]
    pub red: Option<String>,
    #[serde(default)]
    pub green: Option<String>,
    #[serde(default)]
    pub yellow: Option<String>,
    #[serde(default)]
    pub blue: Option<String>,
    #[serde(default)]
    pub magenta: Option<String>,
    #[serde(default)]
    pub cyan: Option<String>,
    #[serde(default)]
    pub white: Option<String>,
    #[serde(default)]
    pub bright_black: Option<String>,
    #[serde(default)]
    pub bright_red: Option<String>,
    #[serde(default)]
    pub bright_green: Option<String>,
    #[serde(default)]
    pub bright_yellow: Option<String>,
    #[serde(default)]
    pub bright_blue: Option<String>,
    #[serde(default)]
    pub bright_magenta: Option<String>,
    #[serde(default)]
    pub bright_cyan: Option<String>,
    #[serde(default)]
    pub bright_white: Option<String>,
}

impl Default for TerminalTextPreferences {
    fn default() -> Self {
        Self {
            font_family: None,
            font_size: None,
            scrollback: default_terminal_scrollback(),
            foreground: None,
            background: None,
            black: None,
            red: None,
            green: None,
            yellow: None,
            blue: None,
            magenta: None,
            cyan: None,
            white: None,
            bright_black: None,
            bright_red: None,
            bright_green: None,
            bright_yellow: None,
            bright_blue: None,
            bright_magenta: None,
            bright_cyan: None,
            bright_white: None,
        }
    }
}

/// Cursor-tab terminal preferences: active/inactive shape, blink, and
/// color/accent color — see issue #467.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalCursorPreferences {
    /// Maps directly to xterm.js's `cursorStyle` option.
    #[serde(default)]
    pub active_shape: CursorShape,
    /// Maps directly to xterm.js's `cursorInactiveStyle` option.
    #[serde(default)]
    pub inactive_shape: CursorInactiveShape,
    /// Maps to xterm.js's `cursorBlink` option. On/off only — see the
    /// terminal-preferences plan doc's "Blink" decision for why there's no
    /// rate control.
    #[serde(default)]
    pub blink: bool,
    /// Cursor color (hex, `ITheme.cursor`). `None` follows the current
    /// light/dark theme.
    #[serde(default)]
    pub color: Option<String>,
    /// Cursor accent color (hex, `ITheme.cursorAccent`). `None` follows the
    /// current light/dark theme.
    #[serde(default)]
    pub accent_color: Option<String>,
}

impl Default for TerminalCursorPreferences {
    fn default() -> Self {
        Self {
            active_shape: CursorShape::Block,
            inactive_shape: CursorInactiveShape::Outline,
            blink: false,
            color: None,
            accent_color: None,
        }
    }
}

/// Compatibility-tab terminal preferences: what the Backspace and Delete
/// keys send — see issue #467.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalCompatibilityPreferences {
    #[serde(default = "default_backspace_key")]
    pub backspace_key: TerminalKeyAction,
    #[serde(default = "default_delete_key")]
    pub delete_key: TerminalKeyAction,
}

impl Default for TerminalCompatibilityPreferences {
    fn default() -> Self {
        Self { backspace_key: default_backspace_key(), delete_key: default_delete_key() }
    }
}

/// Structured terminal preferences, grouped the same way the Preferences
/// dialog's tabs are (Text/Cursor/Compatibility) — see issue #467. Its own
/// top-level field on `UiConfig` (not flattened) so it can be lifted into a
/// future profile-level override layer without a reshape.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TerminalPreferences {
    #[serde(default)]
    pub text: TerminalTextPreferences,
    #[serde(default)]
    pub cursor: TerminalCursorPreferences,
    #[serde(default)]
    pub compatibility: TerminalCompatibilityPreferences,
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

/// Returns the terminal's configured preferences (text/cursor/compatibility)
/// — see `TerminalPreferences`'s doc comment.
#[tauri::command]
pub fn get_terminal_preferences(state: State<UiConfigState>) -> TerminalPreferences {
    state.0.lock().unwrap().terminal_preferences.clone()
}

/// Replaces the terminal's configured preferences wholesale and persists them
/// to `~/.emma/debugger/config/ui.toml`.
#[tauri::command]
pub fn set_terminal_preferences(preferences: TerminalPreferences, state: State<UiConfigState>) -> Result<(), String> {
    let config = {
        let mut guard = state.0.lock().unwrap();
        guard.terminal_preferences = preferences;
        guard.clone()
    };
    save_ui_config_to(&profile::config_dir()?, &config)
}

/// Returns the Symbols panel's configured Name/Address/Source column widths.
#[tauri::command]
pub fn get_symbols_column_widths(state: State<UiConfigState>) -> SymbolsColumnWidths {
    state.0.lock().unwrap().symbols_column_widths
}

/// Replaces the Symbols panel's column widths wholesale and persists them to
/// `~/.emma/debugger/config/ui.toml`. Called once a column-resize drag ends
/// (`SymbolsPanel.tsx`), not on every drag-move tick.
#[tauri::command]
pub fn set_symbols_column_widths(widths: SymbolsColumnWidths, state: State<UiConfigState>) -> Result<(), String> {
    let config = {
        let mut guard = state.0.lock().unwrap();
        guard.symbols_column_widths = widths;
        guard.clone()
    };
    save_ui_config_to(&profile::config_dir()?, &config)
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
    fn defaults_terminal_preferences_when_missing() {
        let config: UiConfig = toml::from_str("").unwrap();
        assert_eq!(config.terminal_preferences, TerminalPreferences::default());
        assert_eq!(config.terminal_preferences.text.scrollback, 1000);
        assert_eq!(config.terminal_preferences.text.font_family, None);
        assert_eq!(config.terminal_preferences.text.font_size, None);
        assert_eq!(config.terminal_preferences.cursor.active_shape, CursorShape::Block);
        assert_eq!(config.terminal_preferences.cursor.inactive_shape, CursorInactiveShape::Outline);
        assert!(!config.terminal_preferences.cursor.blink);
        assert_eq!(config.terminal_preferences.compatibility.backspace_key, TerminalKeyAction::Del);
        assert_eq!(config.terminal_preferences.compatibility.delete_key, TerminalKeyAction::Dch);
    }

    #[test]
    fn defaults_terminal_preferences_fields_when_partially_specified() {
        let config: UiConfig = toml::from_str("[terminal_preferences.text]\nscrollback = 5000\n").unwrap();
        assert_eq!(config.terminal_preferences.text.scrollback, 5000);
        assert_eq!(config.terminal_preferences.text.font_family, None);
        assert_eq!(config.terminal_preferences.cursor.active_shape, CursorShape::Block);
    }

    #[test]
    fn round_trips_terminal_preferences() {
        let preferences = TerminalPreferences {
            text: TerminalTextPreferences {
                font_family: Some("Fira Code".to_string()),
                font_size: Some(16),
                scrollback: 5000,
                foreground: Some("#d4d4d4".to_string()),
                background: Some("#1e1e1e".to_string()),
                red: Some("#ff0000".to_string()),
                ..Default::default()
            },
            cursor: TerminalCursorPreferences {
                active_shape: CursorShape::Bar,
                inactive_shape: CursorInactiveShape::None,
                blink: true,
                color: Some("#ffffff".to_string()),
                accent_color: Some("#000000".to_string()),
            },
            compatibility: TerminalCompatibilityPreferences {
                backspace_key: TerminalKeyAction::Bs,
                delete_key: TerminalKeyAction::Del,
            },
        };
        let config = UiConfig { terminal_preferences: preferences.clone(), ..Default::default() };
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: UiConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.terminal_preferences, preferences);
    }

    #[test]
    fn round_trips_last_file_dialog_dir() {
        let config = UiConfig { last_file_dialog_dir: Some("/home/user/roms".to_string()), ..Default::default() };
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: UiConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.last_file_dialog_dir, Some("/home/user/roms".to_string()));
    }

    #[test]
    fn defaults_symbols_column_widths_when_missing() {
        let config: UiConfig = toml::from_str("").unwrap();
        assert_eq!(config.symbols_column_widths, SymbolsColumnWidths::default());
        assert_eq!(config.symbols_column_widths.name, 140);
        assert_eq!(config.symbols_column_widths.address, 68);
        assert_eq!(config.symbols_column_widths.source, 160);
    }

    #[test]
    fn round_trips_symbols_column_widths() {
        let widths = SymbolsColumnWidths { name: 220, address: 80, source: 190 };
        let config = UiConfig { symbols_column_widths: widths, ..Default::default() };
        let serialized = toml::to_string(&config).unwrap();
        let deserialized: UiConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.symbols_column_widths, widths);
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
