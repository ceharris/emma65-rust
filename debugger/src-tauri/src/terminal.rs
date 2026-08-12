//! Terminal panel: console byte-stream bridge.

use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::unix::AsyncFd;

use crate::MAIN_WINDOW_LABEL;

/// Holds the tx end of the remote pipe so `write_terminal` can send bytes to the console.
///
/// `None` before the first session load completes, and briefly `None` again
/// mid-reload while the previous session's transport is being torn down.
pub struct TerminalTx(pub Mutex<Option<File>>);

/// Buffers console output emitted before the terminal panel's listener attaches.
///
/// The terminal panel lives inside the main window's dockview instance, which
/// `App.tsx` doesn't render until the emulator session itself is ready — so,
/// unlike the old standalone Terminal window (which mounted independently of
/// session status), session bring-up can no longer block on a `terminal_ready`
/// handshake before starting the CPU: that would deadlock, since the panel
/// that sends `terminal_ready` can't mount until the session it's waiting on
/// is already loaded. Instead, `run_terminal_bridge` buffers bytes here until
/// `terminal_ready` fires once (ever, for the life of the process — the panel
/// stays mounted afterward per dockview's confirmed non-lazy-mount behavior,
/// see issue #379), at which point the buffer is flushed and every later byte
/// is emitted directly.
#[derive(Default)]
pub struct TerminalOutputState {
    ready: bool,
    buffered: Vec<u8>,
}

/// Tauri-managed [`TerminalOutputState`].
pub struct TerminalOutputBuffer(pub Mutex<TerminalOutputState>);

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
                let bytes = &buf[..n];
                let state = app.state::<TerminalOutputBuffer>();
                let mut guard = state.0.lock().unwrap();
                if guard.ready {
                    drop(guard);
                    let _ = app.emit_to(MAIN_WINDOW_LABEL, "terminal-output", bytes.to_vec());
                } else {
                    guard.buffered.extend_from_slice(bytes);
                }
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Ok(Err(_)) => break,
            Err(_would_block) => continue,
        }
    }
}

/// Tauri command: called by the terminal panel once its event listener is registered.
/// Flushes any output buffered before this first call; a no-op on any later call.
#[tauri::command]
pub fn terminal_ready(app: AppHandle, state: State<TerminalOutputBuffer>) {
    let mut guard = state.0.lock().unwrap();
    if guard.ready {
        return;
    }
    guard.ready = true;
    let buffered = std::mem::take(&mut guard.buffered);
    drop(guard);
    if !buffered.is_empty() {
        let _ = app.emit_to(MAIN_WINDOW_LABEL, "terminal-output", buffered);
    }
}

/// Tauri command: send bytes typed in the terminal to the emulated console.
#[tauri::command]
pub fn write_terminal(bytes: Vec<u8>, state: State<TerminalTx>) -> Result<(), String> {
    let mut guard = state.0.lock().unwrap();
    let tx = guard.as_mut().ok_or("Terminal not ready")?;
    tx.write_all(&bytes).map_err(|e| e.to_string())
}

