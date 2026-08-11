//! Configuration profile selection: the `--profile` CLI flag, resolving a
//! profile name to its directory under `~/.emma/debugger/profiles/`, seeding
//! a newly-created profile from the default profile's files, and setting
//! each window's title to reflect the active profile.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use clap::Parser;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow, Wry};
use tauri_plugin_dialog::DialogExt;

/// CLI arguments accepted by the `emma65-debugger` binary.
#[derive(Parser)]
#[clap(name = "emma65-debugger")]
pub struct CliArgs {
    /// Name of the configuration profile to use.
    #[clap(long = "profile", default_value = "default")]
    pub profile: String,
}

/// Tauri-managed state holding the directory of the currently active
/// configuration profile, so commands that read or write profile files
/// (watchpoints) can resolve the right directory at call time. UI
/// preferences are not profile-scoped — see [`config_dir`].
pub struct ProfileDirState(pub Mutex<PathBuf>);

/// Returns `~/.emma/debugger`, the root under which both `profiles/` and
/// `config/` live.
fn debugger_home_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable is not set".to_string())?;
    Ok(Path::new(&home).join(".emma/debugger"))
}

/// Returns `~/.emma/debugger/profiles`, the directory under which every
/// named profile lives. Also the default location shown by the Open Profile
/// picker. Kept separate from [`config_dir`] so browsing for a profile never
/// surfaces non-profile entries like `config/`.
pub(crate) fn debugger_root_dir() -> Result<PathBuf, String> {
    Ok(debugger_home_dir()?.join("profiles"))
}

/// Returns `~/.emma/debugger/config`, the directory holding preferences that
/// apply regardless of which profile is active — currently just `ui.toml`.
pub fn config_dir() -> Result<PathBuf, String> {
    Ok(debugger_home_dir()?.join("config"))
}

/// Returns `~/.emma/debugger/profiles/<name>`, the directory holding one
/// profile's `emulator.toml` and `watchpoints.emw`.
pub fn profile_dir(name: &str) -> Result<PathBuf, String> {
    Ok(debugger_root_dir()?.join(name))
}

/// Copies every *file* found directly in the default profile's directory
/// into `dir`, skipping any file that already exists in `dir`. Subdirectories
/// of the default profile are never copied.
///
/// Shared by every path that can point the debugger at a not-yet-fully
/// populated profile: a brand new profile created via `--profile` here, and
/// (in later stories) New Profile, Open Profile, and Open Recent.
pub fn copy_missing_files_from_default(dir: &Path) -> Result<(), String> {
    let default_dir = profile_dir("default")?;
    if !default_dir.exists() || default_dir == dir {
        return Ok(());
    }
    for entry in fs::read_dir(&default_dir).map_err(|e| format!("Failed to read {}: {e}", default_dir.display()))? {
        let entry = entry.map_err(|e| format!("Failed to read {}: {e}", default_dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let dest = dir.join(entry.file_name());
        if dest.exists() {
            continue;
        }
        fs::copy(&path, &dest)
            .map_err(|e| format!("Failed to copy {} to {}: {e}", path.display(), dest.display()))?;
    }
    Ok(())
}

/// Resolves the directory for profile `name`, creating it if it doesn't
/// exist yet. A new `default` profile is seeded from the bundled default
/// configuration (ROM image, VICE labels, and `emulator.toml`); any other
/// new profile is seeded from the `default` profile's files.
pub fn ensure_profile_dir(name: &str) -> Result<PathBuf, String> {
    let dir = profile_dir(name)?;
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create profile directory {}: {e}", dir.display()))?;
        if name == "default" {
            emma65::emulator::config::default::materialize_default_config(&dir)
                .map_err(|e| format!("Failed to seed default profile: {e}"))?;
        } else {
            copy_missing_files_from_default(&dir)?;
        }
    }
    Ok(dir)
}

/// Sets `window`'s title to `"{base} — {profile}"` (em dash separator), e.g.
/// `Emma65 Debugger — default`.
///
/// On Linux, immediately follows with a `set_resizable(false)`/`set_resizable(true)`
/// toggle — GTK/Wayland client-side decorations otherwise don't repaint the
/// titlebar text for a window that's already mapped (same decoration-redraw
/// quirk as tauri-apps/tauri#11856 / tauri-apps/tao#1046, worked around
/// elsewhere in this crate for the hidden→shown terminal/trace windows via
/// their `Focused` handler; this window is visible from startup, so there's
/// no focus event to hook and the toggle must happen right here instead).
pub fn set_window_title(window: &WebviewWindow, base: &str, profile: &str) -> Result<(), String> {
    window.set_title(&format!("{base} — {profile}")).map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    {
        let _ = window.set_resizable(false);
        let _ = window.set_resizable(true);
    }
    Ok(())
}

/// Sets the title of every debugger window (main, terminal, trace) to
/// reflect `profile`. Windows not yet created (e.g. during tests) are
/// silently skipped.
pub fn set_all_window_titles(app: &impl Manager<Wry>, profile: &str) {
    const WINDOWS: [(&str, &str); 3] = [
        (crate::MAIN_WINDOW_LABEL, "Emma65 Debugger"),
        (crate::terminal::TERMINAL_WINDOW_LABEL, "Emma65 Terminal"),
        (crate::trace::TRACE_WINDOW_LABEL, "Emma65 Trace"),
    ];
    for (label, base) in WINDOWS {
        if let Some(window) = app.get_webview_window(label) {
            let _ = set_window_title(&window, base, profile);
        }
    }
}

/// Rejects an empty name, `.`/`..`, or a name containing a path separator or
/// NUL byte — the last three would otherwise let `profile_dir` resolve
/// outside `~/.emma/debugger/profiles/`.
fn validate_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Profile name cannot be empty".to_string());
    }
    if name == "." || name == ".." {
        return Err("Profile name cannot be \".\" or \"..\"".to_string());
    }
    if name.contains(['/', '\\', '\0']) {
        return Err("Profile name cannot contain path separators".to_string());
    }
    Ok(())
}

