//! LED matrix panel: bridges an `LedMatrix` device's composited per-matrix-frame push channel
//! (see `emma65::emulator::LedMatrixFrame`/`LedMatrixGeometry`, and the memory-mapped LED matrix
//! device plan's design doc §10) to the frontend, and owns the dockable/detachable window
//! lifecycle for the panel -- following `terminal.rs`/`display.rs`'s established dock/detach
//! pattern function-for-function. Unlike `display.rs`, there's no keyboard sub-range to bridge
//! (LED matrices have no input capability), and frames carry a `matrix_index` rather than a
//! single whole-device buffer.

use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;

use emma65::emulator::{LedMatrixFrame, LedMatrixGeometry};

use crate::MAIN_WINDOW_LABEL;

/// Label of the detached-LED-Matrix window, statically declared in `tauri.conf.json` — same
/// shown/hidden-not-built/destroyed rationale as `terminal::TERMINAL_DETACHED_WINDOW_LABEL`.
pub const LED_MATRIX_DETACHED_WINDOW_LABEL: &str = "led-matrix-detached";

/// Holds the label of whichever window should currently receive `led-matrix-frame` events —
/// either `MAIN_WINDOW_LABEL` (LED Matrix docked) or `LED_MATRIX_DETACHED_WINDOW_LABEL` (LED
/// Matrix detached). Mirrors `display::DisplayTargetWindow` exactly.
pub struct LedMatrixTargetWindow(pub Mutex<String>);

/// An LED matrix device's fixed geometry, in the shape the frontend needs to size and lay out its
/// N canvases — see `emma65::emulator::LedMatrixGeometry`, which this mirrors field-for-field
/// (this crate defines its own `#[derive(Serialize)]` payload types for Tauri commands rather
/// than deriving `Serialize` on library types directly, matching `display::DisplayGeometryPayload`).
/// `columns` is the device's own configured arrangement (arrangement-coupling follow-up): the
/// panel always mirrors it rather than offering an independent arrangement menu.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct LedMatrixGeometryPayload {
    pub matrices: u32,
    pub columns: u32,
}

impl From<LedMatrixGeometry> for LedMatrixGeometryPayload {
    fn from(geometry: LedMatrixGeometry) -> Self {
        Self { matrices: geometry.matrices, columns: geometry.columns }
    }
}

/// The active session's LED matrix geometry, if an `display/matrix` device is configured for the
/// active profile — set once per session load (before the bridge task ever runs, since it's
/// known entirely from configuration attributes, design doc §10) and read by
/// `get_led_matrix_geometry` so the panel can size its canvases on mount, before any frame has
/// been composited.
#[derive(Default)]
pub struct LedMatrixGeometryState(pub Mutex<Option<LedMatrixGeometryPayload>>);

/// Tauri command: returns the active session's LED matrix geometry, or `None` if no
/// `display/matrix` device is configured for the active profile.
#[tauri::command]
pub fn get_led_matrix_geometry(state: State<LedMatrixGeometryState>) -> Option<LedMatrixGeometryPayload> {
    *state.0.lock().unwrap()
}

/// One matrix's composited frame, in the shape the frontend blits directly via `putImageData` —
/// mirrors `emma65::emulator::LedMatrixFrame` field-for-field, `pixels` base64-encoded for the
/// same reason as `display::DisplayFramePayload::pixels` (avoids Tauri's slow default `Vec<u8>`
/// JSON-array-of-numbers serialization).
#[derive(Clone, serde::Serialize)]
pub struct LedMatrixFramePayload {
    pub matrix_index: u8,
    pub pixels: String,
}

impl From<LedMatrixFrame> for LedMatrixFramePayload {
    fn from(frame: LedMatrixFrame) -> Self {
        use base64::Engine;
        Self {
            matrix_index: frame.matrix_index,
            pixels: base64::engine::general_purpose::STANDARD.encode(&frame.pixels),
        }
    }
}

/// Reads the current `led-matrix-frame` target window's label.
fn current_target(app: &AppHandle) -> String {
    app.state::<LedMatrixTargetWindow>().0.lock().unwrap().clone()
}

