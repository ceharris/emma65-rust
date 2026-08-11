//! Phase 0 spike (issue #379): throwaway prototype code de-risking the
//! dockview-in-Tauri migration in `doc/debugger-single-window-layout-plan.md`
//! before the rest of the plan's phases are scoped in detail. Per the issue,
//! this module isn't held to the project's normal doc-comment/clippy/test
//! bar — expect it to be deleted once the write-up lands, not built upon.

use std::fs;
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::profile;

/// Window label for the throwaway dockview host window (spike questions 1-3, 6).
pub const DOCKVIEW_SPIKE_WINDOW_LABEL: &str = "dockview-spike";

/// Window label for the dynamically (re)built detach target (spike questions
/// 4-5) — fixed and reused across every detach cycle, prototyping Phase 6's
/// `TERMINAL_DETACHED_WINDOW_LABEL` design.
pub const STACK_DETACHED_WINDOW_LABEL: &str = "stack-detached";

/// Shared `emit_to` target for the spike ticker, flipped by detach/reattach —
/// prototypes Phase 6's `TerminalTargetWindow` mechanism (spike question 5).
pub struct SpikeTickTarget(pub Mutex<String>);

/// Shows or hides the (statically declared, initially hidden) dockview spike
/// window. No lifecycle wiring beyond this — unlike Terminal/Trace/Log, this
/// window is thrown away with the rest of the spike, so closing it via native
/// chrome just destroys it; reopen via the same shortcut.
#[tauri::command]
pub fn toggle_dockview_spike_window(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(DOCKVIEW_SPIKE_WINDOW_LABEL)
        .ok_or("dockview-spike window not found")?;
    if window.is_visible().map_err(|e| e.to_string())? {
        window.hide().map_err(|e| e.to_string())
    } else {
        window.show().map_err(|e| e.to_string())
    }
}

/// Builds the detached Stack window under a fixed, reused label — prototypes
/// Phase 6's `detach_terminal` design (spike question 4) against `StackPanel`
/// instead of Terminal, per the issue's suggestion (no event subscriptions to
/// complicate the prototype).
#[tauri::command]
pub fn detach_stack_panel(app: AppHandle, target: State<SpikeTickTarget>) -> Result<(), String> {
    if app.get_webview_window(STACK_DETACHED_WINDOW_LABEL).is_some() {
        return Err("stack-detached window already exists".to_string());
    }
    let window = WebviewWindowBuilder::new(&app, STACK_DETACHED_WINDOW_LABEL, WebviewUrl::App("stack-detached.html".into()))
        .title("Stack (detached spike)")
        .inner_size(300.0, 420.0)
        .build()
        .map_err(|e| e.to_string())?;
    // Same accelerator-collision workaround Phase 6 will need on every detach
    // cycle — see the long comment in lib.rs's setup() for why.
    let _ = window.remove_menu();
    *target.0.lock().unwrap() = STACK_DETACHED_WINDOW_LABEL.to_string();
    Ok(())
}

/// Destroys the detached Stack window and flips the ticker target back to the
/// dockview spike window — prototypes Phase 6's reattach path (spike
/// questions 4-5).
#[tauri::command]
pub fn reattach_stack_panel(app: AppHandle, target: State<SpikeTickTarget>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(STACK_DETACHED_WINDOW_LABEL) {
        window.destroy().map_err(|e| e.to_string())?;
    }
    *target.0.lock().unwrap() = DOCKVIEW_SPIKE_WINDOW_LABEL.to_string();
    Ok(())
}

/// Background ticker mirroring `run_terminal_bridge`'s structure: reads the
/// current `emit_to` target fresh on every tick rather than capturing it
/// once, so a detach/reattach mid-run retargets future ticks without
/// restarting the loop (spike question 5).
pub async fn run_spike_ticker(app: AppHandle) {
    let mut n: u64 = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        n += 1;
        let target = app.state::<SpikeTickTarget>().0.lock().unwrap().clone();
        let _ = app.emit_to(&target, "spike-tick", n);
    }
}

/// Reads the persisted spike dockview layout (spike question 6), returning
/// `None` if missing or unparseable so the frontend falls back to its
/// hardcoded default rather than crashing — prototypes Phase 3's
/// `get_dock_layout` fallback behavior.
#[tauri::command]
pub fn get_spike_layout() -> Option<serde_json::Value> {
    let dir = profile::config_dir().ok()?;
    let contents = fs::read_to_string(dir.join("dockview-spike-layout.json")).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Persists `layout` as opaque JSON — Rust never parses dockview's internal
/// schema, matching Phase 3's design.
#[tauri::command]
pub fn set_spike_layout(layout: serde_json::Value) -> Result<(), String> {
    let dir = profile::config_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let contents = serde_json::to_string(&layout).map_err(|e| e.to_string())?;
    fs::write(dir.join("dockview-spike-layout.json"), contents).map_err(|e| e.to_string())
}
