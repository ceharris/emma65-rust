//! Watchpoint panel support: loads and compiles
//! `~/.emma/debugger/default/watchpoints.emw` at startup, evaluates it
//! against live CPU state on demand, and lets the panel add/remove
//! watchpoints, persisting each change back to the file.

use std::path::Path;
use std::sync::Mutex;

use emma65::emulator::{map_flag_name, map_register_name, Cpu, SymbolTable};
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

/// One row of the watchpoint panel: an expression's source, its
/// triggered/not-triggered status, and any per-expression evaluation error.
#[derive(Clone, serde::Serialize)]
pub struct WatchpointRow {
    /// The watchpoint expression's original source text.
    pub source: String,
    /// True if the expression evaluated to a non-zero value. Meaningless when `error` is `Some`.
    pub triggered: bool,
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
    load_watchpoints_from(&theme::debugger_config_dir()?, symbol_table)
}

/// Loads and compiles `dir/watchpoints.emw` against `symbol_table`. A missing
/// file means zero watchpoints, not an error.
fn load_watchpoints_from(dir: &Path, symbol_table: &SymbolTable) -> Result<WatchEvaluator, String> {
    let path = dir.join("watchpoints.emw");
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
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("watchpoints.emw");
        let message = errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
        return Err(format!("{filename}: {message}"));
    }
    for wp in watchpoints {
        evaluator.add(wp);
    }
    Ok(evaluator)
}

/// Builds a snapshot of `watch`'s current watchpoints evaluated against `cpu`.
///
/// Returns an empty row list if `watch` carries a whole-file compile error or
/// `cpu` is `None` (session not yet ready).
fn build_snapshot(cpu: Option<&Cpu>, watch: &mut WatchData) -> WatchpointsSnapshot {
    if let Some(err) = &watch.compile_error {
        return WatchpointsSnapshot { compile_error: Some(err.clone()), rows: Vec::new() };
    }
    let Some(cpu) = cpu else {
        return WatchpointsSnapshot { compile_error: None, rows: Vec::new() };
    };
    let results = cpu.evaluate_watchpoints(&mut watch.evaluator);
    let rows = watch
        .evaluator
        .watchpoints()
        .iter()
        .zip(results)
        .map(|(wp, result)| match result {
            Ok(value) => WatchpointRow { source: wp.source().to_string(), triggered: value != 0, error: None },
            Err(e) => WatchpointRow { source: wp.source().to_string(), triggered: false, error: Some(e.to_string()) },
        })
        .collect();
    WatchpointsSnapshot { compile_error: None, rows }
}

/// Serializes `evaluator`'s watchpoints back to `~/.emma/debugger/default/watchpoints.emw`,
/// one semicolon-terminated expression per line, in display order.
fn save_watchpoints(evaluator: &WatchEvaluator) -> Result<(), String> {
    save_watchpoints_to(&theme::debugger_config_dir()?, evaluator)
}

/// Serializes `evaluator`'s watchpoints back to `dir/watchpoints.emw`, one
/// semicolon-terminated expression per line, in display order.
fn save_watchpoints_to(dir: &Path, evaluator: &WatchEvaluator) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create config directory: {e}"))?;
    let path = dir.join("watchpoints.emw");
    let contents: String = evaluator.watchpoints().iter().map(|wp| format!("{};\n", wp.source())).collect();
    std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

/// Evaluates all loaded watchpoints against the current CPU state and
/// returns a fresh snapshot for the panel to render.
#[tauri::command]
pub fn get_watchpoints(cpu_state: State<CpuState>, watch_state: State<WatchState>) -> WatchpointsSnapshot {
    let mut watch = watch_state.0.lock().unwrap();
    let cpu_guard = cpu_state.0.lock().unwrap();
    build_snapshot(cpu_guard.as_ref(), &mut watch)
}

