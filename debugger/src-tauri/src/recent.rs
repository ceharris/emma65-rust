//! Recently-used profile list backing the File > Open Recent submenu:
//! `~/.emma/debugger/config/recent.toml`, promoted on every profile
//! activation (initial `--profile` launch, New Profile, Open Profile, Open
//! Recent itself), and the display-path formatting used in the submenu.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};

use crate::menu;
use crate::profile;

/// Maximum number of entries retained in the recent-profiles list.
const MAX_RECENT: usize = 10;

/// On-disk shape of `recent.toml`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct RecentConfig {
    /// Absolute profile directory paths, most-recently-used first.
    #[serde(default)]
    paths: Vec<PathBuf>,
}

/// Managed state holding the in-memory MRU list of profile directories,
/// mirrored to `recent.toml` on every change.
pub struct RecentProfilesState(pub Mutex<Vec<PathBuf>>);

/// Reads `recent.toml` from `dir`, falling back to an empty list if missing or invalid.
pub fn load_recent_from(dir: &Path) -> Vec<PathBuf> {
    fs::read_to_string(dir.join("recent.toml"))
        .ok()
        .and_then(|contents| toml::from_str::<RecentConfig>(&contents).ok())
        .map(|config| config.paths)
        .unwrap_or_default()
}

/// Writes `paths` to `recent.toml` under `dir`, creating the directory if it doesn't exist.
fn save_recent_to(dir: &Path, paths: &[PathBuf]) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("Failed to create config directory: {e}"))?;
    let config = RecentConfig { paths: paths.to_vec() };
    let contents = toml::to_string(&config).map_err(|e| format!("Failed to serialize recent profiles: {e}"))?;
    fs::write(dir.join("recent.toml"), contents).map_err(|e| format!("Failed to write recent profiles: {e}"))
}

