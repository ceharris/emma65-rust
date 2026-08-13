//! Help > About dialog (issue #423): a small static-content panel plus,
//! in production builds only, a build-info line carrying the git commit hash
//! and build timestamp captured by `build.rs`.

use crate::menu;
use tauri::{AppHandle, Emitter};

/// Data backing the About dialog, returned by `get_about_info`.
#[derive(serde::Serialize)]
pub struct AboutInfo {
    /// URL of the project's GitHub repository.
    pub repo_url: String,
    /// Build-info line (git commit hash and UTC build timestamp), present
    /// only in production (non-`debug_assertions`) builds.
    pub build_info: Option<String>,
}

/// Returns the About dialog's dynamic content: the repo URL and the
/// build-info line, since the app name/description/copyright/license are
/// fixed text owned by the frontend component itself.
#[tauri::command]
pub fn get_about_info() -> AboutInfo {
    AboutInfo { repo_url: menu::GITHUB_REPO_URL.to_string(), build_info: build_info() }
}

#[cfg(debug_assertions)]
fn build_info() -> Option<String> {
    None
}

#[cfg(not(debug_assertions))]
fn build_info() -> Option<String> {
    Some(format!("Build {} ({})", env!("EMMA65_BUILD_GIT_HASH"), env!("EMMA65_BUILD_DATE")))
}

/// Emits the event that opens the About dialog, mirroring
/// `profile::emit_open_new_profile_dialog`.
pub(crate) fn emit_open_about_dialog(app: &AppHandle) {
    let _ = app.emit("open-about-dialog", ());
}
