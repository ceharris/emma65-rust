//! Log window: in-memory ring buffer of structured `LogRecord`s pushed live to the frontend as
//! they're produced, plus the commands backing history hydration and window visibility.
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

/// Window label of the auxiliary log window, as declared in `tauri.conf.json`.
pub const LOG_WINDOW_LABEL: &str = "log";

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
    /// Broad source of the message ("CPU" | "DEVICE" | "TRANSPORT").
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
/// already at [`LOG_BUFFER_CAPACITY`]), then emits it to the log window as a `log-record` event.
///
/// Called from the `spawn_log_collector` callback in `lib.rs`, so it runs on the collector's
/// background thread — `AppHandle::state()`/`emit_to` are thread-safe, same as
/// `terminal::run_terminal_bridge`'s `app.emit_to` call from its own task.
pub fn push_record(app: &AppHandle, record: LogRecord) {
    let dto = LogRecordDto::from(record);

    let state = app.state::<LogState>();
    let mut buffer = state.0.lock().unwrap();
    if buffer.len() >= LOG_BUFFER_CAPACITY {
        buffer.pop_front();
    }
    buffer.push_back(dto.clone());
    drop(buffer);

    let _ = app.emit_to(LOG_WINDOW_LABEL, "log-record", dto);
}

/// Returns the full current ring buffer, for the Log window to hydrate on mount/reopen.
#[tauri::command]
pub fn get_log_records(state: State<LogState>) -> Vec<LogRecordDto> {
    state.0.lock().unwrap().iter().cloned().collect()
}

/// Toggles the log window's visibility. Bound to Ctrl+Shift+L (see `useAppKeyBindings.ts`),
/// mirroring `trace::toggle_trace_visibility`. Delegates to the shared helper in `menu.rs` so
/// the Window-menu checkbox stays in sync regardless of which path toggled the window.
#[tauri::command]
pub fn toggle_log_visibility(app: AppHandle, window_menu: State<crate::menu::WindowMenuState>) -> Result<(), String> {
    crate::menu::toggle_window_visibility(&app, LOG_WINDOW_LABEL, &window_menu.log_item)
}