/// Formats `path` for display in the Open Recent submenu: profiles under
/// `~/.emma/debugger/profiles/` show as their bare name, other profiles
/// under the user's home directory show `~/`-relative, and anything else
/// shows the fully-qualified path. Purely a display transform — the stored
/// list always keeps absolute paths.
fn display_label(path: &Path) -> String {
    if let Ok(root) = profile::debugger_root_dir()
        && let Ok(rest) = path.strip_prefix(&root)
    {
        return rest.display().to_string();
    }
    if let Ok(home) = std::env::var("HOME")
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// Promotes `profile_dir` to the front of the recent-profiles list —
/// deduplicating if already present — truncates to [`MAX_RECENT`], persists
/// to `recent.toml`, and rebuilds the File > Open Recent submenu to match.
///
/// Called from `load_or_reload_session`, the one function behind every path
/// that can activate a profile, so usage is recorded consistently without a
/// separate call site per entry point. Called regardless of whether the
/// session itself loads successfully — the user did select it.
pub(crate) fn record_recent_profile(app: &AppHandle, profile_dir: &Path) {
    let paths = {
        let state = app.state::<RecentProfilesState>();
        let mut guard = state.0.lock().unwrap();
        guard.retain(|p| p != profile_dir);
        guard.insert(0, profile_dir.to_path_buf());
        guard.truncate(MAX_RECENT);
        guard.clone()
    };

    if let Ok(config_dir) = profile::config_dir()
        && let Err(e) = save_recent_to(&config_dir, &paths)
    {
        eprintln!("Failed to save recent profiles: {e}");
    }

    // The stored list stays MRU-ordered, dedup/truncate above relies on
    // that, and it still includes `profile_dir` (the now-active profile) so
    // its place in the ordering is preserved for when it's no longer
    // active. The submenu itself omits `profile_dir` — no point offering to
    // switch to the profile that's already active — and reads better
    // sorted by its visible label rather than by recency.
    let mut entries: Vec<(String, PathBuf)> =
        paths.iter().filter(|p| p.as_path() != profile_dir).map(|p| (display_label(p), p.clone())).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    rebuild_submenu(app, &entries, !paths.is_empty());
}

/// Empties the recent-profiles list, persists the empty list to
/// `recent.toml`, and disables the File > Open Recent submenu (which drops
/// the now-pointless "Clear Recent…" item along with any entries).
///
/// Invoked from the frontend confirmation dialog opened by
/// `emit_open_clear_recent_dialog`, once the user clicks "Clear".
#[tauri::command]
pub async fn clear_recent_profiles(app: AppHandle) -> Result<(), String> {
    {
        let state = app.state::<RecentProfilesState>();
        state.0.lock().unwrap().clear();
    }
    save_recent_to(&profile::config_dir()?, &[])?;
    rebuild_submenu(&app, &[], false);
    Ok(())
}

/// Rebuilds the File > Open Recent submenu from already-computed `entries`
/// and `has_recent`, logging (rather than propagating) any menu-API failure
/// — shared by every path that changes the recent-profiles list.
fn rebuild_submenu(app: &AppHandle, entries: &[(String, PathBuf)], has_recent: bool) {
    let menu_state = app.state::<menu::RecentMenuState>();
    if let Err(e) = menu::rebuild_open_recent_submenu(app, &menu_state, entries, has_recent) {
        eprintln!("Failed to rebuild Open Recent submenu: {e}");
    }
}

/// Emits `open-clear-recent-dialog`, telling the main window's React layer
/// to open the Clear Recent confirmation modal. Called directly from the
/// File > Open Recent > Clear Recent… menu item's `on_menu_event` handler,
/// mirroring `profile::emit_open_new_profile_dialog`.
pub(crate) fn emit_open_clear_recent_dialog(app: &AppHandle) {
    let _ = app.emit("open-clear-recent-dialog", ());
}

/// Activates the profile at `path`, selected from the File > Open Recent
/// submenu: (re)creates the directory if it's been removed since it was
/// recorded, fills in any files missing relative to the default profile,
/// and reloads the session against it.
///
/// Unlike `profile::open_profile`, the target isn't guaranteed to still
/// exist — it came from a possibly-stale recent list rather than a picker
/// that only ever returns existing directories.
pub(crate) async fn open_recent_profile(app: AppHandle, path: PathBuf) {
    if let Err(e) = fs::create_dir_all(&path) {
        eprintln!("Failed to prepare profile directory {}: {e}", path.display());
        return;
    }
    if let Err(e) = profile::copy_missing_files_from_default(&path) {
        eprintln!("Failed to seed profile directory {}: {e}", path.display());
    }
    crate::load_or_reload_session(&app, &path).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that mutate the process-global `HOME` env var, since
    /// `cargo test` runs tests in parallel threads within one process.
    static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emma65-recent-test-{name}-{:?}", std::thread::current().id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_and_load_round_trip_via_tempdir() {
        let dir = std::env::temp_dir().join(format!("emma65-recent-io-test-{:?}", std::thread::current().id()));
        let paths = vec![PathBuf::from("/a/b"), PathBuf::from("/c/d")];
        save_recent_to(&dir, &paths).unwrap();
        let loaded = load_recent_from(&dir);
        assert_eq!(loaded, paths);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_recent_from_returns_empty_when_missing() {
        let dir = std::env::temp_dir().join(format!("emma65-recent-missing-test-{:?}", std::thread::current().id()));
        assert!(load_recent_from(&dir).is_empty());
    }

    #[test]
    fn display_label_shows_bare_name_for_profiles_under_root() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("display-root");
        // SAFETY: HOME_ENV_LOCK excludes every other test in this module.
        unsafe { std::env::set_var("HOME", &home) };

        let path = home.join(".emma/debugger/profiles/finch");
        assert_eq!(display_label(&path), "finch");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn display_label_shows_tilde_relative_for_other_home_paths() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("display-home");
        // SAFETY: HOME_ENV_LOCK excludes every other test in this module.
        unsafe { std::env::set_var("HOME", &home) };

        let path = home.join("projects/my-profile");
        assert_eq!(display_label(&path), "~/projects/my-profile");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn display_label_shows_full_path_outside_home() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("display-outside");
        // SAFETY: HOME_ENV_LOCK excludes every other test in this module.
        unsafe { std::env::set_var("HOME", &home) };

        let path = PathBuf::from("/srv/profiles/shared");
        assert_eq!(display_label(&path), "/srv/profiles/shared");
        let _ = fs::remove_dir_all(&home);
    }
}
