//! Keyboard input bridge: forwards bytes typed in the Display panel/window to a configured
//! `keyboard` device's injected transport slot. Unlike `terminal`/`display`, this module owns no
//! window of its own — keyboard input rides along on the Display panel/window that already
//! exists — so there's no dock/detach lifecycle here, just the send side of the pipe.

use std::fs::File;
use std::io::Write;
use std::sync::Mutex;

use tauri::State;

/// Holds the tx end of the remote pipe so `write_keyboard` can send bytes to the keyboard device.
///
/// `None` before the first session load completes, and briefly `None` again mid-reload while the
/// previous session's transport is being torn down. Populated on every successful session load
/// regardless of whether the active profile actually configures a `keyboard` device — mirroring
/// `terminal::TerminalTx` for `console`. If no `keyboard` device is configured, nothing ever
/// drains the pipe on the emulator side, so writes are silently absorbed by OS pipe buffering.
pub struct KeyboardTx(pub Mutex<Option<File>>);

/// Tauri command: send bytes typed in the Display panel to the emulated keyboard device.
#[tauri::command]
pub fn write_keyboard(bytes: Vec<u8>, state: State<KeyboardTx>) -> Result<(), String> {
    let mut guard = state.0.lock().unwrap();
    let tx = guard.as_mut().ok_or("Keyboard not ready")?;
    tx.write_all(&bytes).map_err(|e| e.to_string())
}
