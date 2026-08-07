//! Watchpoints webview support, mirroring the Tauri debugger's `watchpoints.rs`, but
//! reached through DAP's `evaluate` request rather than native Tauri commands or a
//! genuinely custom DAP request — `dap = "0.4.1-alpha1"`'s closed `Command`/`Event`
//! enums rule both of those out (see `session.rs`'s `TERMINAL_SOCKET_SPEC` doc
//! comment and `bus.rs`'s module doc comment for the full story-8/9 finding that
//! established this pattern). Like `trace.rs`, the payload here (a full watchpoints
//! snapshot) is structured, so `expression`/`result` carry JSON.
//!
//! Persists to `~/.emma/vscode/default/watchpoints.emw`, a separate location from
//! the Tauri debugger's `~/.emma/debugger/default/watchpoints.emw` — same reasoning
//! as `session.rs`'s distinct `vscode-terminal` socket name: a VS Code session's
//! watchpoints shouldn't silently overwrite (or be overwritten by) a Tauri debugger
//! profile's.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dap::requests::EvaluateArguments;
use dap::responses::EvaluateResponse;
use dap::types::EvaluateArgumentsContext;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use emma65::emulator::{Cpu, SymbolTable, map_flag_name, map_register_name};
use emma65::watch::{WatchCompiler, WatchEvaluator};

use crate::exec::ExecState;

const GET_WATCHPOINTS: &str = "emma65.getWatchpoints";
const ADD_WATCHPOINT: &str = "emma65.addWatchpoint";
const REMOVE_WATCHPOINT: &str = "emma65.removeWatchpoint";
const EDIT_WATCHPOINT: &str = "emma65.editWatchpoint";
const TOGGLE_WATCHPOINT: &str = "emma65.toggleWatchpoint";

/// The webview's watchpoint evaluator and any whole-file compile error, owned
/// separately from the CPU's own evaluator (used by `Cpu::step()` for real
/// watch-triggered halting) so display evaluation never interferes with execution.
#[derive(Default)]
pub struct WatchData {
    /// Empty if nothing was loaded or the file failed to compile.
    pub evaluator: WatchEvaluator,
    /// `Some(...)` when `watchpoints.emw` failed to compile; rows are empty in that case.
    pub compile_error: Option<String>,
    /// Per-watchpoint enabled flag, aligned by index with `evaluator.watchpoints()`.
    pub enabled: Vec<bool>,
}

/// One row of the watchpoint webview: an expression's source, its
/// triggered/not-triggered status, any per-expression evaluation error, and
/// whether it currently participates in execution halting.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WatchpointRowDto {
    source: String,
    triggered: bool,
    error: Option<String>,
    enabled: bool,
}

/// One watch variable's name and current value, as last assigned by a `:=` expression.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VariableRowDto {
    name: String,
    value: u32,
}

/// A full snapshot of the watchpoint webview's contents for one render.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WatchpointsSnapshot {
    /// Whole-file compile error; `rows` is empty when this is `Some`.
    compile_error: Option<String>,
    /// One row per loaded watchpoint, in source order.
    rows: Vec<WatchpointRowDto>,
    /// One row per watch variable currently known to the evaluator, in the
    /// order each was first introduced by a walrus assignment.
    variables: Vec<VariableRowDto>,
}

/// `emma65.addWatchpoint`'s `expression` payload.
#[derive(Deserialize)]
struct AddWatchpointArgs {
    source: String,
}

/// `emma65.removeWatchpoint`'s and `emma65.toggleWatchpoint`'s `expression` payload.
#[derive(Deserialize)]
struct IndexArgs {
    index: usize,
}

/// `emma65.editWatchpoint`'s `expression` payload.
#[derive(Deserialize)]
struct EditWatchpointArgs {
    index: usize,
    source: String,
}