/// The last composited frame delivered for each matrix index, keyed by `matrix_index`. Since a
/// frame is only ever pushed on an actual swap (design doc §10) rather than every vsync like
/// `CharDisplay`, a panel that starts listening *after* a matrix's last swap would otherwise see
/// nothing for that matrix until some unrelated later write happens to touch it again. Fixed two
/// ways, for two different situations: `LedMatrixPanel.tsx` fetches this cache once on mount (via
/// `get_led_matrix_frames` below), which covers every case where the panel is a genuinely fresh
/// React mount — the initial docked mount, and the docked panel reappearing after a reattach
/// (`DockLayout.tsx` really does destroy and recreate that dock tab). The detached window is
/// *not* such a case: it's a statically-declared Tauri window created once and merely
/// shown/hidden thereafter (`install_detached_window`), so its `LedMatrixPanel` instance mounts
/// exactly once for the app's entire lifetime and never sees a fresh mount-time fetch again on a
/// second or third detach. `show_detached_led_matrix` below instead replays the cache as ordinary
/// `led-matrix-frame` events straight to that window every time it becomes the target, reusing
/// the panel's already-registered live listener rather than needing it to re-fetch anything.
#[derive(Default)]
pub struct LedMatrixFrameCache(pub Mutex<HashMap<u8, LedMatrixFramePayload>>);

/// Every matrix's currently cached frame, cloned out from behind the lock.
fn cached_frames(app: &AppHandle) -> Vec<LedMatrixFramePayload> {
    app.state::<LedMatrixFrameCache>().0.lock().unwrap().values().cloned().collect()
}

/// Tauri command: every matrix's last delivered frame, for a freshly-mounted panel to paint
/// immediately instead of waiting for the next swap. Order is unspecified — the frontend routes
/// each entry to its own canvas by `matrix_index`.
#[tauri::command]
pub fn get_led_matrix_frames(app: AppHandle) -> Vec<LedMatrixFramePayload> {
    cached_frames(&app)
}

/// Tokio task that drains `rx`, caches each frame (see `LedMatrixFrameCache`), and `emit_to`s a
/// `led-matrix-frame` event to the current target window for every frame received. Unlike
/// `display::run_display_bridge`, there's no wall-clock rate limiting here: a frame is only ever
/// pushed on an actual matrix swap (design doc §10), already far less frequent than
/// `CharDisplay`'s per-vsync whole-grid recomposite, and the device's own bounded channel
/// (`try_send`, never blocking `tick()`) already caps how much a misbehaving CPU loop issuing
/// `CMD_SWAP` repeatedly can queue up. Ends (returns) once every `LedMatrixFrame` sender is
/// dropped — the channel equivalent of the terminal bridge seeing EOF on its pipe (see
/// `LedMatrix::shutdown`).
pub async fn run_led_matrix_bridge(mut rx: mpsc::Receiver<LedMatrixFrame>, app: AppHandle) {
    while let Some(frame) = rx.recv().await {
        let target = current_target(&app);
        let payload: LedMatrixFramePayload = frame.into();
        app.state::<LedMatrixFrameCache>().0.lock().unwrap().insert(payload.matrix_index, payload.clone());
        let _ = app.emit_to(target, "led-matrix-frame", payload);
    }
}

/// Shows the detached-LED-Matrix window, focuses it, retargets the bridge so future frames go
/// there instead of the main window, and replays every already-cached frame to it directly (see
/// `LedMatrixFrameCache`'s doc comment for why this window specifically needs that replay, unlike
/// the docked panel). Doesn't touch the main dock's "led-matrix" panel — mirrors
/// `display::show_detached_display` exactly, including the caller-removes-the-dock-panel contract
/// (plus the cache replay, which `display.rs` has no equivalent of: `CharDisplay` self-heals on
/// its own next vsync push regardless of which window is listening).
fn show_detached_led_matrix(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(LED_MATRIX_DETACHED_WINDOW_LABEL)
        .ok_or_else(|| "led-matrix-detached window not found".to_string())?;
    let geometry = app.state::<crate::preferences::UiConfigState>().0.lock().unwrap().led_matrix_window_geometry;
    if let Some(geometry) = geometry {
        crate::preferences::apply_window_geometry(&window, &geometry);
    }
    window.show().map_err(|e| e.to_string())?;
    let _ = window.set_focus();
    *app.state::<LedMatrixTargetWindow>().0.lock().unwrap() = LED_MATRIX_DETACHED_WINDOW_LABEL.to_string();
    for payload in cached_frames(app) {
        let _ = app.emit_to(LED_MATRIX_DETACHED_WINDOW_LABEL, "led-matrix-frame", payload);
    }
    Ok(())
}

