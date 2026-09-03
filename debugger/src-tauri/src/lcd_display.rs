//! LCD display panel: bridges an `LcdDisplay` device's composited-frame push channel (see
//! `emma65::emulator::LcdDisplayFrame`/`LcdDisplayGeometry`, and the memory-mapped LCD display
//! device plan's design doc §7) to the frontend, and owns the dockable/detachable window
//! lifecycle for the panel -- following `display.rs`'s/`led_matrix.rs`'s established dock/detach
//! pattern function-for-function. Like `led_matrix.rs` and unlike `display.rs`, there's no
//! keyboard sub-range to bridge (an LCD display has no input capability) and no wall-clock rate
//! limiting in the bridge task, since a frame is only ever pushed on a register write that
//! changes what's rendered (design doc §7), not on a periodic vsync. Unlike `led_matrix.rs`,
//! there's only ever one frame (not one per matrix index), so the cache below holds a single
//! `Option` rather than a `HashMap`.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;

use emma65::emulator::{LcdDisplayFrame, LcdDisplayGeometry};

use crate::MAIN_WINDOW_LABEL;

/// Label of the detached-LCD-Display window, statically declared in `tauri.conf.json` — same
/// shown/hidden-not-built/destroyed rationale as `terminal::TERMINAL_DETACHED_WINDOW_LABEL`.
pub const LCD_DISPLAY_DETACHED_WINDOW_LABEL: &str = "lcd-display-detached";

/// Holds the label of whichever window should currently receive `lcd-display-frame` events —
/// either `MAIN_WINDOW_LABEL` (LCD Display docked) or `LCD_DISPLAY_DETACHED_WINDOW_LABEL` (LCD
/// Display detached). Mirrors `display::DisplayTargetWindow`/`led_matrix::LedMatrixTargetWindow`
/// exactly.
pub struct LcdDisplayTargetWindow(pub Mutex<String>);

/// An LCD display device's fixed character-grid geometry, in the shape the frontend needs to
/// size its canvas — see `emma65::emulator::LcdDisplayGeometry`, which this mirrors field-for-
/// field (this crate defines its own `#[derive(Serialize)]` payload types for Tauri commands
/// rather than deriving `Serialize` on library types directly, matching
/// `display::DisplayGeometryPayload`/`led_matrix::LedMatrixGeometryPayload`).
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct LcdDisplayGeometryPayload {
    pub columns: u8,
    pub rows: u8,
}

impl From<LcdDisplayGeometry> for LcdDisplayGeometryPayload {
    fn from(geometry: LcdDisplayGeometry) -> Self {
        Self { columns: geometry.columns, rows: geometry.rows }
    }
}

/// The active session's LCD display geometry, if a `display/lcd` device is configured for the
/// active profile — set once per session load (before the bridge task ever runs, since it's
/// known entirely from configuration attributes, design doc §7) and read by
/// `get_lcd_display_geometry` so the panel can size its canvas on mount, before any frame has
/// been composited.
#[derive(Default)]
pub struct LcdDisplayGeometryState(pub Mutex<Option<LcdDisplayGeometryPayload>>);

/// Tauri command: returns the active session's LCD display geometry, or `None` if no
/// `display/lcd` device is configured for the active profile.
#[tauri::command]
pub fn get_lcd_display_geometry(state: State<LcdDisplayGeometryState>) -> Option<LcdDisplayGeometryPayload> {
    *state.0.lock().unwrap()
}

/// A composited frame, in the shape the frontend blits directly via `putImageData` — mirrors
/// `emma65::emulator::LcdDisplayFrame` field-for-field, `pixels` base64-encoded for the same
/// reason as `display::DisplayFramePayload::pixels`/`led_matrix::LedMatrixFramePayload::pixels`
/// (avoids Tauri's slow default `Vec<u8>` JSON-array-of-numbers serialization).
#[derive(Clone, serde::Serialize)]
pub struct LcdDisplayFramePayload {
    pub pixels: String,
    pub columns: u8,
    pub rows: u8,
}