/// Emits `open-new-profile-dialog`, telling the main window's React layer to
/// open the New Profile modal. Called directly from the File > New Profile
/// menu item's `on_menu_event` handler; the Ctrl+N shortcut is scoped to the
/// main window and handled there in `NewProfileDialog.tsx` without going
/// through Rust at all.
pub(crate) fn emit_open_new_profile_dialog(app: &AppHandle) {
    let _ = app.emit("open-new-profile-dialog", ());
}

/// Validates `name`, creates a new profile directory seeded from the default
/// profile, and reloads the session against it — the same effect as
/// launching with `--profile <name>` for a not-yet-existing profile, but
/// without restarting the app.
///
/// Rejects a name that already names an existing profile: New Profile is
/// expected to create one, not silently switch to an existing one (that's
/// what Open Profile / Open Recent are for).
#[tauri::command]
pub async fn create_profile(app: AppHandle, name: String) -> Result<(), String> {
    let name = name.trim();
    validate_profile_name(name)?;
    let dir = profile_dir(name)?;
    if dir.exists() {
        return Err(format!("A profile named \"{name}\" already exists"));
    }
    let dir = ensure_profile_dir(name)?;
    crate::load_or_reload_session(&app, &dir).await;
    Ok(())
}

/// Awaits the result of a native folder picker, defaulting to
/// `~/.emma/debugger/profiles`, bridging its callback-based API to
/// async/await via a oneshot channel (same pattern as `stop_active_run` in
/// `lib.rs`). Returns `None` if the user cancels, or if the picked location
/// can't be resolved to a local path.
async fn pick_profile_directory(app: &AppHandle) -> Option<PathBuf> {
    let default_dir = debugger_root_dir().ok()?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_title("Open Profile").set_directory(default_dir).pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten()?.into_path().ok()
}

