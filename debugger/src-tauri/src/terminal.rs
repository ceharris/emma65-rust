//! Terminal panel: console byte-stream bridge.

use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::unix::AsyncFd;

use crate::MAIN_WINDOW_LABEL;

/// Label of the detached-Terminal window, statically declared (fixed, not
/// per-instance) in `tauri.conf.json` — issue #385's revised design shows
/// and hides this one pre-declared window rather than dynamically building
/// and destroying it (see this module's `install_detached_window` doc
/// comment for why).
pub const TERMINAL_DETACHED_WINDOW_LABEL: &str = "terminal-detached";

/// Holds the tx end of the remote pipe so `write_terminal` can send bytes to the console.
///
/// `None` before the first session load completes, and briefly `None` again
/// mid-reload while the previous session's transport is being torn down.
pub struct TerminalTx(pub Mutex<Option<File>>);

/// Holds the label of whichever window should currently receive
/// `terminal-output` events — either `MAIN_WINDOW_LABEL` (Terminal docked)
/// or `TERMINAL_DETACHED_WINDOW_LABEL` (Terminal detached). Read by
/// `run_terminal_bridge` before each `emit_to` call and flipped by
/// `begin_terminal_detach`/`reattach_terminal`.
pub struct TerminalTargetWindow(pub Mutex<String>);

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
                    let target = current_target(&app);
                    let _ = app.emit_to(target, "terminal-output", bytes.to_vec());
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

/// Reads the current `terminal-output` target window's label.
fn current_target(app: &AppHandle) -> String {
    app.state::<TerminalTargetWindow>().0.lock().unwrap().clone()
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
        let target = current_target(&app);
        let _ = app.emit_to(target, "terminal-output", buffered);
    }
}

/// Tauri command: send bytes typed in the terminal to the emulated console.
#[tauri::command]
pub fn write_terminal(bytes: Vec<u8>, state: State<TerminalTx>) -> Result<(), String> {
    let mut guard = state.0.lock().unwrap();
    let tx = guard.as_mut().ok_or("Terminal not ready")?;
    tx.write_all(&bytes).map_err(|e| e.to_string())
}

/// Shows the detached-Terminal window, focuses it, and retargets the console
/// bridge so future output/writes go there instead of the main window.
/// Doesn't touch the main dock's "terminal" panel — the caller is
/// responsible for removing it (the frontend, right after a successful
/// `detach_terminal` call) or for never having added it in the first place
/// (a session restore that finds the persisted terminal-detached flag
/// already set — see `crate::lib::run`'s `setup()`).
fn show_detached_terminal(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(TERMINAL_DETACHED_WINDOW_LABEL)
        .ok_or_else(|| "terminal-detached window not found".to_string())?;
    window.show().map_err(|e| e.to_string())?;
    let _ = window.set_focus();
    *app.state::<TerminalTargetWindow>().0.lock().unwrap() = TERMINAL_DETACHED_WINDOW_LABEL.to_string();
    Ok(())
}

/// Shows the detached-Terminal window, retargets the console bridge,
/// persists the terminal-detached flag, and updates the Window > Terminal
/// menu label — the part shared by both ways of starting a detach: the dock
/// tab's Detach button (via the `detach_terminal` command below, which the
/// frontend then follows with removing the "terminal" dock panel itself)
/// and the Window > "Detach Terminal…" menu item (whose `on_menu_event`
/// handler in `lib.rs` has no JS of its own to run that removal, so it
/// instead emits `terminal-detach-requested` for the main layout to do the
/// same thing).
pub(crate) fn begin_terminal_detach(app: &AppHandle) -> Result<(), String> {
    show_detached_terminal(app)?;
    crate::layout::set_terminal_detached(app, true)?;
    crate::menu::set_terminal_menu_label(&app.state::<crate::menu::WindowMenuState>(), true);
    Ok(())
}