/// Dispatches a recognized `emma65.*` watchpoint `evaluate` context to its operation.
/// Returns `None` if `args.context` isn't one of the constants above, so the caller
/// (chained after `bus::handle_evaluate`/`trace::handle_evaluate`) can respond
/// "unsupported request" the same way it does for any other unrecognized command.
pub fn handle_evaluate(
    state: &ExecState,
    watch_state: &Mutex<WatchData>,
    args: &EvaluateArguments,
) -> Option<Result<EvaluateResponse, String>> {
    let context = match &args.context {
        Some(EvaluateArgumentsContext::String(context)) => context.as_str(),
        _ => return None,
    };
    let result = match context {
        GET_WATCHPOINTS => Ok(get_watchpoints(state, watch_state)),
        ADD_WATCHPOINT => (|| {
            let dir = config_dir()?;
            let a = parse_args::<AddWatchpointArgs>(args)?;
            add_watchpoint(&dir, state, watch_state, a.source)
        })(),
        REMOVE_WATCHPOINT => (|| {
            let dir = config_dir()?;
            let a = parse_args::<IndexArgs>(args)?;
            remove_watchpoint(&dir, state, watch_state, a.index)
        })(),
        EDIT_WATCHPOINT => (|| {
            let dir = config_dir()?;
            let a = parse_args::<EditWatchpointArgs>(args)?;
            edit_watchpoint(&dir, state, watch_state, a.index, a.source)
        })(),
        TOGGLE_WATCHPOINT => (|| {
            let dir = config_dir()?;
            let a = parse_args::<IndexArgs>(args)?;
            toggle_watchpoint(&dir, state, watch_state, a.index)
        })(),
        _ => return None,
    };
    Some(result.and_then(|snapshot| json_response(&snapshot)))
}

fn parse_args<T: DeserializeOwned>(args: &EvaluateArguments) -> Result<T, String> {
    serde_json::from_str(&args.expression).map_err(|e| format!("invalid arguments: {e}"))
}

fn json_response<T: Serialize>(value: &T) -> Result<EvaluateResponse, String> {
    let result = serde_json::to_string(value).map_err(|e| e.to_string())?;
    Ok(EvaluateResponse { result, ..Default::default() })
}

/// Returns `~/.emma/vscode/default`, the adapter's own config directory for
/// webview-persisted state (kept separate from the Tauri debugger's
/// `~/.emma/debugger/default` — see this module's doc comment).
fn config_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable is not set".to_string())?;
    Ok(Path::new(&home).join(".emma/vscode/default"))
}

/// Loads and compiles `~/.emma/vscode/default/watchpoints.emw` against
/// `symbol_table`, along with each watchpoint's enabled state. A missing file
/// means zero watchpoints, not an error.
pub fn load_watchpoints(symbol_table: &SymbolTable) -> Result<(WatchEvaluator, Vec<bool>), String> {
    load_watchpoints_from(&config_dir()?, symbol_table)
}

/// Loads and compiles `dir/watchpoints.emw` against `symbol_table`, along with
/// each watchpoint's enabled state from `dir/watchpoints.enabled`. A missing
/// `.emw` file means zero watchpoints, not an error.
fn load_watchpoints_from(dir: &Path, symbol_table: &SymbolTable) -> Result<(WatchEvaluator, Vec<bool>), String> {
    let path = dir.join("watchpoints.emw");
    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((WatchEvaluator::new(), Vec::new())),
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
    let count = watchpoints.len();
    for wp in watchpoints {
        evaluator.add(wp);
    }
    let enabled = load_enabled_from(dir, count);
    Ok((evaluator, enabled))
}

/// Reads `dir/watchpoints.enabled`: one `1`/`0` line per watchpoint, in the
/// same order as `watchpoints.emw`. A missing file, or a line count that
/// doesn't match `count`, means every watchpoint is enabled.
fn load_enabled_from(dir: &Path, count: usize) -> Vec<bool> {
    let path = dir.join("watchpoints.enabled");
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let flags: Vec<bool> = contents.lines().map(|line| line.trim() == "1").collect();
            if flags.len() == count { flags } else { vec![true; count] }
        }
        Err(_) => vec![true; count],
    }
}

