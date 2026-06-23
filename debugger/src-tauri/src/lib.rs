use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;

use figment::{Figment, providers::{Format, Toml, Env}};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::io::unix::AsyncFd;

use emma65::emulator::{
    Config, DeviceRegistry, EmulatorSession, InstantiationContext, PipeTransport, TransportSlot,
    run as run_cpu, StepResult,
};

const TERMINAL_WINDOW_LABEL: &str = "terminal";

/// Holds the tx end of the remote pipe so `write_terminal` can send bytes to the console.
pub struct TerminalTx(pub Mutex<File>);

/// Holds the emulator session once it has been successfully constructed.
pub struct SessionState(pub Mutex<Option<EmulatorSession>>);

/// Payload emitted to the frontend on the `session-status` event.
#[derive(Clone, serde::Serialize)]
pub struct SessionStatus {
    /// Human-readable status message.
    pub message: String,
    /// True if the session was constructed successfully.
    pub ok: bool,
}

/// Holds the last emitted session status so late-connecting frontends can retrieve it.
pub struct SessionStatusState(pub Mutex<Option<SessionStatus>>);

/// Loads emulator config from `~/.emma/debugger/default/emulator.toml`,
/// builds the session with an injected pipe transport for the console,
/// and returns the session along with the remote end of the pipe.
async fn load_session() -> Result<(EmulatorSession, PipeTransport), String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable is not set".to_string())?;
    let config_path = std::path::Path::new(&home).join(".emma/debugger/default/emulator.toml");

    let config: Config = Figment::new()
        .merge(Toml::file(&config_path))
        .merge(Env::prefixed("EMMA65_").map(|k| k.as_str().replace('_', "-").into()))
        .extract()
        .map_err(|e| format!("Configuration error: {e}"))?;

    let (local, remote) = PipeTransport::pair()
        .map_err(|e| format!("Failed to create console transport: {e}"))?;

    let transport_slot: TransportSlot = std::sync::Arc::new(Mutex::new(Some(Box::new(local))));
    let context = InstantiationContext {
        clock_hz: config.clock_speed_hz,
        error_sender: None,
        console_transport: Some(transport_slot),
    };

    let registry = DeviceRegistry::with_builtins();
    let session = config.build_with_context(&registry, context).await
        .map_err(|e| format!("Failed to build emulator session: {e}"))?;

    Ok((session, remote))
}

/// Tokio task that reads bytes from the remote pipe rx and emits `terminal-output` events.
async fn run_terminal_bridge(rx: File, app: AppHandle) {
    let async_rx = match AsyncFd::new(rx) {
        Ok(fd) => fd,
        Err(_) => return,
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
            Ok(Err(_)) => break,
            Err(_would_block) => continue,
        }
    }
}

/// Tauri command: send bytes typed in the terminal to the emulated console.
#[tauri::command]
fn write_terminal(bytes: Vec<u8>, state: State<TerminalTx>) -> Result<(), String> {
    let mut tx = state.0.lock().unwrap();
    tx.write_all(&bytes).map_err(|e| e.to_string())
}

/// Returns the current session status, or `None` if not yet determined.
#[tauri::command]
fn get_session_status(state: State<SessionStatusState>) -> Option<SessionStatus> {
    state.0.lock().unwrap().clone()
}

fn emit_status(app: &AppHandle, status: SessionStatus) {
    app.state::<SessionStatusState>().0.lock().unwrap().replace(status.clone());
    let _ = app.emit("session-status", status);
}

fn open_terminal_window(app: &AppHandle) -> Result<(), String> {
    WebviewWindowBuilder::new(
        app,
        TERMINAL_WINDOW_LABEL,
        WebviewUrl::App("index.html?window=terminal".into()),
    )
    .title("emma65 Terminal")
    .inner_size(640.0, 400.0)
    .resizable(true)
    .build()
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(SessionState(Mutex::new(None)))
        .manage(SessionStatusState(Mutex::new(None)))
        // TerminalTx is registered after setup; commands are only called after the
        // terminal window is open, so it will always be present by then.
        .invoke_handler(tauri::generate_handler![get_session_status, write_terminal])
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match load_session().await {
                    Ok((session, remote)) => {
                        let (remote_rx, remote_tx) = remote.into_split();

                        // Register the tx side so write_terminal can use it.
                        handle.manage(TerminalTx(Mutex::new(remote_tx)));

                        // Store the session.
                        handle.state::<SessionState>().0.lock().unwrap().replace(session);

                        emit_status(&handle, SessionStatus {
                            message: "Emulator session ready".to_string(),
                            ok: true,
                        });

                        // Open the terminal window.
                        if let Err(e) = open_terminal_window(&handle) {
                            eprintln!("Failed to open terminal window: {e}");
                            return;
                        }

                        // Start the terminal bridge.
                        let bridge_handle = handle.clone();
                        tauri::async_runtime::spawn(async move {
                            run_terminal_bridge(remote_rx, bridge_handle).await;
                        });

                        // Start the CPU on a dedicated thread and watch for STP.
                        let cpu = handle.state::<SessionState>()
                            .0.lock().unwrap()
                            .take()
                            .expect("session was just stored")
                            .cpu;
                        let run_handle = run_cpu(cpu);
                        let exit_handle = handle.clone();
                        tauri::async_runtime::spawn(async move {
                            let result = run_handle.wait().await;
                            if matches!(result, StepResult::Stopped) {
                                exit_handle.exit(0);
                            }
                        });
                    }
                    Err(message) => {
                        emit_status(&handle, SessionStatus { message, ok: false });
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