impl From<LcdDisplayFrame> for LcdDisplayFramePayload {
    fn from(frame: LcdDisplayFrame) -> Self {
        use base64::Engine;
        Self {
            pixels: base64::engine::general_purpose::STANDARD.encode(&frame.pixels),
            columns: frame.columns,
            rows: frame.rows,
        }
    }
}

/// Reads the current `lcd-display-frame` target window's label.
fn current_target(app: &AppHandle) -> String {
    app.state::<LcdDisplayTargetWindow>().0.lock().unwrap().clone()
}

/// The last composited frame delivered, if any. Since a frame is only ever pushed on an actual
/// render-affecting register write (design doc §7) rather than every vsync like `CharDisplay`, a
/// panel that starts listening *after* the last such write would otherwise see nothing until
/// some unrelated later write happens. Fixed the same two ways `led_matrix::LedMatrixFrameCache`
/// is: `LcdDisplayPanel.tsx` fetches this cache once on mount (via `get_lcd_display_frame`
/// below), covering the initial docked mount and the docked panel reappearing after a reattach;
/// `show_detached_lcd_display` below instead replays the cached frame as an ordinary
/// `lcd-display-frame` event straight to the detached window every time it becomes the target,
/// since that window is only ever shown/hidden after its first creation, never remounted.
#[derive(Default)]
pub struct LcdDisplayFrameCache(pub Mutex<Option<LcdDisplayFramePayload>>);

/// The currently cached frame, cloned out from behind the lock.
fn cached_frame(app: &AppHandle) -> Option<LcdDisplayFramePayload> {
    app.state::<LcdDisplayFrameCache>().0.lock().unwrap().clone()
}

/// Tauri command: the last delivered frame, for a freshly-mounted panel to paint immediately
/// instead of waiting for the next render-affecting write. `None` if nothing has been composited
/// yet this session.
#[tauri::command]
pub fn get_lcd_display_frame(app: AppHandle) -> Option<LcdDisplayFramePayload> {
    cached_frame(&app)
}

/// Tokio task that drains `rx`, caches each frame (see `LcdDisplayFrameCache`), and `emit_to`s an
/// `lcd-display-frame` event to the current target window for every frame received. Unlike
/// `display::run_display_bridge`, there's no wall-clock rate limiting here: a frame is only ever
/// pushed on a render-affecting register write (design doc §7), and this device's total render
/// cost (at most 80 glyph cells) is far smaller than `CharDisplay`'s default 1000-cell grid, so
/// there's no backlog risk worth throttling against. Ends (returns) once every `LcdDisplayFrame`
/// sender is dropped — the channel equivalent of the terminal bridge seeing EOF on its pipe (see
/// `LcdDisplay::shutdown`).
pub async fn run_lcd_display_bridge(mut rx: mpsc::Receiver<LcdDisplayFrame>, app: AppHandle) {
    while let Some(frame) = rx.recv().await {
        let target = current_target(&app);
        let payload: LcdDisplayFramePayload = frame.into();
        *app.state::<LcdDisplayFrameCache>().0.lock().unwrap() = Some(payload.clone());
        let _ = app.emit_to(target, "lcd-display-frame", payload);
    }
}

/// Shows the detached-LCD-Display window, focuses it, retargets the bridge so future frames go
/// there instead of the main window, and replays the cached frame to it directly (see
/// `LcdDisplayFrameCache`'s doc comment for why this window specifically needs that replay,
/// unlike the docked panel). Doesn't touch the main dock's "lcd-display" panel — mirrors
/// `led_matrix::show_detached_led_matrix` exactly, including the caller-removes-the-dock-panel
/// contract.
fn show_detached_lcd_display(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(LCD_DISPLAY_DETACHED_WINDOW_LABEL)
        .ok_or_else(|| "lcd-display-detached window not found".to_string())?;
    let geometry = app.state::<crate::preferences::UiConfigState>().0.lock().unwrap().lcd_display_window_geometry;
    if let Some(geometry) = geometry {
        crate::preferences::apply_window_geometry(&window, &geometry);
    }
    window.show().map_err(|e| e.to_string())?;
    let _ = window.set_focus();
    *app.state::<LcdDisplayTargetWindow>().0.lock().unwrap() = LCD_DISPLAY_DETACHED_WINDOW_LABEL.to_string();
    if let Some(payload) = cached_frame(app) {
        let _ = app.emit_to(LCD_DISPLAY_DETACHED_WINDOW_LABEL, "lcd-display-frame", payload);
    }
    Ok(())
}