/// Shows a native folder picker, defaulting to `~/.emma/debugger/profiles`;
/// if the user selects a directory, fills in any files missing relative to the
/// default profile (existing files in the selected directory are left
/// untouched) and reloads the session against it. A no-op if the user
/// cancels the picker.
///
/// Unlike `create_profile`, the selected directory is expected to already
/// exist and may already hold some or all of a profile's files — this is how
/// the user points the debugger at a profile outside the New Profile flow.
#[tauri::command]
pub async fn open_profile(app: AppHandle) -> Result<(), String> {
    let Some(dir) = pick_profile_directory(&app).await else {
        return Ok(());
    };
    copy_missing_files_from_default(&dir)?;
    crate::load_or_reload_session(&app, &dir).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::HOME_ENV_LOCK;

    fn temp_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emma65-profile-test-{name}-{:?}", std::thread::current().id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copy_missing_files_from_default_copies_only_files_not_subdirectories() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("copy-basic");
        let default_dir = home.join(".emma/debugger/profiles/default");
        fs::create_dir_all(default_dir.join("subdir")).unwrap();
        fs::write(default_dir.join("emulator.toml"), "a").unwrap();
        fs::write(default_dir.join("subdir/nope.txt"), "b").unwrap();

        // SAFETY: HOME_ENV_LOCK excludes every other test using it, across modules.
        unsafe { std::env::set_var("HOME", &home) };
        let target = home.join(".emma/debugger/profiles/custom");
        fs::create_dir_all(&target).unwrap();

        copy_missing_files_from_default(&target).unwrap();

        assert!(target.join("emulator.toml").exists());
        assert!(!target.join("subdir").exists());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn copy_missing_files_from_default_does_not_overwrite_existing_files() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("copy-no-overwrite");
        let default_dir = home.join(".emma/debugger/profiles/default");
        fs::create_dir_all(&default_dir).unwrap();
        fs::write(default_dir.join("ui.toml"), "default-contents").unwrap();

        // SAFETY: HOME_ENV_LOCK excludes every other test using it, across modules.
        unsafe { std::env::set_var("HOME", &home) };
        let target = home.join(".emma/debugger/profiles/custom");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("ui.toml"), "existing-contents").unwrap();

        copy_missing_files_from_default(&target).unwrap();

        assert_eq!(fs::read_to_string(target.join("ui.toml")).unwrap(), "existing-contents");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_profile_dir_creates_and_seeds_a_new_named_profile() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("ensure-new-profile");
        let default_dir = home.join(".emma/debugger/profiles/default");
        fs::create_dir_all(&default_dir).unwrap();
        fs::write(default_dir.join("emulator.toml"), "config").unwrap();

        // SAFETY: HOME_ENV_LOCK excludes every other test using it, across modules.
        unsafe { std::env::set_var("HOME", &home) };
        let dir = ensure_profile_dir("custom").unwrap();

        assert!(dir.exists());
        assert_eq!(fs::read_to_string(dir.join("emulator.toml")).unwrap(), "config");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_profile_dir_does_not_reseed_an_existing_profile() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("ensure-existing-profile");
        let default_dir = home.join(".emma/debugger/profiles/default");
        fs::create_dir_all(&default_dir).unwrap();
        fs::write(default_dir.join("emulator.toml"), "default-config").unwrap();
        let custom_dir = home.join(".emma/debugger/profiles/custom");
        fs::create_dir_all(&custom_dir).unwrap();

        // SAFETY: HOME_ENV_LOCK excludes every other test using it, across modules.
        unsafe { std::env::set_var("HOME", &home) };
        let dir = ensure_profile_dir("custom").unwrap();

        assert!(!dir.join("emulator.toml").exists());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_profile_dir_seeds_a_new_default_profile_from_the_bundled_default() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("ensure-default-profile");
        // SAFETY: HOME_ENV_LOCK excludes every other test using it, across modules.
        unsafe { std::env::set_var("HOME", &home) };

        let dir = ensure_profile_dir("default").unwrap();

        assert!(dir.join("emulator.toml").exists());
        assert!(dir.join("program.bin").exists());
        assert!(dir.join("program.lbl").exists());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_profile_dir_composes_named_profile_seeding_with_a_freshly_seeded_default() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("ensure-default-then-named");
        // SAFETY: HOME_ENV_LOCK excludes every other test using it, across modules.
        unsafe { std::env::set_var("HOME", &home) };

        ensure_profile_dir("default").unwrap();
        let dir = ensure_profile_dir("custom").unwrap();

        assert!(dir.join("emulator.toml").exists());
        assert!(dir.join("program.bin").exists());
        assert!(dir.join("program.lbl").exists());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn validate_profile_name_accepts_legal_names() {
        assert!(validate_profile_name("default").is_ok());
        assert!(validate_profile_name("my-profile_2").is_ok());
    }

    #[test]
    fn validate_profile_name_rejects_empty_name() {
        assert!(validate_profile_name("").is_err());
    }

    #[test]
    fn validate_profile_name_rejects_dot_and_dot_dot() {
        assert!(validate_profile_name(".").is_err());
        assert!(validate_profile_name("..").is_err());
    }

    #[test]
    fn validate_profile_name_rejects_path_separators_and_nul() {
        assert!(validate_profile_name("a/b").is_err());
        assert!(validate_profile_name("a\\b").is_err());
        assert!(validate_profile_name("a\0b").is_err());
    }
}
