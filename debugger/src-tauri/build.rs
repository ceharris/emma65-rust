fn main() {
    tauri_build::build();
    emit_build_info();
}

/// Captures the short git commit hash and a UTC build timestamp as
/// `EMMA65_BUILD_GIT_HASH`/`EMMA65_BUILD_DATE` env vars, read back via `env!`
/// in `about.rs` to populate the About dialog's build-info line (issue #423,
/// production builds only). Falls back to `"unknown"` for the hash if `git`
/// isn't available or the source tree isn't a git checkout (e.g. a source
/// tarball build), rather than failing the build.
fn emit_build_info() {
    let git_hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|hash| hash.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=EMMA65_BUILD_GIT_HASH={git_hash}");

    let build_date = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
    println!("cargo:rustc-env=EMMA65_BUILD_DATE={build_date}");
}