/// Shows the detached-LED-Matrix window, retargets the bridge, persists the
/// led-matrix-detached flag, and updates the Window > LED Matrix menu label — shared by the dock
/// tab's Detach button (via `detach_led_matrix` below) and the Window > "Detach LED Matrix…"
/// menu item. Mirrors `display::begin_display_detach`.
pub(crate) fn begin_led_matrix_detach(app: &AppHandle) -> Result<(), String> {
    show_detached_led_matrix(app)?;
    crate::layout::set_led_matrix_detached(app, true)?;
    crate::menu::set_led_matrix_menu_label(&app.state::<crate::menu::WindowMenuState>(), true);
    Ok(())
}

/// Tauri command: the dock tab's "Detach" action. Mirrors `display::detach_display`.
#[tauri::command]
pub fn detach_led_matrix(app: AppHandle) -> Result<(), String> {
    begin_led_matrix_detach(&app)
}

/// Hides the detached-LED-Matrix window, focuses the main window, retargets the bridge back to
/// the main window, persists the led-matrix-detached flag as false, updates the Window > LED
/// Matrix menu label, and tells the main layout to reinsert the LED Matrix panel via
/// `led-matrix-reattached`. Mirrors `display::reattach_display`.
pub(crate) fn reattach_led_matrix(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LED_MATRIX_DETACHED_WINDOW_LABEL) {
        let state = app.state::<crate::preferences::UiConfigState>();
        if let Err(e) =
            crate::preferences::save_window_geometry(&window, &state, |c, g| c.led_matrix_window_geometry = Some(g))
        {
            eprintln!("Failed to save LED matrix window geometry: {e}");
        }
        let _ = window.hide();
    }
    if let Some(main_window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = main_window.set_focus();
    }
    *app.state::<LedMatrixTargetWindow>().0.lock().unwrap() = MAIN_WINDOW_LABEL.to_string();
    if let Err(e) = crate::layout::set_led_matrix_detached(app, false) {
        eprintln!("Failed to persist led-matrix-detached flag: {e}");
    }
    crate::menu::set_led_matrix_menu_label(&app.state::<crate::menu::WindowMenuState>(), false);
    let _ = app.emit_to(MAIN_WINDOW_LABEL, "led-matrix-reattached", ());
}

/// Tauri command: the detached window's own reattach action. Mirrors `display::attach_display`.
#[tauri::command]
pub fn attach_led_matrix(app: AppHandle) {
    reattach_led_matrix(&app);
}

/// One-time setup for the detached-LED-Matrix window, called from `setup()` regardless of
/// whether it's ever actually detached this run — mirrors `display::install_detached_window`
/// exactly, including the app-menu strip and the Wayland/GTK resizable-toggle workaround.
pub(crate) fn install_detached_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LED_MATRIX_DETACHED_WINDOW_LABEL) else { return };
    let _ = window.remove_menu();
    let window_for_events = window.clone();
    let app_for_events = app.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            reattach_led_matrix(&app_for_events);
        }
        #[cfg(target_os = "linux")]
        tauri::WindowEvent::Focused(true) => {
            let _ = window_for_events.set_resizable(false);
            let _ = window_for_events.set_resizable(true);
        }
        _ => {}
    });
}

/// Shows the detached-LED-Matrix window at startup if the persisted layout says LED Matrix was
/// detached when the app last exited. Mirrors `display::restore_detached_window_if_needed`.
pub(crate) fn restore_detached_window_if_needed(app: &AppHandle, was_detached: bool) {
    if !was_detached {
        return;
    }
    if let Err(e) = show_detached_led_matrix(app) {
        eprintln!("Failed to restore detached LED matrix window: {e}");
        return;
    }
    crate::menu::set_led_matrix_menu_label(&app.state::<crate::menu::WindowMenuState>(), true);
}