/// Shows the detached-LCD-Display window, retargets the bridge, persists the
/// lcd-display-detached flag, and updates the Window > LCD Display menu label — shared by the
/// dock tab's Detach button (via `detach_lcd_display` below) and the Window > "Detach LCD
/// Display…" menu item. Mirrors `led_matrix::begin_led_matrix_detach`.
pub(crate) fn begin_lcd_display_detach(app: &AppHandle) -> Result<(), String> {
    show_detached_lcd_display(app)?;
    crate::layout::set_lcd_display_detached(app, true)?;
    crate::menu::set_lcd_display_menu_label(&app.state::<crate::menu::WindowMenuState>(), true);
    Ok(())
}

/// Tauri command: the dock tab's "Detach" action. Mirrors `led_matrix::detach_led_matrix`.
#[tauri::command]
pub fn detach_lcd_display(app: AppHandle) -> Result<(), String> {
    begin_lcd_display_detach(&app)
}

/// Hides the detached-LCD-Display window, focuses the main window, retargets the bridge back to
/// the main window, persists the lcd-display-detached flag as false, updates the Window > LCD
/// Display menu label, and tells the main layout to reinsert the LCD Display panel via
/// `lcd-display-reattached`. Mirrors `led_matrix::reattach_led_matrix`.
pub(crate) fn reattach_lcd_display(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LCD_DISPLAY_DETACHED_WINDOW_LABEL) {
        let state = app.state::<crate::preferences::UiConfigState>();
        if let Err(e) =
            crate::preferences::save_window_geometry(&window, &state, |c, g| c.lcd_display_window_geometry = Some(g))
        {
            eprintln!("Failed to save LCD display window geometry: {e}");
        }
        let _ = window.hide();
    }
    if let Some(main_window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = main_window.set_focus();
    }
    *app.state::<LcdDisplayTargetWindow>().0.lock().unwrap() = MAIN_WINDOW_LABEL.to_string();
    if let Err(e) = crate::layout::set_lcd_display_detached(app, false) {
        eprintln!("Failed to persist lcd-display-detached flag: {e}");
    }
    crate::menu::set_lcd_display_menu_label(&app.state::<crate::menu::WindowMenuState>(), false);
    let _ = app.emit_to(MAIN_WINDOW_LABEL, "lcd-display-reattached", ());
}

/// Tauri command: the detached window's own reattach action. Mirrors
/// `led_matrix::attach_led_matrix`.
#[tauri::command]
pub fn attach_lcd_display(app: AppHandle) {
    reattach_lcd_display(&app);
}

/// One-time setup for the detached-LCD-Display window, called from `setup()` regardless of
/// whether it's ever actually detached this run — mirrors `led_matrix::install_detached_window`
/// exactly, including the app-menu strip and the Wayland/GTK resizable-toggle workaround.
pub(crate) fn install_detached_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LCD_DISPLAY_DETACHED_WINDOW_LABEL) else { return };
    let _ = window.remove_menu();
    let window_for_events = window.clone();
    let app_for_events = app.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            reattach_lcd_display(&app_for_events);
        }
        #[cfg(target_os = "linux")]
        tauri::WindowEvent::Focused(true) => {
            let _ = window_for_events.set_resizable(false);
            let _ = window_for_events.set_resizable(true);
        }
        _ => {}
    });
}

/// Shows the detached-LCD-Display window at startup if the persisted layout says LCD Display was
/// detached when the app last exited. Mirrors `led_matrix::restore_detached_window_if_needed`.
pub(crate) fn restore_detached_window_if_needed(app: &AppHandle, was_detached: bool) {
    if !was_detached {
        return;
    }
    if let Err(e) = show_detached_lcd_display(app) {
        eprintln!("Failed to restore detached LCD display window: {e}");
        return;
    }
    crate::menu::set_lcd_display_menu_label(&app.state::<crate::menu::WindowMenuState>(), true);
}
