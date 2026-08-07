//! Trace recording/browsing, mirroring the Tauri debugger's `trace.rs`, but reached
//! through DAP's `evaluate` request rather than native Tauri commands or a genuinely
//! custom DAP request — `dap = "0.4.1-alpha1"`'s closed `Command`/`Event` enums rule
//! both of those out (see `session.rs`'s `TERMINAL_SOCKET_SPEC` doc comment and
//! `bus.rs`'s module doc comment for the full story-8/9 finding that established this
//! pattern). Unlike `bus.rs`'s one-line `describe()` summaries, the payloads here
//! (a windowed page of trace rows, a recording status) are structured, so
//! `expression`/`result` carry JSON instead of a fixed context string with no
//! arguments and a human-readable result.
//!
//! `record_trace` caches the CPU variant and a symbol table snapshot in [`TraceData`]
//! at record time, so `get_trace_window`/`get_trace_status` never need CPU access at
//! all — unlike every other command in this adapter, trace browsing works even while
//! a background run/step currently owns the CPU (`ExecState::with_cpu`/`with_cpu_mut`
//! would otherwise report "CPU not ready").

use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use dap::requests::EvaluateArguments;
use dap::responses::EvaluateResponse;
use dap::types::EvaluateArgumentsContext;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use emma65::disasm::{Disassembler, TraceBusOp, TraceRowAssembler};
use emma65::emulator::{
    BinaryTraceReader, BinaryTraceWriter, ChannelTraceCallback, CpuVariant, OverflowPolicy,
    SymbolTable, TraceCallback, TraceKind, TraceRecord, spawn_trace_writer,
};

use crate::exec::ExecState;

const RECORD_TRACE: &str = "emma65.recordTrace";
const STOP_TRACE: &str = "emma65.stopTrace";
const GET_TRACE_WINDOW: &str = "emma65.getTraceWindow";
const GET_TRACE_STATUS: &str = "emma65.getTraceStatus";

/// Capacity of the bounded channel between the CPU thread and the trace writer thread.
const TRACE_CHANNEL_CAPACITY: usize = 4096;

/// Recording state and bookkeeping for the trace webview, independent of [`ExecState`]
/// (mirrors the Tauri debugger's `TraceState` being managed separately from `CpuState`).
#[derive(Default)]
pub struct TraceData {
    /// Path of the trace file currently being recorded, or last recorded.
    path: Option<PathBuf>,
    /// Record ordinal where each row's `Registers` record begins, in row order.
    /// Shared with the [`RowIndexingTraceCallback`] installed on the CPU while
    /// recording, so it can grow independently of this struct's own lock.
    row_index: Arc<Mutex<Vec<u64>>>,
    /// True while a recording is in progress.
    recording: bool,
    /// Join handle for the writer thread, awaited by `stop_trace` so the file
    /// is guaranteed flushed before the command returns.
    writer_handle: Option<JoinHandle<()>>,
    /// CPU variant snapshotted at `record_trace` time, so `get_trace_window` can
    /// disassemble without CPU access.
    variant: Option<CpuVariant>,
    /// Symbol table snapshotted at `record_trace` time, for the same reason.
    symbols: Option<SymbolTable>,
}

/// Wraps a [`ChannelTraceCallback`], counting records and indexing the start
/// of each row (its `Registers` record) into a shared `row_index`, the same
/// way `DisassemblingTraceCallback` wraps a callback in the library.
struct RowIndexingTraceCallback {
    inner: ChannelTraceCallback,
    row_index: Arc<Mutex<Vec<u64>>>,
    record_count: u64,
}

impl TraceCallback for RowIndexingTraceCallback {
    fn record(&mut self, rec: TraceRecord) {
        if matches!(rec.kind, TraceKind::Registers(_)) {
            self.row_index.lock().unwrap().push(self.record_count);
        }
        self.inner.record(rec);
        self.record_count += 1;
    }

    fn flush(&mut self) {
        self.inner.flush();
    }
}

/// One bus operation in a [`TraceRowDto`]'s detail pane.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceBusOpDto {
    addr: u16,
    op: String,
    value: u8,
    comment: String,
}

