//! Debugger dock layout persistence: the dockview panel arrangement,
//! persisted as opaque JSON — see issue #382. Also carries the "Terminal is
//! currently detached to its own window" flag (issue #385) and the per-panel
//! last-known-dock-position map (issue #393) alongside it, rather than in
//! `preferences.rs`'s `ui.toml`, since both are properties of the dock
//! arrangement rather than a general UI preference.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::profile;

/// The persisted dock layout: dockview's own serialized arrangement (treated
/// as opaque JSON — this module never parses its internal schema) plus the
/// terminal-detached flag and the per-panel last-known-position map.
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
    /// Opaque per-panel "last known dock position" map (issue #393):
    /// dockview panel id -> `{group_id, index}`, kept up to date by
    /// `DockLayout.tsx`'s `recordPanelPositions` and consulted when the View
    /// menu needs to re-add a panel whose dock tab was closed. Persisted
    /// alongside `dockview` rather than derived from it, since dockview's own
    /// serialization only ever describes currently-present panels, never
    /// ones that were removed — this module still never parses its shape.
    #[serde(default)]
    pub panel_positions: Option<Value>,
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

/// Persists `layout` (dockview's own serialized arrangement) and
/// `panel_positions` (the per-panel last-known-position map — see
/// `DockLayoutData`) to `~/.emma/debugger/config/layout.json`, leaving the
/// terminal-detached flag untouched. The two always originate from, and are
/// written by, the same `onDidLayoutChange` tick in `DockLayout.tsx`, so
/// they're accepted and persisted together rather than via separate calls.
#[tauri::command]
pub fn set_dock_layout(layout: Value, panel_positions: Value, state: State<LayoutState>) -> Result<(), String> {
    let data = {
        let mut guard = state.0.lock().unwrap();
        guard.dockview = Some(layout);
        guard.panel_positions = Some(panel_positions);
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

/// Discards the persisted dock arrangement and per-panel position map
/// (issue #398's Window > Restore Layout…), reattaching Terminal first if
/// it's currently detached to its own window — the default layout always
/// docks Terminal, so a restore that left it floating separately wouldn't
/// actually be "default." Invoked from the frontend confirmation dialog
/// opened by `emit_open_restore_layout_dialog`, once the user clicks
/// "Restore". This only clears the Rust-side record and re-saves the empty
/// result to `layout.json`; the `dock-layout-reset` event it emits is what
/// tells `DockLayout.tsx` to actually clear and rebuild the live dockview
/// arrangement (and re-persist it, which is what makes the reset stick
/// rather than being immediately overwritten by the next debounced save of
/// whatever was on screen before this ran).
#[tauri::command]
pub fn restore_dock_layout(app: AppHandle) -> Result<(), String> {
    let was_detached = app.state::<LayoutState>().0.lock().unwrap().terminal_detached;
    if was_detached {
        crate::terminal::reattach_terminal(&app);
    }
    let data = {
        let state = app.state::<LayoutState>();
        let mut guard = state.0.lock().unwrap();
        guard.dockview = None;
        guard.panel_positions = None;
        guard.clone()
    };
    save_dock_layout_to(&profile::config_dir()?, &data)?;
    let _ = app.emit_to(crate::MAIN_WINDOW_LABEL, "dock-layout-reset", ());
    Ok(())
}

/// Emits `open-restore-layout-dialog`, telling the main window's React layer
/// to open the Restore Layout confirmation modal. Called directly from the
/// Window > Restore Layout… menu item's `on_menu_event` handler, mirroring
/// `recent::emit_open_clear_recent_dialog`.
pub(crate) fn emit_open_restore_layout_dialog(app: &AppHandle) {
    let _ = app.emit("open-restore-layout-dialog", ());
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
            panel_positions: Some(serde_json::json!({"trace": {"group_id": "group_1", "index": 0}})),
        };
        save_dock_layout_to(&dir, &data).unwrap();
        let loaded = load_dock_layout_from(&dir);
        assert_eq!(loaded, data);
        let _ = fs::remove_dir_all(&dir);
    }
}