/// Builds a snapshot of `watch`'s current watchpoints evaluated against `cpu`,
/// along with its current watch variables.
///
/// Returns an empty row list (and no variables) if `watch` carries a
/// whole-file compile error. When `cpu` is `None` (session not yet ready, or
/// a background run/step currently owns it), rows are empty but variables
/// still reflect whatever was last assigned. Disabled watchpoints are still
/// evaluated (so walrus assignments they make stay visible to later
/// watchpoints), but their row always reports `triggered: false` and no
/// error, since a disabled watchpoint's status is meaningless to the user.
fn build_snapshot(cpu: Option<&Cpu>, watch: &mut WatchData) -> WatchpointsSnapshot {
    if let Some(err) = &watch.compile_error {
        return WatchpointsSnapshot { compile_error: Some(err.clone()), rows: Vec::new(), variables: Vec::new() };
    }
    let Some(cpu) = cpu else {
        let variables = collect_variables(&watch.evaluator);
        return WatchpointsSnapshot { compile_error: None, rows: Vec::new(), variables };
    };
    let results = cpu.evaluate_watchpoints(&mut watch.evaluator);
    let rows = watch
        .evaluator
        .watchpoints()
        .iter()
        .zip(results)
        .zip(watch.enabled.iter().copied())
        .map(|((wp, result), enabled)| {
            if !enabled {
                return WatchpointRowDto { source: wp.source().to_string(), triggered: false, error: None, enabled };
            }
            match result {
                Ok(value) => {
                    WatchpointRowDto { source: wp.source().to_string(), triggered: value != 0, error: None, enabled }
                }
                Err(e) => WatchpointRowDto {
                    source: wp.source().to_string(),
                    triggered: false,
                    error: Some(e.to_string()),
                    enabled,
                },
            }
        })
        .collect();
    let variables = collect_variables(&watch.evaluator);
    WatchpointsSnapshot { compile_error: None, rows, variables }
}

/// Snapshots `evaluator`'s current watch variables as owned `VariableRowDto`s,
/// in the order each was first introduced by a walrus assignment.
fn collect_variables(evaluator: &WatchEvaluator) -> Vec<VariableRowDto> {
    evaluator.named_variables().into_iter().map(|(name, value)| VariableRowDto { name: name.to_string(), value }).collect()
}

/// Rebuilds `cpu`'s own watch evaluator — the one `Cpu::step()` consults to
/// halt execution — from `evaluator`'s currently *enabled* watchpoints, so
/// webview watchpoints actually stop the running CPU instead of only being
/// reflected in the display snapshot, and disabled ones never participate in
/// that halt check.
///
/// Recompiles each source string into `cpu`'s evaluator rather than sharing
/// `Watchpoint` values, since the two evaluators keep independent variable
/// storage (`evaluator`'s display re-evaluation must never perturb the state
/// `step()` relies on for real halting).
pub fn sync_cpu_evaluator(cpu: &mut Cpu, evaluator: &WatchEvaluator, enabled: &[bool]) -> Result<(), String> {
    let table = cpu.bus().symbol_table().clone();
    let mut compiler = WatchCompiler::new(map_register_name, map_flag_name, move |name| {
        table.address_for(name).map(|a| a as u32)
    });
    let sources: Vec<&str> = evaluator
        .watchpoints()
        .iter()
        .zip(enabled.iter().copied())
        .filter(|(_, enabled)| *enabled)
        .map(|(wp, _)| wp.source())
        .collect();
    let exec_evaluator = cpu.evaluator_mut();
    exec_evaluator.clear();
    for source in sources {
        let wp = compiler.compile(source, exec_evaluator).map_err(|e| e.to_string())?;
        exec_evaluator.add(wp);
    }
    Ok(())
}