/// One reconstructed trace row returned to the webview.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceRowDto {
    /// 1-based sequence number (`instr_id + 1`), matching the `emma65-tracer` CLI's numbering.
    seq: u64,
    addr: u16,
    /// Raw bytes as hex strings, e.g. ["4C", "00", "06"].
    bytes: Vec<String>,
    /// Symbol names from the symbol table associated with `addr`, without trailing colons.
    labels: Vec<String>,
    mnemonic: String,
    operand: String,
    comment: String,
    /// False for invalid opcodes under the active variant.
    is_valid: bool,
    cycles: Option<u8>,
    /// Registers as they stood immediately before this instruction executed.
    a: u8,
    x: u8,
    y: u8,
    s: u8,
    p: u8,
    pc: u16,
    /// Bus reads and writes observed while this instruction executed, in stream order.
    bus_ops: Vec<TraceBusOpDto>,
}

/// A windowed page of trace rows, returned for `emma65.getTraceWindow`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceWindowPage {
    rows: Vec<TraceRowDto>,
    /// Total rows recorded so far, letting the webview compute the tail window as
    /// `start_row = total_rows.saturating_sub(viewport_rows)` on each poll.
    total_rows: usize,
}

/// Recording/browsing status, returned for `emma65.recordTrace`/`stopTrace`/`getTraceStatus`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceStatus {
    recording: bool,
    path: Option<String>,
}

/// `emma65.recordTrace`'s `expression` payload.
#[derive(Deserialize)]
struct RecordTraceArgs {
    path: String,
}

/// `emma65.getTraceWindow`'s `expression` payload.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetTraceWindowArgs {
    start_row: usize,
    count: usize,
}

/// Dispatches a recognized `emma65.*` trace `evaluate` context to its operation.
/// Returns `None` if `args.context` isn't one of the constants above, so the caller
/// (chained after `bus::handle_evaluate`) can respond "unsupported request" the same
/// way it does for any other unrecognized command.
pub fn handle_evaluate(
    state: &ExecState,
    trace_state: &Mutex<TraceData>,
    args: &EvaluateArguments,
) -> Option<Result<EvaluateResponse, String>> {
    let context = match &args.context {
        Some(EvaluateArgumentsContext::String(context)) => context.as_str(),
        _ => return None,
    };
    let result = match context {
        RECORD_TRACE => parse_args::<RecordTraceArgs>(args)
            .and_then(|a| record_trace(state, trace_state, a.path))
            .and_then(|status| json_response(&status)),
        STOP_TRACE => stop_trace(state, trace_state).and_then(|status| json_response(&status)),
        GET_TRACE_WINDOW => parse_args::<GetTraceWindowArgs>(args)
            .and_then(|a| get_trace_window(trace_state, a.start_row, a.count))
            .and_then(|page| json_response(&page)),
        GET_TRACE_STATUS => json_response(&get_trace_status(trace_state)),
        _ => return None,
    };
    Some(result)
}

fn parse_args<T: DeserializeOwned>(args: &EvaluateArguments) -> Result<T, String> {
    serde_json::from_str(&args.expression).map_err(|e| format!("invalid arguments: {e}"))
}

fn json_response<T: Serialize>(value: &T) -> Result<EvaluateResponse, String> {
    let result = serde_json::to_string(value).map_err(|e| e.to_string())?;
    Ok(EvaluateResponse { result, ..Default::default() })
}

/// Starts recording an execution trace to `path`, spooling records to a dedicated
/// writer thread. Only callable while the CPU is halted (mirrors the Tauri debugger's
/// `record_trace` command).
fn record_trace(state: &ExecState, trace_state: &Mutex<TraceData>, path: String) -> Result<TraceStatus, String> {
    state
        .with_cpu_mut(|cpu| {
            let file = File::create(&path).map_err(|e| e.to_string())?;
            let writer = BinaryTraceWriter::new(file, cpu.variant());
            // BlockOnFull, unlike the CLI capture use case's DropOnFull: a debugger
            // trace that silently drops instructions would be misleading.
            let (callback, writer_handle, _dropped) =
                spawn_trace_writer(writer, TRACE_CHANNEL_CAPACITY, OverflowPolicy::BlockOnFull);

            let row_index = Arc::new(Mutex::new(Vec::new()));
            let indexing_callback =
                RowIndexingTraceCallback { inner: callback, row_index: Arc::clone(&row_index), record_count: 0 };
            cpu.set_trace_callback(Some(Box::new(indexing_callback)));

            let mut data = trace_state.lock().unwrap();
            data.path = Some(PathBuf::from(&path));
            data.row_index = row_index;
            data.recording = true;
            data.writer_handle = Some(writer_handle);
            data.variant = Some(cpu.variant());
            data.symbols = Some(cpu.bus().symbol_table().clone());
            Ok(TraceStatus { recording: true, path: Some(path) })
        })
        .ok_or_else(|| "CPU not ready".to_string())?
}

