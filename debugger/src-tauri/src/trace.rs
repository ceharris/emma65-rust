//! Trace window: live-recorded execution trace, windowed reads, and window visibility.

use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use tauri::{AppHandle, State};

use emma65::disasm::{Disassembler, TraceBusOp, TraceRowAssembler};
use emma65::emulator::{
    BinaryTraceReader, BinaryTraceWriter, ChannelTraceCallback, OverflowPolicy,
    TraceCallback, TraceKind, TraceRecord, spawn_trace_writer,
};

use crate::CpuState;

/// Window label of the auxiliary trace window, as declared in `tauri.conf.json`.
pub const TRACE_WINDOW_LABEL: &str = "trace";

/// Capacity of the bounded channel between the CPU thread and the trace writer thread.
const TRACE_CHANNEL_CAPACITY: usize = 4096;

/// Recording state and bookkeeping for the trace window, managed by Tauri.
pub struct TraceState(pub Mutex<TraceData>);

/// The data behind [`TraceState`].
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
}

impl TraceData {
    /// Creates an empty, not-yet-recording state.
    pub fn new() -> Self {
        Self { path: None, row_index: Arc::new(Mutex::new(Vec::new())), recording: false, writer_handle: None }
    }
}

impl Default for TraceData {
    fn default() -> Self {
        Self::new()
    }
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
#[derive(Clone, serde::Serialize)]
pub struct TraceBusOpDto {
    /// The bus address accessed.
    pub addr: u16,
    /// `"Read"` or `"Write"`.
    pub op: String,
    /// The byte value read or written.
    pub value: u8,
    /// Alternate representations of `value` (decimal, ASCII/signed), the same
    /// text `emma65-tracer` shows alongside a bus op's hex value.
    pub comment: String,
}

/// One reconstructed trace row returned to the frontend.
#[derive(Clone, serde::Serialize)]
pub struct TraceRowDto {
    /// 1-based sequence number (`instr_id + 1`), matching the `emma65-tracer` CLI's numbering.
    pub seq: u64,
    /// Instruction address.
    pub addr: u16,
    /// Raw bytes as hex strings, e.g. ["4C", "00", "06"].
    pub bytes: Vec<String>,
    /// Symbol names from the symbol table associated with `addr`, without trailing colons.
    pub labels: Vec<String>,
    /// Mnemonic string, e.g. "JMP".
    pub mnemonic: String,
    /// Formatted operand text, e.g. "$0600".
    pub operand: String,
    /// Comment string.
    pub comment: String,
    /// False for invalid opcodes under the active variant.
    pub is_valid: bool,
    /// The instruction's total cycle count, if known.
    pub cycles: Option<u8>,
    /// Accumulator, as it stood immediately before this instruction executed.
    pub a: u8,
    /// X index register, as it stood immediately before this instruction executed.
    pub x: u8,
    /// Y index register, as it stood immediately before this instruction executed.
    pub y: u8,
    /// Stack pointer, as it stood immediately before this instruction executed.
    pub s: u8,
    /// Status register, as it stood immediately before this instruction executed.
    pub p: u8,
    /// Program counter, as it stood immediately before this instruction executed.
    pub pc: u16,
    /// Bus reads and writes observed while this instruction executed, in stream order.
    pub bus_ops: Vec<TraceBusOpDto>,
}

/// A windowed page of trace rows, returned by `get_trace_window`.
#[derive(Clone, serde::Serialize)]
pub struct TraceWindowPage {
    /// The rows in this page, in row order.
    pub rows: Vec<TraceRowDto>,
    /// Total rows recorded so far, letting the frontend compute the tail window as
    /// `start_row = total_rows.saturating_sub(viewport_rows)` on each poll.
    pub total_rows: usize,
}

/// Recording/browsing status, returned by `get_trace_status`.
#[derive(Clone, serde::Serialize)]
pub struct TraceStatus {
    /// True while a recording is in progress.
    pub recording: bool,
    /// The trace file currently or most recently recorded, if any.
    pub path: Option<String>,
}

/// Starts recording an execution trace to `path`, spooling records to a
/// dedicated writer thread. Only callable while the CPU is halted.
#[tauri::command]
pub fn record_trace(path: String, cpu_state: State<CpuState>, trace_state: State<TraceState>) -> Result<(), String> {
    let mut cpu_guard = cpu_state.0.lock().unwrap();
    let cpu = cpu_guard.as_mut().ok_or("CPU not ready")?;

    let file = File::create(&path).map_err(|e| e.to_string())?;
    let writer = BinaryTraceWriter::new(file, cpu.variant());
    // BlockOnFull, unlike the CLI capture use case's DropOnFull: a debugger
    // trace that silently drops instructions would be misleading. `Cpu::flush_trace`
    // is called once per debugger step/run command (see `exec::step_into` and
    // friends), not per record (which cratered free-run throughput, issue #273),
    // so get_trace_window's independent file handle sees every row from a command
    // by the time its `debugger-halted`/`debugger-run-stopped` event fires.
    let (callback, writer_handle, _dropped) =
        spawn_trace_writer(writer, TRACE_CHANNEL_CAPACITY, OverflowPolicy::BlockOnFull);

    let row_index = Arc::new(Mutex::new(Vec::new()));
    let indexing_callback =
        RowIndexingTraceCallback { inner: callback, row_index: Arc::clone(&row_index), record_count: 0 };
    cpu.set_trace_callback(Some(Box::new(indexing_callback)));

    let mut state = trace_state.0.lock().unwrap();
    state.path = Some(PathBuf::from(&path));
    state.row_index = row_index;
    state.recording = true;
    state.writer_handle = Some(writer_handle);

    Ok(())
}

/// Stops the current recording: detaches the trace callback and waits for the
/// writer thread to flush and exit. The recorded file stays browsable via
/// `get_trace_window`; the next `record_trace` call requires a new path.
#[tauri::command]
pub fn stop_trace(cpu_state: State<CpuState>, trace_state: State<TraceState>) -> Result<(), String> {
    let mut cpu_guard = cpu_state.0.lock().unwrap();
    let cpu = cpu_guard.as_mut().ok_or("CPU not ready")?;
    cpu.set_trace_callback(None);
    drop(cpu_guard);

    let mut state = trace_state.0.lock().unwrap();
    if let Some(handle) = state.writer_handle.take() {
        let _ = handle.join();
    }
    state.recording = false;

    Ok(())
}

/// Returns up to `count` reconstructed trace rows starting at `start_row`
/// (clamped into the recorded range), plus the total row count recorded so far.
#[tauri::command]
pub fn get_trace_window(
    start_row: usize,
    count: usize,
    cpu_state: State<CpuState>,
    trace_state: State<TraceState>,
) -> Result<TraceWindowPage, String> {
    let cpu_guard = cpu_state.0.lock().unwrap();
    let cpu = cpu_guard.as_ref().ok_or("CPU not ready")?;

    let state = trace_state.0.lock().unwrap();
    let path = state.path.as_ref().ok_or("No trace recorded yet")?;
    // Only the length and one offset are ever needed here, so read them out
    // without cloning the whole index — with a long recording, `row_index`
    // can hold millions of entries, and cloning it on every scroll/poll made
    // the window sluggish even though the DOM itself stays small (issue #272).
    let (total_rows, start_row, seek_record) = {
        let row_offsets = state.row_index.lock().unwrap();
        let total_rows = row_offsets.len();
        if total_rows == 0 {
            return Ok(TraceWindowPage { rows: Vec::new(), total_rows: 0 });
        }
        let start_row = start_row.min(total_rows - 1);
        (total_rows, start_row, row_offsets[start_row])
    };

    // Opened fresh per call: simplest option, and an extra open/close per
    // viewport-sized fetch is cheap relative to a UI-paced scroll/poll action.
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut reader = BinaryTraceReader::new(file).map_err(|e| e.to_string())?;
    reader.seek_to_record(seek_record).map_err(|e| e.to_string())?;

    let mut assembler = TraceRowAssembler::new(cpu.variant(), cpu.bus().symbol_table().clone());
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

/// Returns the current recording status, letting the toolbar restore
/// Record/Stop button state on window reopen.
#[tauri::command]
pub fn get_trace_status(trace_state: State<TraceState>) -> TraceStatus {
    let state = trace_state.0.lock().unwrap();
    TraceStatus { recording: state.recording, path: state.path.as_ref().map(|p| p.display().to_string()) }
}

/// Toggles the trace window's visibility. Bound to Ctrl+Shift+T (see
/// `useAppKeyBindings.ts`), mirroring `terminal::toggle_terminal_visibility`.
/// Delegates to the shared helper in `menu.rs` so the Window-menu checkbox
/// stays in sync regardless of which path toggled the window.
#[tauri::command]
pub fn toggle_trace_visibility(app: AppHandle, window_menu: State<crate::menu::WindowMenuState>) -> Result<(), String> {
    crate::menu::toggle_window_visibility(&app, TRACE_WINDOW_LABEL, &window_menu.trace_item)
}
