//! Display panel: bridges a `CharDisplay` device's composited-frame push channel (see
//! `emma65::emulator::DisplayFrame`/`DisplayGeometry`, and the memory-mapped display device
//! plan's design doc §9) to the frontend, and owns the dockable/detachable window lifecycle for
//! the panel — following `terminal.rs`'s established dock/detach pattern (design doc §10) as
//! closely as the two devices' different data shapes allow: Terminal streams bytes through an OS
//! pipe and an `AsyncFd` poll loop, while this device pushes pre-composited frames through an
//! in-process `mpsc` channel, so there's no OS-level byte stream to bridge, just a
//! channel-to-event forwarder.

use std::fs::File;
use std::io::Write as _;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;

use emma65::emulator::{DisplayFrame, DisplayGeometry};

use crate::MAIN_WINDOW_LABEL;

/// Holds the tx end of the remote pipe so `write_keyboard` can send bytes to the active
/// `display` device's keyboard sub-range.
///
/// `None` before the first session load completes, and briefly `None` again mid-reload while the
/// previous session's transport is being torn down. Populated on every successful session load
/// regardless of whether the active profile's `display` device actually configures
/// `keyboard-address=` — mirroring `terminal::TerminalTx` for `console`. If no keyboard range is
/// configured, `CharDisplay::tick` still drains the pipe unconditionally (the relay-drain
/// correctness fix from work units 2+3), so writes are silently absorbed there instead of by OS
/// pipe buffering.
pub struct KeyboardTx(pub Mutex<Option<File>>);

/// Tauri command: send bytes typed in the Display panel to the active `display` device's
/// keyboard sub-range.
#[tauri::command]
pub fn write_keyboard(bytes: Vec<u8>, state: State<KeyboardTx>) -> Result<(), String> {
    let mut guard = state.0.lock().unwrap();
    let tx = guard.as_mut().ok_or("Keyboard not ready")?;
    tx.write_all(&bytes).map_err(|e| e.to_string())
}

/// Label of the detached-Display window, statically declared in `tauri.conf.json` — same
/// shown/hidden-not-built/destroyed rationale as `terminal::TERMINAL_DETACHED_WINDOW_LABEL`
/// (issue #385).
pub const DISPLAY_DETACHED_WINDOW_LABEL: &str = "display-detached";

/// Fallback rate cap used when no display device is configured for the active profile (so
/// `DisplayGeometryState` holds `None`) — never actually exercised in that case, since no
/// frames arrive either, but keeps `run_display_bridge` well-defined without needing an
/// `Option` cadence in its hot loop.
const DEFAULT_MIN_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Holds the label of whichever window should currently receive `display-frame` events —
/// either `MAIN_WINDOW_LABEL` (Display docked) or `DISPLAY_DETACHED_WINDOW_LABEL` (Display
/// detached). Mirrors `terminal::TerminalTargetWindow` exactly.
pub struct DisplayTargetWindow(pub Mutex<String>);

/// A display device's fixed pixel/cell geometry, in the shape the frontend needs to size its
/// canvas — see `emma65::emulator::DisplayGeometry`, which this mirrors field-for-field (this
/// crate defines its own `#[derive(Serialize)]` payload types for Tauri commands rather than
/// deriving `Serialize` on library types directly, matching `cpu_bus::CpuBusSnapshot` and
/// `registers::RegisterSnapshot`).
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct DisplayGeometryPayload {
    pub columns: u32,
    pub rows: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub frame_rate_hz: u32,
}

impl From<DisplayGeometry> for DisplayGeometryPayload {
    fn from(geometry: DisplayGeometry) -> Self {
        Self {
            columns: geometry.columns,
            rows: geometry.rows,
            pixel_width: geometry.pixel_width,
            pixel_height: geometry.pixel_height,
            frame_rate_hz: geometry.frame_rate_hz,
        }
    }
}