/// Tauri command: the dock tab's "Detach" action. Shows the detached window
/// and retargets the console bridge (see `begin_terminal_detach`); the
/// frontend closes the "terminal" dock panel itself once this resolves,
/// deliberately after the new window is fully shown and the target has
/// already flipped (see this module's doc comment on the `emit_to`-retarget
/// race in issue #385's plan).
#[tauri::command]
pub fn detach_terminal(app: AppHandle) -> Result<(), String> {
    begin_terminal_detach(&app)
}

/// Hides the detached-Terminal window, retargets the console bridge back to
/// the main window, persists the terminal-detached flag as false, updates
/// the Window > Terminal menu label, and tells the main layout to reinsert
/// the Terminal panel via the `terminal-reattached` event. Shared by the
/// detached window's native close button (see `install_detached_window`),
/// the Window > "Attach Terminal" menu action (`lib.rs`'s `on_menu_event`),
/// and the `attach_terminal` command below.
pub(crate) fn reattach_terminal(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(TERMINAL_DETACHED_WINDOW_LABEL) {
        let _ = window.hide();
    }
    *app.state::<TerminalTargetWindow>().0.lock().unwrap() = MAIN_WINDOW_LABEL.to_string();
    if let Err(e) = crate::layout::set_terminal_detached(app, false) {
        eprintln!("Failed to persist terminal-detached flag: {e}");
    }
    crate::menu::set_terminal_menu_label(&app.state::<crate::menu::WindowMenuState>(), false);
    let _ = app.emit_to(MAIN_WINDOW_LABEL, "terminal-reattached", ());
}

/// Tauri command: the detached window's own Ctrl+Shift+T reattaches it — a
/// thin wrapper around `reattach_terminal`, the same fn the window's native
/// close button and the Window > "Attach Terminal" menu item use.
#[tauri::command]
pub fn attach_terminal(app: AppHandle) {
    reattach_terminal(&app);
}

/// One-time setup for the detached-Terminal window, called from `setup()`
/// regardless of whether the window is ever actually detached — unlike the
/// dynamic-build design Phase 0 (#379) rejected (it froze the whole app on
/// every build), this window is statically declared and only ever
/// shown/hidden, so there's no per-detach-cycle setup to repeat.
///
/// Strips the app menu Tauri would otherwise attach automatically
/// (`app.set_menu()` covers every window lacking its own explicit menu,
/// same accelerator-collision reasoning as the pre-#383 Terminal/Trace/Log
/// windows), and installs a close-hides-and-reattaches handler plus the
/// Wayland decoration-hit-test workaround for windows that get hidden/shown
/// repeatedly (see the "Wayland/GTK window quirks" project notes).
pub(crate) fn install_detached_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window(TERMINAL_DETACHED_WINDOW_LABEL) else { return };
    let _ = window.remove_menu();
    let window_for_events = window.clone();
    let app_for_events = app.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            reattach_terminal(&app_for_events);
        }
        #[cfg(target_os = "linux")]
        tauri::WindowEvent::Focused(true) => {
            let _ = window_for_events.set_resizable(false);
            let _ = window_for_events.set_resizable(true);
        }
        _ => {}
    });
}

/// Shows the detached-Terminal window at startup if the persisted layout
/// says Terminal was detached when the app last exited (issue #385's risk
/// #3) — otherwise a restart mid-detach would silently re-dock Terminal
/// instead of reopening its window. Doesn't touch the main dock's "terminal"
/// panel; `DockLayout.tsx`'s `restoreLayout` independently consults the same
/// flag (returned by `layout::get_dock_layout`) to skip adding it.
pub(crate) fn restore_detached_window_if_needed(app: &AppHandle, was_detached: bool) {
    if !was_detached {
        return;
    }
    if let Err(e) = show_detached_terminal(app) {
        eprintln!("Failed to restore detached terminal window: {e}");
        return;
    }
    crate::menu::set_terminal_menu_label(&app.state::<crate::menu::WindowMenuState>(), true);
}

