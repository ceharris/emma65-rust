//! Terminal panel: console byte-stream bridge.

use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, State};
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;

use crate::MAIN_WINDOW_LABEL;

/// Holds the tx end of the remote pipe so `write_terminal` can send bytes to the console.
///
/// `None` before the first session load completes, and briefly `None` again
/// mid-reload while the previous session's transport is being torn down.
pub struct TerminalTx(pub Mutex<Option<File>>);

/// One-shot sender signaling that the terminal panel is ready to receive output.
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
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "terminal-output", bytes);
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Ok(Err(_)) => break,
            Err(_would_block) => continue,
        }
    }
}

/// Tauri command: called by the terminal panel once its event listener is registered.
#[tauri::command]
pub fn terminal_ready(state: State<TerminalReadyTx>) {
    if let Some(tx) = state.0.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

/// Tauri command: send bytes typed in the terminal to the emulated console.
#[tauri::command]
pub fn write_terminal(bytes: Vec<u8>, state: State<TerminalTx>) -> Result<(), String> {
    let mut guard = state.0.lock().unwrap();
    let tx = guard.as_mut().ok_or("Terminal not ready")?;
    tx.write_all(&bytes).map_err(|e| e.to_string())
}