/// Stops the current recording: detaches the trace callback and waits for the writer
/// thread to flush and exit. The recorded file stays browsable via `get_trace_window`;
/// the next `record_trace` call requires a new path. Mirrors the Tauri debugger's
/// `stop_trace` command.
fn stop_trace(state: &ExecState, trace_state: &Mutex<TraceData>) -> Result<TraceStatus, String> {
    state
        .with_cpu_mut(|cpu| cpu.set_trace_callback(None))
        .ok_or_else(|| "CPU not ready".to_string())?;

    let mut data = trace_state.lock().unwrap();
    if let Some(handle) = data.writer_handle.take() {
        let _ = handle.join();
    }
    data.recording = false;
    Ok(TraceStatus { recording: false, path: data.path.as_ref().map(|p| p.display().to_string()) })
}

/// Returns up to `count` reconstructed trace rows starting at `start_row` (clamped
/// into the recorded range), plus the total row count recorded so far. Mirrors the
/// Tauri debugger's `get_trace_window` command; works regardless of whether the CPU
/// is currently halted, since `record_trace` already snapshotted everything needed.
fn get_trace_window(trace_state: &Mutex<TraceData>, start_row: usize, count: usize) -> Result<TraceWindowPage, String> {
    let (path, variant, symbols, total_rows, start_row, seek_record) = {
        let data = trace_state.lock().unwrap();
        let path = data.path.clone().ok_or("No trace recorded yet")?;
        let variant = data.variant.ok_or("No trace recorded yet")?;
        let symbols = data.symbols.clone().ok_or("No trace recorded yet")?;
        let row_offsets = data.row_index.lock().unwrap();
        let total_rows = row_offsets.len();
        if total_rows == 0 {
            return Ok(TraceWindowPage { rows: Vec::new(), total_rows: 0 });
        }
        let start_row = start_row.min(total_rows - 1);
        (path, variant, symbols, total_rows, start_row, row_offsets[start_row])
    };

    // Opened fresh per call: simplest option, and an extra open/close per
    // viewport-sized fetch is cheap relative to a UI-paced scroll/poll action.
    let file = File::open(&path).map_err(|e| e.to_string())?;
    let mut reader = BinaryTraceReader::new(file).map_err(|e| e.to_string())?;
    reader.seek_to_record(seek_record).map_err(|e| e.to_string())?;

    let mut assembler = TraceRowAssembler::new(variant, symbols);
    let mut rows = Vec::with_capacity(count.min(total_rows - start_row));
    for item in reader {
        if rows.len() >= count {
            break;
        }
        let rec = item.map_err(|e| e.to_string())?;
        if let Some(row) = assembler.feed(&rec) {
            // Computed up front: it borrows `row` as a whole via `non_fetch_bus_ops(&self)`,
            // which the field-by-field moves out of `row.line` below would otherwise block.
            let bus_ops = row
                .non_fetch_bus_ops()
                .map(|op| match *op {
                    TraceBusOp::Read { addr, value } => TraceBusOpDto {
                        addr,
                        op: "Read".to_string(),
                        value,
                        comment: Disassembler::immediate_mode_comment(value),
                    },
                    TraceBusOp::Write { addr, value } => TraceBusOpDto {
                        addr,
                        op: "Write".to_string(),
                        value,
                        comment: Disassembler::immediate_mode_comment(value),
                    },
                })
                .collect();
            rows.push(TraceRowDto {
                seq: row.instr_id + 1,
                addr: row.line.addr,
                bytes: row.line.raw_bytes.iter().map(|b| format!("{b:02X}")).collect(),
                labels: row.line.labels,
                mnemonic: row.line.mnemonic.to_string(),
                operand: row.line.operand_text,
                comment: row.line.comment_text.unwrap_or_default(),
                is_valid: row.line.is_valid,
                cycles: row.cycles,
                a: row.regs.a,
                x: row.regs.x,
                y: row.regs.y,
                s: row.regs.s,
                p: row.regs.p.to_byte(),
                pc: row.regs.pc,
                bus_ops,
            });
        }
    }

    Ok(TraceWindowPage { rows, total_rows })
}