/// Serializes `evaluator`'s watchpoints to `dir/watchpoints.emw`, one
/// semicolon-terminated expression per line, and `enabled` to
/// `dir/watchpoints.enabled`, one `1`/`0` line per watchpoint, both in
/// display order.
fn save_watchpoints_to(dir: &Path, evaluator: &WatchEvaluator, enabled: &[bool]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create config directory: {e}"))?;
    let path = dir.join("watchpoints.emw");
    let contents: String = evaluator.watchpoints().iter().map(|wp| format!("{};\n", wp.source())).collect();
    std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    let enabled_path = dir.join("watchpoints.enabled");
    let enabled_contents: String = enabled.iter().map(|&e| if e { "1\n" } else { "0\n" }).collect();
    std::fs::write(&enabled_path, enabled_contents).map_err(|e| format!("{}: {e}", enabled_path.display()))
}

/// Recompiles `sources` in order against `symbol_table`, in a fresh
/// evaluator, used to validate an edited watchpoint and, on success, replace
/// the evaluator that backs the webview.
///
/// Rebuilding from source strings (rather than replacing one `Watchpoint` in
/// place) keeps variable IDs consistent: the compiler assigns them by source
/// order, and a walrus assignment in one watchpoint must resolve to the same
/// ID when read by a later one.
fn recompile_sources(sources: &[String], symbol_table: &SymbolTable) -> Result<WatchEvaluator, String> {
    let table = symbol_table.clone();
    let mut compiler = WatchCompiler::new(map_register_name, map_flag_name, move |name| {
        table.address_for(name).map(|a| a as u32)
    });
    let mut evaluator = WatchEvaluator::new();
    for source in sources {
        let wp = compiler.compile(source, &mut evaluator).map_err(|e| e.to_string())?;
        evaluator.add(wp);
    }
    Ok(evaluator)
}

/// Evaluates all loaded watchpoints against the current CPU state and
/// returns a fresh snapshot for the webview to render. Mirrors the Tauri
/// debugger's `get_watchpoints` command.
fn get_watchpoints(state: &ExecState, watch_state: &Mutex<WatchData>) -> WatchpointsSnapshot {
    let mut watch = watch_state.lock().unwrap();
    match state.with_cpu(|cpu| build_snapshot(Some(cpu), &mut watch)) {
        Some(snapshot) => snapshot,
        None => build_snapshot(None, &mut watch),
    }
}

/// Compiles `source` as a new watchpoint, appends it (enabled), persists the
/// updated watchpoint file, and returns a fresh snapshot. Mirrors the Tauri
/// debugger's `add_watchpoint` command.
///
/// Fails without modifying state if `source` fails to compile, if the loaded
/// file already carries an unresolved whole-file compile error, or if the
/// CPU isn't currently halted (symbol resolution needs direct CPU access).
fn add_watchpoint(
    dir: &Path,
    state: &ExecState,
    watch_state: &Mutex<WatchData>,
    source: String,
) -> Result<WatchpointsSnapshot, String> {
    let mut watch = watch_state.lock().unwrap();
    if watch.compile_error.is_some() {
        return Err("watchpoints.emw has a compile error; fix it before adding watchpoints".to_string());
    }
    state
        .with_cpu_mut(|cpu| {
            let table = cpu.bus().symbol_table().clone();
            let mut compiler = WatchCompiler::new(map_register_name, map_flag_name, move |name| {
                table.address_for(name).map(|a| a as u32)
            });
            let watchpoint = compiler.compile(&source, &mut watch.evaluator).map_err(|e| e.to_string())?;
            watch.evaluator.add(watchpoint);
            watch.enabled.push(true);
            sync_cpu_evaluator(cpu, &watch.evaluator, &watch.enabled)?;
            save_watchpoints_to(dir, &watch.evaluator, &watch.enabled)?;
            Ok(build_snapshot(Some(cpu), &mut watch))
        })
        .ok_or_else(|| "CPU not ready".to_string())?
}

