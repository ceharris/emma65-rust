//! Debugger dock layout persistence: the dockview panel arrangement,
//! persisted as opaque JSON — see issue #382.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use serde_json::Value;
use tauri::State;

use crate::profile;

/// Managed state wrapping the last-known dock layout, if any has been
/// persisted or set this session.
pub struct LayoutState(pub Mutex<Option<Value>>);

/// Reads `layout.json` from `dir`, returning `None` if missing or unparseable
/// — dockview's serialization is an arbitrary nested JSON tree that this
/// module never validates beyond "is this parseable JSON," so a schema
/// change in a future dockview version degrades to "layout not restored"
/// rather than a crash.
pub fn load_dock_layout_from(dir: &Path) -> Option<Value> {
    fs::read_to_string(dir.join("layout.json")).ok().and_then(|contents| serde_json::from_str(&contents).ok())
}

/// Writes `layout` to `layout.json` under `dir`, creating the directory if it
/// doesn't exist.
fn save_dock_layout_to(dir: &Path, layout: &Value) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("Failed to create config directory: {e}"))?;
    let contents = serde_json::to_string(layout).map_err(|e| format!("Failed to serialize dock layout: {e}"))?;
    fs::write(dir.join("layout.json"), contents).map_err(|e| format!("Failed to write dock layout: {e}"))
}

/// Returns the last persisted dock layout, or `None` if none has been saved
/// yet or the persisted file couldn't be parsed.
#[tauri::command]
pub fn get_dock_layout(state: State<LayoutState>) -> Option<Value> {
    state.0.lock().unwrap().clone()
}

/// Persists `layout` to `~/.emma/debugger/config/layout.json`.
#[tauri::command]
pub fn set_dock_layout(layout: Value, state: State<LayoutState>) -> Result<(), String> {
    {
        let mut guard = state.0.lock().unwrap();
        *guard = Some(layout.clone());
    }
    save_dock_layout_to(&profile::config_dir()?, &layout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_when_layout_file_missing() {
        let dir = std::env::temp_dir().join(format!("emma65-layout-test-missing-{:?}", std::thread::current().id()));
        assert_eq!(load_dock_layout_from(&dir), None);
    }

    #[test]
    fn returns_none_when_layout_file_unparseable() {
        let dir = std::env::temp_dir().join(format!("emma65-layout-test-corrupt-{:?}", std::thread::current().id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("layout.json"), "not valid json").unwrap();
        assert_eq!(load_dock_layout_from(&dir), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_and_load_round_trip_via_tempdir() {
        let dir = std::env::temp_dir().join(format!("emma65-layout-test-roundtrip-{:?}", std::thread::current().id()));
        let layout = serde_json::json!({"grid": {"root": {"type": "leaf"}}});
        save_dock_layout_to(&dir, &layout).unwrap();
        let loaded = load_dock_layout_from(&dir);
        assert_eq!(loaded, Some(layout));
        let _ = fs::remove_dir_all(&dir);
    }
}