/// The active session's display geometry, if a `display` device is configured for the
/// active profile — set once per session load (before the bridge task ever runs, since it's
/// known entirely from configuration attributes, design doc §9) and read by
/// `get_display_geometry` so the panel can size its canvas on mount, before any frame has been
/// composited.
#[derive(Default)]
pub struct DisplayGeometryState(pub Mutex<Option<DisplayGeometryPayload>>);

/// Tauri command: returns the active session's display geometry, or `None` if no display
/// device is configured for the active profile.
#[tauri::command]
pub fn get_display_geometry(state: State<DisplayGeometryState>) -> Option<DisplayGeometryPayload> {
    *state.0.lock().unwrap()
}

/// A composited frame, in the shape the frontend blits directly via `putImageData` — mirrors
/// `emma65::emulator::DisplayFrame` field-for-field, same reasoning as `DisplayGeometryPayload`,
/// except `pixels` is base64-encoded rather than carried as a raw `Vec<u8>`: Tauri's event
/// system serializes a `Vec<u8>` as a JSON array of individual numbers (e.g. ~256,000 of them for
/// a 320x200 frame), and parsing that on the frontend at up to `frame_rate_hz` per second is slow
/// enough that the panel visibly lags for the first several seconds until the JS engine's JIT
/// warms up on that code path. Base64 encode/decode are each a single native call on both ends
/// (Rust's `base64` crate, JS's `atob`) instead of ~256,000 parsed JSON tokens, so there's no
/// warm-up-dependent bottleneck — worth the ~33% larger wire payload.
#[derive(Clone, serde::Serialize)]
pub struct DisplayFramePayload {
    pub pixels: String,
    pub columns: u32,
    pub rows: u32,
}

impl From<DisplayFrame> for DisplayFramePayload {
    fn from(frame: DisplayFrame) -> Self {
        use base64::Engine;
        Self {
            pixels: base64::engine::general_purpose::STANDARD.encode(&frame.pixels),
            columns: frame.columns,
            rows: frame.rows,
        }
    }
}

/// Reads the current `display-frame` target window's label.
fn current_target(app: &AppHandle) -> String {
    app.state::<DisplayTargetWindow>().0.lock().unwrap().clone()
}

/// Tokio task that drains `rx` and `emit_to`s a `display-frame` event to the current target
/// window, wall-clock rate-limited to roughly `frame_rate_hz` per second (design doc §6): a
/// backstop independent of the device's own cycle-accurate vsync, needed whenever the CPU
/// produces frames faster than real time (unthrottled clock, or just a fast host relative to a
/// throttled one) — frames arriving before the interval has elapsed are dropped rather than
/// queued, so the panel always renders the most recent state rather than catching up on stale
/// ones. Ends (returns) once every `DisplayFrame` sender is dropped — the channel equivalent of
/// the terminal bridge seeing EOF on its pipe (see `CharDisplay::shutdown`).
pub async fn run_display_bridge(mut rx: mpsc::Receiver<DisplayFrame>, app: AppHandle, frame_rate_hz: Option<u32>) {
    let min_interval = frame_rate_hz
        .filter(|hz| *hz > 0)
        .map(|hz| Duration::from_secs_f64(1.0 / hz as f64))
        .unwrap_or(DEFAULT_MIN_FRAME_INTERVAL);
    let mut last_emit: Option<Instant> = None;
    while let Some(frame) = rx.recv().await {
        let now = Instant::now();
        if let Some(last) = last_emit
            && now.duration_since(last) < min_interval
        {
            continue;
        }
        last_emit = Some(now);
        let target = current_target(&app);
        let payload: DisplayFramePayload = frame.into();
        let _ = app.emit_to(target, "display-frame", payload);
    }
}