/// Removes the watchpoint at `index`, persists the updated watchpoint file,
/// and returns a fresh snapshot. Mirrors the Tauri debugger's
/// `remove_watchpoint` command.
fn remove_watchpoint(
    dir: &Path,
    state: &ExecState,
    watch_state: &Mutex<WatchData>,
    index: usize,
) -> Result<WatchpointsSnapshot, String> {
    let mut watch = watch_state.lock().unwrap();
    if watch.compile_error.is_some() {
        return Err("watchpoints.emw has a compile error; fix it before removing watchpoints".to_string());
    }
    if index >= watch.evaluator.watchpoints().len() {
        return Err("Invalid watchpoint index".to_string());
    }
    watch.evaluator.remove(index);
    watch.enabled.remove(index);
    let synced = state.with_cpu_mut(|cpu| sync_cpu_evaluator(cpu, &watch.evaluator, &watch.enabled));
    if let Some(Err(e)) = synced {
        return Err(e);
    }
    save_watchpoints_to(dir, &watch.evaluator, &watch.enabled)?;
    match state.with_cpu(|cpu| build_snapshot(Some(cpu), &mut watch)) {
        Some(snapshot) => Ok(snapshot),
        None => Ok(build_snapshot(None, &mut watch)),
    }
}

/// Replaces the expression at `index` with `source`, persists the updated
/// watchpoint file, and returns a fresh snapshot. Mirrors the Tauri
/// debugger's `edit_watchpoint` command.
///
/// Fails without modifying state if the edited source fails to compile, if
/// the loaded file already carries an unresolved whole-file compile error,
/// or if the CPU isn't currently halted. The watchpoint's enabled state is
/// left unchanged.
fn edit_watchpoint(
    dir: &Path,
    state: &ExecState,
    watch_state: &Mutex<WatchData>,
    index: usize,
    source: String,
) -> Result<WatchpointsSnapshot, String> {
    let mut watch = watch_state.lock().unwrap();
    if watch.compile_error.is_some() {
        return Err("watchpoints.emw has a compile error; fix it before editing watchpoints".to_string());
    }
    if index >= watch.evaluator.watchpoints().len() {
        return Err("Invalid watchpoint index".to_string());
    }
    state
        .with_cpu_mut(|cpu| {
            let mut sources: Vec<String> =
                watch.evaluator.watchpoints().iter().map(|wp| wp.source().to_string()).collect();
            sources[index] = source;
            watch.evaluator = recompile_sources(&sources, cpu.bus().symbol_table())?;
            sync_cpu_evaluator(cpu, &watch.evaluator, &watch.enabled)?;
            save_watchpoints_to(dir, &watch.evaluator, &watch.enabled)?;
            Ok(build_snapshot(Some(cpu), &mut watch))
        })
        .ok_or_else(|| "CPU not ready".to_string())?
}

