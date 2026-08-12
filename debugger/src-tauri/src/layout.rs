//! Debugger dock layout persistence: the dockview panel arrangement,
//! persisted as opaque JSON — see issue #382. Also carries the "Terminal is
//! currently detached to its own window" flag (issue #385) alongside it,
//! rather than in `preferences.rs`'s `ui.toml`, since it's a property of the
//! dock arrangement rather than a general UI preference.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;
use tauri::{AppHandle, Manager, State};

use crate::profile;

/// The persisted dock layout: dockview's own serialized arrangement (treated
/// as opaque JSON — this module never parses its internal schema) plus the
/// terminal-detached flag.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DockLayoutData {
    /// dockview's `api.toJSON()` output, or `None` if nothing has been
    /// persisted yet this profile.
    #[serde(default)]
    pub dockview: Option<Value>,
    /// True if the Terminal panel is currently detached to its own window
    /// rather than docked in the main layout.
    #[serde(default)]
    pub terminal_detached: bool,
}

/// Managed state wrapping the last-known dock layout data.
pub struct LayoutState(pub Mutex<DockLayoutData>);

/// Reads `layout.json` from `dir`, returning the default (empty) value if
/// missing or unparseable — a schema change in a future dockview version
/// degrades to "layout not restored" rather than a crash.
pub fn load_dock_layout_from(dir: &Path) -> DockLayoutData {
    fs::read_to_string(dir.join("layout.json")).ok().and_then(|contents| serde_json::from_str(&contents).ok()).unwrap_or_default()
}

/// Writes `data` to `layout.json` under `dir`, creating the directory if it
/// doesn't exist.
fn save_dock_layout_to(dir: &Path, data: &DockLayoutData) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("Failed to create config directory: {e}"))?;
    let contents = serde_json::to_string(data).map_err(|e| format!("Failed to serialize dock layout: {e}"))?;
    fs::write(dir.join("layout.json"), contents).map_err(|e| format!("Failed to write dock layout: {e}"))
}

/// Returns the last persisted dock layout data.
#[tauri::command]
pub fn get_dock_layout(state: State<LayoutState>) -> DockLayoutData {
    state.0.lock().unwrap().clone()
}

/// Persists `layout` (dockview's own serialized arrangement) to
/// `~/.emma/debugger/config/layout.json`, leaving the terminal-detached flag
/// untouched.
#[tauri::command]
pub fn set_dock_layout(layout: Value, state: State<LayoutState>) -> Result<(), String> {
    let data = {
        let mut guard = state.0.lock().unwrap();
        guard.dockview = Some(layout);
        guard.clone()
    };
    save_dock_layout_to(&profile::config_dir()?, &data)
}

/// Updates the terminal-detached flag and persists it, leaving the dockview
/// arrangement untouched. Called from `terminal.rs`'s detach/reattach paths
/// — not exposed as a Tauri command, since the frontend never sets this flag
/// directly; it only ever follows from a Rust-driven detach or reattach.
pub(crate) fn set_terminal_detached(app: &AppHandle, detached: bool) -> Result<(), String> {
    let data = {
        let state = app.state::<LayoutState>();
        let mut guard = state.0.lock().unwrap();
        guard.terminal_detached = detached;
        guard.clone()
    };
    save_dock_layout_to(&profile::config_dir()?, &data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_default_when_layout_file_missing() {
        let dir = std::env::temp_dir().join(format!("emma65-layout-test-missing-{:?}", std::thread::current().id()));
        assert_eq!(load_dock_layout_from(&dir), DockLayoutData::default());
    }

    #[test]
    fn returns_default_when_layout_file_unparseable() {
        let dir = std::env::temp_dir().join(format!("emma65-layout-test-corrupt-{:?}", std::thread::current().id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("layout.json"), "not valid json").unwrap();
        assert_eq!(load_dock_layout_from(&dir), DockLayoutData::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_and_load_round_trip_via_tempdir() {
        let dir = std::env::temp_dir().join(format!("emma65-layout-test-roundtrip-{:?}", std::thread::current().id()));
        let data = DockLayoutData {
            dockview: Some(serde_json::json!({"grid": {"root": {"type": "leaf"}}})),
            terminal_detached: true,
        };
        save_dock_layout_to(&dir, &data).unwrap();
        let loaded = load_dock_layout_from(&dir);
        assert_eq!(loaded, data);
        let _ = fs::remove_dir_all(&dir);
    }
}
