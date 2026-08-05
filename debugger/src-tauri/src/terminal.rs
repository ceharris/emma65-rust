//! Terminal window: console byte-stream bridge and window visibility.

use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;

/// Window label of the auxiliary terminal window, as declared in `tauri.conf.json`.
pub const TERMINAL_WINDOW_LABEL: &str = "terminal";

/// Holds the tx end of the remote pipe so `write_terminal` can send bytes to the console.
pub struct TerminalTx(pub Mutex<File>);

/// One-shot sender signaling that the terminal window is ready to receive output.
pub struct TerminalReadyTx(pub Mutex<Option<oneshot::Sender<()>>>);

/// Tokio task that reads bytes from the remote pipe rx and emits `terminal-output` events.
pub async fn run_terminal_bridge(rx: File, app: AppHandle) {
    let async_rx = match AsyncFd::new(rx) {
        Ok(fd) => fd,
        Err(e) => { eprintln!("terminal bridge: AsyncFd::new failed: {e}"); return; }
    };
    let mut buf = [0u8; 256];
    loop {
        let mut guard = match async_rx.readable().await {
            Ok(g) => g,
            Err(_) => break,
        };
        match guard.try_io(|fd| fd.get_ref().read(&mut buf)) {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                let bytes: Vec<u8> = buf[..n].to_vec();
                let _ = app.emit_to(TERMINAL_WINDOW_LABEL, "terminal-output", bytes);
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Ok(Err(_)) => break,
            Err(_would_block) => continue,
        }
    }
}

/// Tauri command: called by the terminal window once its event listener is registered.
#[tauri::command]
pub fn terminal_ready(state: State<TerminalReadyTx>) {
    if let Some(tx) = state.0.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

/// Tauri command: send bytes typed in the terminal to the emulated console.
#[tauri::command]
pub fn write_terminal(bytes: Vec<u8>, state: State<TerminalTx>) -> Result<(), String> {
    let mut tx = state.0.lock().unwrap();
    tx.write_all(&bytes).map_err(|e| e.to_string())
}

/// Toggles the terminal window's visibility. Bound to Ctrl+Shift+` in both the
/// main and terminal windows (see `useAppKeyBindings.ts`), so the frontend
/// doesn't need to track visibility state itself. Delegates to the shared
/// helper in `menu.rs` so the Window-menu checkbox stays in sync regardless
/// of which path (menu click, shortcut, or this command) toggled the window.
#[tauri::command]
pub fn toggle_terminal_visibility(app: AppHandle, window_menu: State<crate::menu::WindowMenuState>) -> Result<(), String> {
    crate::menu::toggle_window_visibility(&app, TERMINAL_WINDOW_LABEL, &window_menu.terminal_item)
}

/// Shows the terminal window (created hidden at startup, per `tauri.conf.json`).
///
/// On the webkit2gtk backend, a window's webview doesn't realize — and its JS
/// never runs — until the window is actually mapped, so this must happen
/// before awaiting the terminal's ready handshake. Callers should hide it
/// again afterward (see `hide_terminal_window`) so the window stays hidden
/// at launch as intended, until the user toggles it via `toggle_terminal_visibility`.
pub fn show_terminal_window(app: &AppHandle) -> Result<(), String> {
    app.get_webview_window(TERMINAL_WINDOW_LABEL)
        .ok_or_else(|| "terminal window not found".to_string())?
        .show()
        .map_err(|e| e.to_string())
}

/// Hides the terminal window.
///
/// Used to re-hide the window after the startup `show_terminal_window` call
/// that works around webkit2gtk's hidden-webview bug, restoring the intended
/// "hidden until the user toggles it" launch state.
pub fn hide_terminal_window(app: &AppHandle) -> Result<(), String> {
    app.get_webview_window(TERMINAL_WINDOW_LABEL)
        .ok_or_else(|| "terminal window not found".to_string())?
        .hide()
        .map_err(|e| e.to_string())
}
