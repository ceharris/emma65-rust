//! Log panel: in-memory ring buffer of structured `LogRecord`s pushed live to the frontend as
//! they're produced, plus the command backing history hydration.
//!
//! Named `logging`, not `log` — `log` is already a direct crate dependency, and a module
//! literally named `log` would shadow it. This is a different module from the library's own
//! `emma65::emulator::logging` (private there, re-exported as `emma65::emulator::{LogRecord,
//! LogSender, spawn_log_collector}`) — same name, different crate, no conflict.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use tauri::{AppHandle, Emitter, Manager, State};

use emma65::emulator::LogRecord;

use crate::MAIN_WINDOW_LABEL;

/// Cap on the in-memory ring buffer held in [`LogState`]; the frontend mirrors the same cap on
/// its own buffer of live-streamed records.
const LOG_BUFFER_CAPACITY: usize = 5000;

/// Bounded channel capacity passed to `spawn_log_collector`, same drop-and-count-on-full
/// semantics as any other `LogSender` consumer.
pub const LOG_CHANNEL_CAPACITY: usize = 1024;

/// A `LogRecord`, adapted for serialization to the frontend: timestamp as epoch milliseconds,
/// level/category as their `Display` text.
#[derive(Clone, serde::Serialize)]
pub struct LogRecordDto {
    /// Epoch milliseconds the record was created at.
    pub timestamp_ms: u128,
    /// CPU cycle count accumulated at the time the record was created.
    pub cycles: u64,
    /// Severity of the message ("INFO" | "WARN" | "ERROR").
    pub level: String,
    /// Broad source of the message ("CPU" | "Device" | "Transport").
    pub category: String,
    /// Free-text message.
    pub message: String,
}

impl From<LogRecord> for LogRecordDto {
    /// Converts `timestamp` to epoch milliseconds (defaulting to 0 if it predates the epoch)
    /// and `level`/`category` via their existing `Display` impls.
    fn from(record: LogRecord) -> Self {
        let timestamp_ms = record.timestamp.duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        LogRecordDto {
            timestamp_ms,
            cycles: record.cycles,
            level: record.level.to_string(),
            category: record.category.to_string(),
            message: record.message,
        }
    }
}

/// Tauri-managed state: the in-memory ring buffer of log records shown in the Log window.
pub struct LogState(pub Mutex<VecDeque<LogRecordDto>>);

/// Converts `record` to a DTO, appends it to `LogState` (dropping the oldest entry first if
/// already at [`LOG_BUFFER_CAPACITY`]), then emits it to the main window (since #383, the Log
/// panel lives there, not in its own window) as a `log-record` event.
///
/// Called from the `spawn_log_collector` callback in `lib.rs`, so it runs on the collector's
/// background thread — `AppHandle::state()`/`emit_to` are thread-safe, same as
/// `terminal::run_terminal_bridge`'s `app.emit_to` call from its own task.
///
/// Generic over `R: tauri::Runtime` (rather than the concrete `AppHandle` alias) solely so the
/// test below can exercise it against `tauri::test::MockRuntime`; every production call site
/// still passes a plain `&AppHandle` (= `AppHandle<Wry>`), inferred automatically.
pub fn push_record<R: tauri::Runtime>(app: &AppHandle<R>, record: LogRecord) {
    let dto = LogRecordDto::from(record);

    let state = app.state::<LogState>();
    let mut buffer = state.0.lock().unwrap();
    if buffer.len() >= LOG_BUFFER_CAPACITY {
        buffer.pop_front();
    }
    buffer.push_back(dto.clone());
    drop(buffer);

    let _ = app.emit_to(MAIN_WINDOW_LABEL, "log-record", dto);
}

/// Returns the full current ring buffer, for the Log panel to hydrate on mount.
#[tauri::command]
pub fn get_log_records(state: State<LogState>) -> Vec<LogRecordDto> {
    state.0.lock().unwrap().iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime};

    use emma65::emulator::{LogCategory, LogLevel};
    use tauri::test::{mock_builder, mock_context, noop_assets};
    use tauri::{Listener, WebviewWindowBuilder};

    /// Builds a mocked app + real main window, calls `push_record` from a genuine background
    /// thread (mirroring `spawn_log_collector`'s callback), then asserts both that the ring
    /// buffer state was updated and that the `log-record` event was actually emitted and
    /// received by a listener on the window — since #383, the Log panel lives in the main
    /// window rather than its own, so this targets `MAIN_WINDOW_LABEL`.
    #[test]
    fn push_record_from_background_thread_updates_state_and_emits_event() {
        let app = mock_builder()
            .manage(LogState(Mutex::new(VecDeque::new())))
            .build(mock_context(noop_assets()))
            .expect("failed to build mock app");

        let window = WebviewWindowBuilder::new(&app, MAIN_WINDOW_LABEL, Default::default())
            .build()
            .expect("failed to build main window");

        let (tx, rx) = mpsc::channel();
        window.listen("log-record", move |event| {
            let _ = tx.send(event.payload().to_string());
        });

        let app_handle = app.handle().clone();
        std::thread::spawn(move || {
            push_record(&app_handle, LogRecord {
                timestamp: SystemTime::now(),
                cycles: 42,
                level: LogLevel::Info,
                category: LogCategory::Cpu,
                message: "background thread test message".into(),
            });
        })
        .join()
        .expect("push_record thread panicked");

        let state = app.state::<LogState>();
        assert_eq!(state.0.lock().unwrap().len(), 1, "push_record did not append to LogState");

        let payload = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("log-record event was never received by the window listener");
        assert!(payload.contains("background thread test message"));
    }
}