/// Compiles `source` as a new watchpoint, appends it, persists the updated
/// watchpoint file, and returns a fresh snapshot.
///
/// Fails without modifying state if `source` fails to compile, or if the
/// loaded file already carries an unresolved whole-file compile error.
#[tauri::command]
pub fn add_watchpoint(
    source: String,
    cpu_state: State<CpuState>,
    watch_state: State<WatchState>,
) -> Result<WatchpointsSnapshot, String> {
    let mut watch = watch_state.0.lock().unwrap();
    if watch.compile_error.is_some() {
        return Err("watchpoints.emw has a compile error; fix it before adding watchpoints".to_string());
    }
    let cpu_guard = cpu_state.0.lock().unwrap();
    let cpu = cpu_guard.as_ref().ok_or("CPU not ready")?;
    let table = cpu.bus().symbol_table().clone();
    let mut compiler = WatchCompiler::new(map_register_name, map_flag_name, move |name| {
        table.address_for(name).map(|a| a as u32)
    });
    let watchpoint = compiler.compile(&source, &mut watch.evaluator).map_err(|e| e.to_string())?;
    watch.evaluator.add(watchpoint);
    save_watchpoints(&watch.evaluator)?;
    Ok(build_snapshot(Some(cpu), &mut watch))
}

/// Removes the watchpoint at `index`, persists the updated watchpoint file,
/// and returns a fresh snapshot.
#[tauri::command]
pub fn remove_watchpoint(
    index: usize,
    cpu_state: State<CpuState>,
    watch_state: State<WatchState>,
) -> Result<WatchpointsSnapshot, String> {
    let mut watch = watch_state.0.lock().unwrap();
    if watch.compile_error.is_some() {
        return Err("watchpoints.emw has a compile error; fix it before removing watchpoints".to_string());
    }
    if index >= watch.evaluator.watchpoints().len() {
        return Err("Invalid watchpoint index".to_string());
    }
    watch.evaluator.remove(index);
    save_watchpoints(&watch.evaluator)?;
    let cpu_guard = cpu_state.0.lock().unwrap();
    Ok(build_snapshot(cpu_guard.as_ref(), &mut watch))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a fresh, uniquely-named temp directory for one test's config files.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("emma65-watchpoints-test-{name}-{:?}", std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn evaluator_with(sources: &[&str]) -> WatchEvaluator {
        let mut compiler = WatchCompiler::new(map_register_name, map_flag_name, |_| None);
        let mut evaluator = WatchEvaluator::new();
        for source in sources {
            let wp = compiler.compile(source, &mut evaluator).unwrap();
            evaluator.add(wp);
        }
        evaluator
    }

    #[test]
    fn save_watchpoints_writes_one_semicolon_terminated_expression_per_line() {
        let dir = temp_dir("save-basic");
        let evaluator = evaluator_with(&["A == 0", "X == 1"]);
        save_watchpoints_to(&dir, &evaluator).unwrap();
        let contents = std::fs::read_to_string(dir.join("watchpoints.emw")).unwrap();
        assert_eq!(contents, "A == 0;\nX == 1;\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_watchpoints_writes_empty_file_when_no_watchpoints() {
        let dir = temp_dir("save-empty");
        save_watchpoints_to(&dir, &WatchEvaluator::new()).unwrap();
        let contents = std::fs::read_to_string(dir.join("watchpoints.emw")).unwrap();
        assert_eq!(contents, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_watchpoints_round_trips_through_load_watchpoints() {
        let dir = temp_dir("round-trip");
        let evaluator = evaluator_with(&["A == 0", "X == 1"]);
        save_watchpoints_to(&dir, &evaluator).unwrap();
        let reloaded = load_watchpoints_from(&dir, &SymbolTable::new()).unwrap();
        let sources: Vec<&str> = reloaded.watchpoints().iter().map(|wp| wp.source()).collect();
        assert_eq!(sources, vec!["A == 0", "X == 1"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