/// Shows the detached-Display window, focuses it, and retargets the bridge so future frames go
/// there instead of the main window. Doesn't touch the main dock's "display" panel — mirrors
/// `terminal::show_detached_terminal` exactly, including the caller-removes-the-dock-panel
/// contract.
fn show_detached_display(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(DISPLAY_DETACHED_WINDOW_LABEL)
        .ok_or_else(|| "display-detached window not found".to_string())?;
    let geometry = app.state::<crate::preferences::UiConfigState>().0.lock().unwrap().display_window_geometry;
    if let Some(geometry) = geometry {
        crate::preferences::apply_window_geometry(&window, &geometry);
    }
    window.show().map_err(|e| e.to_string())?;
    let _ = window.set_focus();
    *app.state::<DisplayTargetWindow>().0.lock().unwrap() = DISPLAY_DETACHED_WINDOW_LABEL.to_string();
    Ok(())
}

/// Shows the detached-Display window, retargets the bridge, persists the display-detached flag,
/// and updates the Window > Display menu label — shared by the dock tab's Detach button (via
/// `detach_display` below) and the Window > "Detach Display…" menu item. Mirrors
/// `terminal::begin_terminal_detach`.
pub(crate) fn begin_display_detach(app: &AppHandle) -> Result<(), String> {
    show_detached_display(app)?;
    crate::layout::set_display_detached(app, true)?;
    crate::menu::set_display_menu_label(&app.state::<crate::menu::WindowMenuState>(), true);
    Ok(())
}

/// Tauri command: the dock tab's "Detach" action. Mirrors `terminal::detach_terminal`.
#[tauri::command]
pub fn detach_display(app: AppHandle) -> Result<(), String> {
    begin_display_detach(&app)
}

/// Hides the detached-Display window, focuses the main window, retargets the bridge back to the
/// main window, persists the display-detached flag as false, updates the Window > Display menu
/// label, and tells the main layout to reinsert the Display panel via `display-reattached`.
/// Mirrors `terminal::reattach_terminal`.
pub(crate) fn reattach_display(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(DISPLAY_DETACHED_WINDOW_LABEL) {
        let state = app.state::<crate::preferences::UiConfigState>();
        if let Err(e) =
            crate::preferences::save_window_geometry(&window, &state, |c, g| c.display_window_geometry = Some(g))
        {
            eprintln!("Failed to save display window geometry: {e}");
        }
        let _ = window.hide();
    }
    if let Some(main_window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = main_window.set_focus();
    }
    *app.state::<DisplayTargetWindow>().0.lock().unwrap() = MAIN_WINDOW_LABEL.to_string();
    if let Err(e) = crate::layout::set_display_detached(app, false) {
        eprintln!("Failed to persist display-detached flag: {e}");
    }
    crate::menu::set_display_menu_label(&app.state::<crate::menu::WindowMenuState>(), false);
    let _ = app.emit_to(MAIN_WINDOW_LABEL, "display-reattached", ());
}

/// Tauri command: the detached window's own reattach action. Mirrors `terminal::attach_terminal`.
#[tauri::command]
pub fn attach_display(app: AppHandle) {
    reattach_display(&app);
}

/// One-time setup for the detached-Display window, called from `setup()` regardless of whether
/// it's ever actually detached this run — mirrors `terminal::install_detached_window` exactly,
/// including the app-menu strip and the Wayland/GTK resizable-toggle workaround.
pub(crate) fn install_detached_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(DISPLAY_DETACHED_WINDOW_LABEL) else { return };
    let _ = window.remove_menu();
    let window_for_events = window.clone();
    let app_for_events = app.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            reattach_display(&app_for_events);
        }
        #[cfg(target_os = "linux")]
        tauri::WindowEvent::Focused(true) => {
            let _ = window_for_events.set_resizable(false);
            let _ = window_for_events.set_resizable(true);
        }
        _ => {}
    });
}

/// Shows the detached-Display window at startup if the persisted layout says Display was
/// detached when the app last exited. Mirrors `terminal::restore_detached_window_if_needed`.
pub(crate) fn restore_detached_window_if_needed(app: &AppHandle, was_detached: bool) {
    if !was_detached {
        return;
    }
    if let Err(e) = show_detached_display(app) {
        eprintln!("Failed to restore detached display window: {e}");
        return;
    }
    crate::menu::set_display_menu_label(&app.state::<crate::menu::WindowMenuState>(), true);
}
