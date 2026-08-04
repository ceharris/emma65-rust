//! Read-only watchpoint panel support: loads and compiles
//! `~/.emma/debugger/default/watchpoints.emw` at startup and evaluates it
//! against live CPU state on demand.

use std::sync::Mutex;

use emma65::emulator::{map_flag_name, map_register_name, SymbolTable};
use emma65::watch::{WatchCompiler, WatchEvaluator};
use tauri::State;

use crate::theme;
use crate::CpuState;

/// Tauri-managed state wrapping the debugger's own watchpoint evaluator.
///
/// Owns a separate `WatchEvaluator` from the CPU's internal one (used by
/// `Cpu::step()` for real watch-triggered halting) so display evaluation
/// never interferes with execution.
pub struct WatchState(pub Mutex<WatchData>);

/// The debugger's watchpoint evaluator and any whole-file compile error.
pub struct WatchData {
    /// Empty if nothing was loaded or the file failed to compile.
    pub evaluator: WatchEvaluator,
    /// `Some(...)` when `watchpoints.emw` failed to compile; rows are empty in that case.
    pub compile_error: Option<String>,
}

/// One row of the watchpoint panel: an expression's source, its current
/// value, and any per-expression evaluation error.
#[derive(Clone, serde::Serialize)]
pub struct WatchpointRow {
    /// The watchpoint expression's original source text.
    pub source: String,
    /// The evaluated value, or `None` when `error` is `Some`.
    pub value: Option<u32>,
    /// The evaluation error, if this expression failed (e.g. an out-of-range memory fetch).
    pub error: Option<String>,
}

/// A full snapshot of the watchpoint panel's contents for one render.
#[derive(Clone, serde::Serialize)]
pub struct WatchpointsSnapshot {
    /// Whole-file compile error; `rows` is empty when this is `Some`.
    pub compile_error: Option<String>,
    /// One row per loaded watchpoint, in source order.
    pub rows: Vec<WatchpointRow>,
}

/// Loads and compiles `~/.emma/debugger/default/watchpoints.emw` against
/// `symbol_table`. A missing file means zero watchpoints, not an error.
pub fn load_watchpoints(symbol_table: &SymbolTable) -> Result<WatchEvaluator, String> {
    let path = theme::debugger_config_dir()?.join("watchpoints.emw");
    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WatchEvaluator::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let table = symbol_table.clone();
    let mut compiler = WatchCompiler::new(map_register_name, map_flag_name, move |name| {
        table.address_for(name).map(|a| a as u32)
    });
    let mut evaluator = WatchEvaluator::new();
    let (watchpoints, errors) = compiler.compile_all(&source, &mut evaluator);
    if !errors.is_empty() {
        let message = errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
        return Err(message);
    }
    for wp in watchpoints {
        evaluator.add(wp);
    }
    Ok(evaluator)
}

/// Evaluates all loaded watchpoints against the current CPU state and
/// returns a fresh snapshot for the panel to render.
#[tauri::command]
pub fn get_watchpoints(cpu_state: State<CpuState>, watch_state: State<WatchState>) -> WatchpointsSnapshot {
    let mut watch = watch_state.0.lock().unwrap();
    if let Some(err) = &watch.compile_error {
        return WatchpointsSnapshot { compile_error: Some(err.clone()), rows: Vec::new() };
    }
    let cpu_guard = cpu_state.0.lock().unwrap();
    let Some(cpu) = cpu_guard.as_ref() else {
        return WatchpointsSnapshot { compile_error: None, rows: Vec::new() };
    };
    let results = cpu.evaluate_watchpoints(&mut watch.evaluator);
    let rows = watch
        .evaluator
        .watchpoints()
        .iter()
        .zip(results)
        .map(|(wp, result)| match result {
            Ok(value) => WatchpointRow { source: wp.source().to_string(), value: Some(value), error: None },
            Err(e) => WatchpointRow { source: wp.source().to_string(), value: None, error: Some(e.to_string()) },
        })
        .collect();
    WatchpointsSnapshot { compile_error: None, rows }
}
