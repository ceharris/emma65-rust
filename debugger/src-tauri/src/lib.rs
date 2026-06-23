use std::sync::Mutex;
use figment::{Figment, providers::{Format, Toml, Env}};
use tauri::{AppHandle, Emitter, Manager};
use emma65::emulator::{Config, DeviceRegistry, EmulatorSession, InstantiationContext, PipeTransport, TransportSlot};

/// Holds the emulator session once it has been successfully constructed.
pub struct SessionState(pub Mutex<Option<EmulatorSession>>);

/// Holds the last emitted session status so late-connecting frontends can retrieve it.
pub struct SessionStatusState(pub Mutex<Option<SessionStatus>>);

/// Payload emitted to the frontend on the `session-status` event.
#[derive(Clone, serde::Serialize)]
pub struct SessionStatus {
    /// Human-readable status message.
    pub message: String,
    /// True if the session was constructed successfully.
    pub ok: bool,
}

/// Loads emulator config from `~/.emma/debugger/default/emulator.toml`,
/// builds the session with an injected pipe transport for the console,
/// and returns the session along with the local end of the pipe.
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

    let transport_slot: TransportSlot = std::sync::Arc::new(Mutex::new(Some(Box::new(remote))));
    let context = InstantiationContext {
        clock_hz: config.clock_speed_hz,
        error_sender: None,
        console_transport: Some(transport_slot),
    };

    let registry = DeviceRegistry::with_builtins();
    let session = config.build_with_context(&registry, context).await
        .map_err(|e| format!("Failed to build emulator session: {e}"))?;

    Ok((session, local))
}

/// Returns the current session status, or `None` if not yet determined.
#[tauri::command]
fn get_session_status(state: tauri::State<SessionStatusState>) -> Option<SessionStatus> {
    state.0.lock().unwrap().clone()
}

fn emit_status(app: &AppHandle, status: SessionStatus) {
    app.state::<SessionStatusState>().0.lock().unwrap().replace(status.clone());
    let _ = app.emit("session-status", status);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(SessionState(Mutex::new(None)))
        .manage(SessionStatusState(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![get_session_status])
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match load_session().await {
                    Ok((session, _local)) => {
                        handle.state::<SessionState>().0.lock().unwrap().replace(session);
                        emit_status(&handle, SessionStatus {
                            message: "Emulator session ready".to_string(),
                            ok: true,
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
