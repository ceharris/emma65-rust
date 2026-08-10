//! Configuration profile selection: the `--profile` CLI flag, resolving a
//! profile name to its directory under `~/.emma/debugger/`, seeding a
//! newly-created profile from the default profile's files, and setting each
//! window's title to reflect the active profile.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use clap::Parser;
use tauri::{Manager, WebviewWindow, Wry};

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
/// (theme, watchpoints) can resolve the right directory at call time.
pub struct ProfileDirState(pub Mutex<PathBuf>);

/// Returns `~/.emma/debugger/<name>`, the directory holding one profile's
/// `emulator.toml`, `ui.toml`, and `watchpoints.emw`.
pub fn profile_dir(name: &str) -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable is not set".to_string())?;
    Ok(Path::new(&home).join(".emma/debugger").join(name))
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

/// Resolves the directory for profile `name`, creating it — seeded from the
/// default profile's files — if it doesn't exist yet. The default profile
/// itself is never auto-seeded, since there's nowhere to seed it from.
pub fn ensure_profile_dir(name: &str) -> Result<PathBuf, String> {
    let dir = profile_dir(name)?;
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create profile directory {}: {e}", dir.display()))?;
        if name != "default" {
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
    const WINDOWS: [(&str, &str); 3] =
        [("main", "Emma65 Debugger"), (crate::terminal::TERMINAL_WINDOW_LABEL, "Emma65 Terminal"), (crate::trace::TRACE_WINDOW_LABEL, "Emma65 Trace")];
    for (label, base) in WINDOWS {
        if let Some(window) = app.get_webview_window(label) {
            let _ = set_window_title(&window, base, profile);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that mutate the process-global `HOME` env var, since
    /// `cargo test` runs tests in parallel threads within one process.
    static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

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
        let default_dir = home.join(".emma/debugger/default");
        fs::create_dir_all(default_dir.join("subdir")).unwrap();
        fs::write(default_dir.join("emulator.toml"), "a").unwrap();
        fs::write(default_dir.join("subdir/nope.txt"), "b").unwrap();

        // SAFETY: HOME_ENV_LOCK excludes every other test in this module.
        unsafe { std::env::set_var("HOME", &home) };
        let target = home.join(".emma/debugger/custom");
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
        let default_dir = home.join(".emma/debugger/default");
        fs::create_dir_all(&default_dir).unwrap();
        fs::write(default_dir.join("ui.toml"), "default-contents").unwrap();

        // SAFETY: HOME_ENV_LOCK excludes every other test in this module.
        unsafe { std::env::set_var("HOME", &home) };
        let target = home.join(".emma/debugger/custom");
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
        let default_dir = home.join(".emma/debugger/default");
        fs::create_dir_all(&default_dir).unwrap();
        fs::write(default_dir.join("emulator.toml"), "config").unwrap();

        // SAFETY: HOME_ENV_LOCK excludes every other test in this module.
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
        let default_dir = home.join(".emma/debugger/default");
        fs::create_dir_all(&default_dir).unwrap();
        fs::write(default_dir.join("emulator.toml"), "default-config").unwrap();
        let custom_dir = home.join(".emma/debugger/custom");
        fs::create_dir_all(&custom_dir).unwrap();

        // SAFETY: HOME_ENV_LOCK excludes every other test in this module.
        unsafe { std::env::set_var("HOME", &home) };
        let dir = ensure_profile_dir("custom").unwrap();

        assert!(!dir.join("emulator.toml").exists());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_profile_dir_creates_default_without_seeding() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let home = temp_home("ensure-default-profile");
        // SAFETY: HOME_ENV_LOCK excludes every other test in this module.
        unsafe { std::env::set_var("HOME", &home) };

        let dir = ensure_profile_dir("default").unwrap();

        assert!(dir.exists());
        let _ = fs::remove_dir_all(&home);
    }
}