/// Returns the current recording status, letting the webview restore Record/Stop
/// button state on open. Mirrors the Tauri debugger's `get_trace_status` command.
fn get_trace_status(trace_state: &Mutex<TraceData>) -> TraceStatus {
    let data = trace_state.lock().unwrap();
    TraceStatus { recording: data.recording, path: data.path.as_ref().map(|p| p.display().to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emma65::emulator::{AddressRange, Bus, Cpu, CpuVariant as Variant};

    fn make_cpu() -> Cpu {
        let bus = Bus::config().ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0).unwrap().build();
        Cpu::builder(Variant::Wdc65C02).bus(bus).build().unwrap()
    }

    fn evaluate(context: &str, expression: &str) -> EvaluateArguments {
        EvaluateArguments {
            expression: expression.to_string(),
            context: Some(EvaluateArgumentsContext::String(context.to_string())),
            ..Default::default()
        }
    }

    #[test]
    fn handle_evaluate_returns_none_for_an_unrecognized_context() {
        let state = ExecState::default();
        let trace_state = Mutex::new(TraceData::default());
        assert!(handle_evaluate(&state, &trace_state, &evaluate("something.else", "")).is_none());
    }

    #[test]
    fn get_trace_status_reports_not_recording_before_any_record_trace_call() {
        let state = ExecState::default();
        let trace_state = Mutex::new(TraceData::default());
        let response = handle_evaluate(&state, &trace_state, &evaluate(GET_TRACE_STATUS, "")).unwrap().unwrap();
        let status: TraceStatus = serde_json::from_str(&response.result).unwrap();
        assert!(!status.recording);
        assert!(status.path.is_none());
    }

    #[test]
    fn record_trace_reports_cpu_not_ready_before_launch() {
        let state = ExecState::default();
        let trace_state = Mutex::new(TraceData::default());
        let path = "/tmp/emma65-vscode-adapter-test-not-ready.trace";
        let result = handle_evaluate(
            &state,
            &trace_state,
            &evaluate(RECORD_TRACE, &format!(r#"{{"path":"{path}"}}"#)),
        )
        .unwrap();
        assert_eq!(result.unwrap_err(), "CPU not ready");
    }

    #[test]
    fn record_trace_reports_the_error_for_an_unwritable_path() {
        let state = ExecState::default();
        state.set_cpu(make_cpu());
        let trace_state = Mutex::new(TraceData::default());
        let result = handle_evaluate(
            &state,
            &trace_state,
            &evaluate(RECORD_TRACE, r#"{"path":"/nonexistent-dir/x.trace"}"#),
        )
        .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn get_trace_window_reports_no_trace_recorded_yet() {
        let state = ExecState::default();
        let trace_state = Mutex::new(TraceData::default());
        let result =
            handle_evaluate(&state, &trace_state, &evaluate(GET_TRACE_WINDOW, r#"{"startRow":0,"count":10}"#))
                .unwrap();
        assert_eq!(result.unwrap_err(), "No trace recorded yet");
    }

    #[test]
    fn record_step_stop_and_browse_round_trip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("emma65-vscode-adapter-test-{}.trace", std::process::id()));
        let path_str = path.to_string_lossy().to_string();

        let state = ExecState::default();
        let mut cpu = make_cpu();
        cpu.bus_mut().write(0x0000, 0xEA).unwrap(); // NOP
        cpu.bus_mut().write(0x0001, 0xEA).unwrap(); // NOP
        cpu.registers_mut().pc = 0x0000;
        state.set_cpu(cpu);
        let trace_state = Mutex::new(TraceData::default());

        let record_response = handle_evaluate(
            &state,
            &trace_state,
            &evaluate(RECORD_TRACE, &format!(r#"{{"path":"{path_str}"}}"#)),
        )
        .unwrap()
        .unwrap();
        let status: TraceStatus = serde_json::from_str(&record_response.result).unwrap();
        assert!(status.recording);

        state.with_cpu_mut(emma65::emulator::step_into).unwrap();
        state.with_cpu_mut(emma65::emulator::step_into).unwrap();

        let stop_response = handle_evaluate(&state, &trace_state, &evaluate(STOP_TRACE, "")).unwrap().unwrap();
        let status: TraceStatus = serde_json::from_str(&stop_response.result).unwrap();
        assert!(!status.recording);

        let window_response = handle_evaluate(
            &state,
            &trace_state,
            &evaluate(GET_TRACE_WINDOW, r#"{"startRow":0,"count":10}"#),
        )
        .unwrap()
        .unwrap();
        let page: TraceWindowPage = serde_json::from_str(&window_response.result).unwrap();
        assert_eq!(page.total_rows, 2);
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.rows[0].mnemonic, "NOP");
        assert_eq!(page.rows[0].addr, 0x0000);
        assert_eq!(page.rows[1].addr, 0x0001);

        let _ = std::fs::remove_file(&path);
    }
}