/// Toggles the enabled state of the watchpoint at `index`, persists the
/// updated state, and returns a fresh snapshot. Mirrors the Tauri debugger's
/// `toggle_watchpoint` command.
///
/// A disabled watchpoint is removed from `cpu`'s own evaluator so it can
/// never halt execution, but remains in the webview's list, grayed, with a
/// neutral status until re-enabled.
fn toggle_watchpoint(
    dir: &Path,
    state: &ExecState,
    watch_state: &Mutex<WatchData>,
    index: usize,
) -> Result<WatchpointsSnapshot, String> {
    let mut watch = watch_state.lock().unwrap();
    if watch.compile_error.is_some() {
        return Err("watchpoints.emw has a compile error; fix it before editing watchpoints".to_string());
    }
    let Some(enabled) = watch.enabled.get_mut(index) else {
        return Err("Invalid watchpoint index".to_string());
    };
    *enabled = !*enabled;
    let synced = state.with_cpu_mut(|cpu| sync_cpu_evaluator(cpu, &watch.evaluator, &watch.enabled));
    if let Some(Err(e)) = synced {
        return Err(e);
    }
    save_watchpoints_to(dir, &watch.evaluator, &watch.enabled)?;
    match state.with_cpu(|cpu| build_snapshot(Some(cpu), &mut watch)) {
        Some(snapshot) => Ok(snapshot),
        None => Ok(build_snapshot(None, &mut watch)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emma65::emulator::cpu::StepResult;
    use emma65::emulator::{AddressRange, Bus, CpuVariant};

    fn make_cpu() -> Cpu {
        let bus = Bus::config().ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0).unwrap().build();
        Cpu::builder(CpuVariant::Wdc65C02).bus(bus).build().unwrap()
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
        let watch_state = Mutex::new(WatchData::default());
        assert!(handle_evaluate(&state, &watch_state, &evaluate("something.else", "")).is_none());
    }

    #[test]
    fn get_watchpoints_reports_empty_rows_before_launch() {
        let state = ExecState::default();
        let watch_state = Mutex::new(WatchData::default());
        let response = handle_evaluate(&state, &watch_state, &evaluate(GET_WATCHPOINTS, "")).unwrap().unwrap();
        let snapshot: WatchpointsSnapshot = serde_json::from_str(&response.result).unwrap();
        assert!(snapshot.compile_error.is_none());
        assert!(snapshot.rows.is_empty());
        assert!(snapshot.variables.is_empty());
    }

    #[test]
    fn add_watchpoint_reports_cpu_not_ready_before_launch() {
        let state = ExecState::default();
        let watch_state = Mutex::new(WatchData::default());
        let result = handle_evaluate(&state, &watch_state, &evaluate(ADD_WATCHPOINT, r#"{"source":"A == 0"}"#)).unwrap();
        assert_eq!(result.unwrap_err(), "CPU not ready");
    }

    #[test]
    fn add_watchpoint_reports_a_compile_error_for_invalid_source() {
        let state = ExecState::default();
        state.set_cpu(make_cpu());
        let watch_state = Mutex::new(WatchData::default());
        let result = handle_evaluate(&state, &watch_state, &evaluate(ADD_WATCHPOINT, r#"{"source":"@@@"}"#)).unwrap();
        assert!(result.is_err());
    }

    // The four tests below call `add_watchpoint`/`remove_watchpoint`/`edit_watchpoint`/
    // `toggle_watchpoint` directly (with an explicit temp dir) rather than through
    // `handle_evaluate`, since `handle_evaluate`'s `ADD_WATCHPOINT`/etc. branches
    // resolve `config_dir()` — the real `~/.emma/vscode/default` — and would
    // otherwise persist test data into the user's actual home directory.

    #[test]
    fn add_then_get_watchpoints_round_trips_a_new_row() {
        let dir = temp_dir("add-then-get");
        let state = ExecState::default();
        state.set_cpu(make_cpu());
        let watch_state = Mutex::new(WatchData::default());

        let snapshot = add_watchpoint(&dir, &state, &watch_state, "A == 0".to_string()).unwrap();
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.rows[0].source, "A == 0");
        assert!(snapshot.rows[0].enabled);
        assert!(snapshot.rows[0].triggered);

        // The added watchpoint must also reach the CPU's own evaluator, so it
        // actually halts execution (regression scenario for issue #246 in the
        // Tauri debugger, ported here since `add_watchpoint` calls the same
        // `sync_cpu_evaluator`).
        assert!(state.with_cpu(|cpu| !cpu.evaluator().is_empty()).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_watchpoint_disables_without_removing_the_row() {
        let dir = temp_dir("toggle");
        let state = ExecState::default();
        state.set_cpu(make_cpu());
        let watch_state = Mutex::new(WatchData::default());
        add_watchpoint(&dir, &state, &watch_state, "A == 0".to_string()).unwrap();

        let snapshot = toggle_watchpoint(&dir, &state, &watch_state, 0).unwrap();
        assert_eq!(snapshot.rows.len(), 1);
        assert!(!snapshot.rows[0].enabled);
        assert!(!snapshot.rows[0].triggered);
        assert!(state.with_cpu(|cpu| cpu.evaluator().is_empty()).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edit_watchpoint_replaces_the_source_at_index() {
        let dir = temp_dir("edit");
        let state = ExecState::default();
        state.set_cpu(make_cpu());
        let watch_state = Mutex::new(WatchData::default());
        add_watchpoint(&dir, &state, &watch_state, "A == 0".to_string()).unwrap();

        let snapshot = edit_watchpoint(&dir, &state, &watch_state, 0, "X == 0".to_string()).unwrap();
        assert_eq!(snapshot.rows[0].source, "X == 0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_watchpoint_deletes_the_row() {
        let dir = temp_dir("remove");
        let state = ExecState::default();
        state.set_cpu(make_cpu());
        let watch_state = Mutex::new(WatchData::default());
        add_watchpoint(&dir, &state, &watch_state, "A == 0".to_string()).unwrap();

        let snapshot = remove_watchpoint(&dir, &state, &watch_state, 0).unwrap();
        assert!(snapshot.rows.is_empty());
        assert!(state.with_cpu(|cpu| cpu.evaluator().is_empty()).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_watchpoint_reports_invalid_index() {
        let dir = temp_dir("remove-invalid-index");
        let state = ExecState::default();
        state.set_cpu(make_cpu());
        let watch_state = Mutex::new(WatchData::default());
        let result = remove_watchpoint(&dir, &state, &watch_state, 0);
        assert_eq!(result.unwrap_err(), "Invalid watchpoint index");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_cpu_evaluator_installs_watchpoints_that_halt_execution() {
        let mut cpu = make_cpu();
        cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
        cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
        cpu.reset().unwrap();
        cpu.bus_mut().write(0x0200, 0xEA).unwrap(); // NOP
        cpu.bus_mut().write(0x0201, 0xA2).unwrap(); // LDX #$FF
        cpu.bus_mut().write(0x0202, 0xFF).unwrap();
        cpu.bus_mut().write(0x0203, 0xEA).unwrap(); // NOP

        let evaluator = evaluator_with(&["X == $FF"]);
        sync_cpu_evaluator(&mut cpu, &evaluator, &[true]).unwrap();
        assert!(!cpu.evaluator().is_empty());

        let mut result = StepResult::Waiting;
        for _ in 0..3 {
            result = cpu.step(None, true);
            if matches!(result, StepResult::WatchTriggered { .. }) {
                break;
            }
        }
        assert!(matches!(result, StepResult::WatchTriggered { pc: 0x0203, .. }));
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

    /// Returns a fresh, uniquely-named temp directory for one test's config files.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("emma65-vscode-adapter-watchpoints-test-{name}-{:?}", std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_watchpoints_round_trips_through_load_watchpoints() {
        let dir = temp_dir("round-trip");
        let evaluator = evaluator_with(&["A == 0", "X == 1"]);
        save_watchpoints_to(&dir, &evaluator, &[true, false]).unwrap();
        let (reloaded, enabled) = load_watchpoints_from(&dir, &SymbolTable::new()).unwrap();
        let sources: Vec<&str> = reloaded.watchpoints().iter().map(|wp| wp.source()).collect();
        assert_eq!(sources, vec!["A == 0", "X == 1"]);
        assert_eq!(enabled, vec![true, false]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_watchpoints_from_missing_file_reports_zero_watchpoints() {
        let dir = temp_dir("missing-file");
        let (reloaded, enabled) = load_watchpoints_from(&dir, &SymbolTable::new()).unwrap();
        assert!(reloaded.watchpoints().is_empty());
        assert!(enabled.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
