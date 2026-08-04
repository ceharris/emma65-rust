//! Watchpoint panel support: loads and compiles
//! `~/.emma/debugger/default/watchpoints.emw` at startup, evaluates it
//! against live CPU state on demand, and lets the panel add/remove
//! watchpoints, persisting each change back to the file.

use std::path::Path;
use std::sync::Mutex;

use emma65::emulator::{Cpu, SymbolTable, map_flag_name, map_register_name};
use emma65::watch::{WatchCompiler, WatchEvaluator};
use tauri::State;

use crate::CpuState;
use crate::theme;

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
    /// Per-watchpoint enabled flag, aligned by index with `evaluator.watchpoints()`.
    pub enabled: Vec<bool>,
}

/// One row of the watchpoint panel: an expression's source, its
/// triggered/not-triggered status, any per-expression evaluation error, and
/// whether it currently participates in execution halting.
#[derive(Clone, serde::Serialize)]
pub struct WatchpointRow {
    /// The watchpoint expression's original source text.
    pub source: String,
    /// True if the expression evaluated to a non-zero value. Meaningless when `error` is `Some`.
    pub triggered: bool,
    /// The evaluation error, if this expression failed (e.g. an out-of-range memory fetch).
    pub error: Option<String>,
    /// False if the watchpoint has been disabled: it no longer halts execution.
    pub enabled: bool,
}

/// One row of the watchpoint panel's variables section: a variable's name
/// and its current value, as last assigned by a `:=` expression.
#[derive(Clone, serde::Serialize)]
pub struct VariableRow {
    /// The variable's name, as it appears on the left-hand side of `:=`.
    pub name: String,
    /// The variable's current value.
    pub value: u32,
}

/// A full snapshot of the watchpoint panel's contents for one render.
#[derive(Clone, serde::Serialize)]
pub struct WatchpointsSnapshot {
    /// Whole-file compile error; `rows` is empty when this is `Some`.
    pub compile_error: Option<String>,
    /// One row per loaded watchpoint, in source order.
    pub rows: Vec<WatchpointRow>,
    /// One row per watch variable currently known to the evaluator, in the
    /// order each was first introduced by a walrus assignment.
    pub variables: Vec<VariableRow>,
}

/// Loads and compiles `~/.emma/debugger/default/watchpoints.emw` against
/// `symbol_table`, along with each watchpoint's enabled state. A missing file
/// means zero watchpoints, not an error.
pub fn load_watchpoints(symbol_table: &SymbolTable) -> Result<(WatchEvaluator, Vec<bool>), String> {
    load_watchpoints_from(&theme::debugger_config_dir()?, symbol_table)
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
/// whole-file compile error. When `cpu` is `None` (session not yet ready),
/// rows are empty but variables still reflect whatever was last assigned.
/// Disabled watchpoints are still evaluated (so walrus assignments they make
/// stay visible to later watchpoints), but their row always reports
/// `triggered: false` and no error, since a disabled watchpoint's status is
/// meaningless to the user.
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
                return WatchpointRow { source: wp.source().to_string(), triggered: false, error: None, enabled };
            }
            match result {
                Ok(value) => {
                    WatchpointRow { source: wp.source().to_string(), triggered: value != 0, error: None, enabled }
                }
                Err(e) => WatchpointRow {
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

/// Snapshots `evaluator`'s current watch variables as owned `VariableRow`s,
/// in the order each was first introduced by a walrus assignment.
fn collect_variables(evaluator: &WatchEvaluator) -> Vec<VariableRow> {
    evaluator.named_variables().into_iter().map(|(name, value)| VariableRow { name: name.to_string(), value }).collect()
}

/// Rebuilds `cpu`'s own watch evaluator — the one `Cpu::step()` consults to
/// halt execution — from `evaluator`'s currently *enabled* watchpoints, so
/// panel watchpoints actually stop the running CPU instead of only being
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

/// Serializes `evaluator`'s watchpoints and `enabled` state back to
/// `~/.emma/debugger/default/watchpoints.emw` and `watchpoints.enabled`.
fn save_watchpoints(evaluator: &WatchEvaluator, enabled: &[bool]) -> Result<(), String> {
    save_watchpoints_to(&theme::debugger_config_dir()?, evaluator, enabled)
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

/// Evaluates all loaded watchpoints against the current CPU state and
/// returns a fresh snapshot for the panel to render.
#[tauri::command]
pub fn get_watchpoints(cpu_state: State<CpuState>, watch_state: State<WatchState>) -> WatchpointsSnapshot {
    let mut watch = watch_state.0.lock().unwrap();
    let cpu_guard = cpu_state.0.lock().unwrap();
    build_snapshot(cpu_guard.as_ref(), &mut watch)
}

/// Compiles `source` as a new watchpoint, appends it (enabled), persists the
/// updated watchpoint file, and returns a fresh snapshot.
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
    let mut cpu_guard = cpu_state.0.lock().unwrap();
    let cpu = cpu_guard.as_mut().ok_or("CPU not ready")?;
    let table = cpu.bus().symbol_table().clone();
    let mut compiler = WatchCompiler::new(map_register_name, map_flag_name, move |name| {
        table.address_for(name).map(|a| a as u32)
    });
    let watchpoint = compiler.compile(&source, &mut watch.evaluator).map_err(|e| e.to_string())?;
    watch.evaluator.add(watchpoint);
    watch.enabled.push(true);
    sync_cpu_evaluator(cpu, &watch.evaluator, &watch.enabled)?;
    save_watchpoints(&watch.evaluator, &watch.enabled)?;
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
    watch.enabled.remove(index);
    let mut cpu_guard = cpu_state.0.lock().unwrap();
    if let Some(cpu) = cpu_guard.as_mut() {
        sync_cpu_evaluator(cpu, &watch.evaluator, &watch.enabled)?;
    }
    save_watchpoints(&watch.evaluator, &watch.enabled)?;
    Ok(build_snapshot(cpu_guard.as_ref(), &mut watch))
}

/// Recompiles `sources` in order against `symbol_table`, in a fresh
/// evaluator, used to validate an edited watchpoint and, on success, replace
/// the evaluator that backs the panel.
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

/// Replaces the expression at `index` with `source`, persists the updated
/// watchpoint file, and returns a fresh snapshot.
///
/// Fails without modifying state if the edited source fails to compile, or
/// if the loaded file already carries an unresolved whole-file compile
/// error. The watchpoint's enabled state is left unchanged.
#[tauri::command]
pub fn edit_watchpoint(
    index: usize,
    source: String,
    cpu_state: State<CpuState>,
    watch_state: State<WatchState>,
) -> Result<WatchpointsSnapshot, String> {
    let mut watch = watch_state.0.lock().unwrap();
    if watch.compile_error.is_some() {
        return Err("watchpoints.emw has a compile error; fix it before editing watchpoints".to_string());
    }
    if index >= watch.evaluator.watchpoints().len() {
        return Err("Invalid watchpoint index".to_string());
    }
    let mut cpu_guard = cpu_state.0.lock().unwrap();
    let cpu = cpu_guard.as_mut().ok_or("CPU not ready")?;
    let mut sources: Vec<String> = watch.evaluator.watchpoints().iter().map(|wp| wp.source().to_string()).collect();
    sources[index] = source;
    watch.evaluator = recompile_sources(&sources, cpu.bus().symbol_table())?;
    sync_cpu_evaluator(cpu, &watch.evaluator, &watch.enabled)?;
    save_watchpoints(&watch.evaluator, &watch.enabled)?;
    Ok(build_snapshot(Some(cpu), &mut watch))
}

/// Toggles the enabled state of the watchpoint at `index`, persists the
/// updated state, and returns a fresh snapshot.
///
/// A disabled watchpoint is removed from `cpu`'s own evaluator so it can
/// never halt execution, but remains in the panel's list, grayed, with a
/// neutral status until re-enabled.
#[tauri::command]
pub fn toggle_watchpoint(
    index: usize,
    cpu_state: State<CpuState>,
    watch_state: State<WatchState>,
) -> Result<WatchpointsSnapshot, String> {
    let mut watch = watch_state.0.lock().unwrap();
    if watch.compile_error.is_some() {
        return Err("watchpoints.emw has a compile error; fix it before editing watchpoints".to_string());
    }
    let Some(enabled) = watch.enabled.get_mut(index) else {
        return Err("Invalid watchpoint index".to_string());
    };
    *enabled = !*enabled;
    let mut cpu_guard = cpu_state.0.lock().unwrap();
    if let Some(cpu) = cpu_guard.as_mut() {
        sync_cpu_evaluator(cpu, &watch.evaluator, &watch.enabled)?;
    }
    save_watchpoints(&watch.evaluator, &watch.enabled)?;
    Ok(build_snapshot(cpu_guard.as_ref(), &mut watch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use emma65::emulator::cpu::StepResult;
    use emma65::emulator::{AddressRange, Bus, CpuVariant};

    /// Builds a CPU with 64KB RAM and a reset vector pointing to `start`.
    fn make_cpu(start: u16) -> Cpu {
        let mut bus = Bus::config().ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0).unwrap().build();
        bus.write(0xFFFC, (start & 0xFF) as u8).unwrap();
        bus.write(0xFFFD, (start >> 8) as u8).unwrap();
        let mut cpu = Cpu::builder(CpuVariant::Wdc65C02).bus(bus).build().unwrap();
        cpu.reset().unwrap();
        cpu
    }

    #[test]
    fn sync_cpu_evaluator_installs_watchpoints_that_halt_execution() {
        // Regression test for issue #246: a watchpoint added via the panel
        // (only ever landing in the display-only `WatchState` evaluator) must
        // also reach `cpu.evaluator()`, since that's the one `Cpu::step()`
        // consults to decide whether to halt.
        let mut cpu = make_cpu(0x0200);
        cpu.bus_mut().write(0x0200, 0xEA).unwrap(); // NOP
        cpu.bus_mut().write(0x0201, 0xA2).unwrap(); // LDX #$FF
        cpu.bus_mut().write(0x0202, 0xFF).unwrap();
        cpu.bus_mut().write(0x0203, 0xEA).unwrap(); // NOP

        let display_evaluator = evaluator_with(&["X == $FF"]);
        sync_cpu_evaluator(&mut cpu, &display_evaluator, &[true]).unwrap();

        assert!(!cpu.evaluator().is_empty());

        let mut result = StepResult::Waiting;
        for _ in 0..3 {
            result = cpu.step(None, true);
            if matches!(result, StepResult::WatchTriggered { .. }) {
                break;
            }
        }
        assert!(
            matches!(result, StepResult::WatchTriggered { pc: 0x0203, .. }),
            "expected watchpoint to halt execution at 0x0203"
        );
    }

    #[test]
    fn sync_cpu_evaluator_clears_stale_watchpoints() {
        let mut cpu = make_cpu(0x0200);
        sync_cpu_evaluator(&mut cpu, &evaluator_with(&["A == 0"]), &[true]).unwrap();
        assert!(!cpu.evaluator().is_empty());

        sync_cpu_evaluator(&mut cpu, &WatchEvaluator::new(), &[]).unwrap();
        assert!(cpu.evaluator().is_empty());
    }

    #[test]
    fn sync_cpu_evaluator_omits_disabled_watchpoints() {
        // A disabled watchpoint must never reach cpu.evaluator(), since that's
        // the one Cpu::step() consults to decide whether to halt.
        let mut cpu = make_cpu(0x0200);
        let evaluator = evaluator_with(&["A == 0", "X == 0"]);
        sync_cpu_evaluator(&mut cpu, &evaluator, &[true, false]).unwrap();
        assert_eq!(cpu.evaluator().watchpoints().len(), 1);
        assert_eq!(cpu.evaluator().watchpoints()[0].source(), "A == 0");
    }

    #[test]
    fn disabled_watchpoint_does_not_halt_execution() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus_mut().write(0x0200, 0xA2).unwrap(); // LDX #$FF
        cpu.bus_mut().write(0x0201, 0xFF).unwrap();
        cpu.bus_mut().write(0x0202, 0xEA).unwrap(); // NOP
        cpu.bus_mut().write(0x0203, 0xEA).unwrap(); // NOP

        let evaluator = evaluator_with(&["X == $FF"]);
        sync_cpu_evaluator(&mut cpu, &evaluator, &[false]).unwrap();
        assert!(cpu.evaluator().is_empty());

        let mut halted = false;
        for _ in 0..3 {
            if matches!(cpu.step(None, true), StepResult::WatchTriggered { .. }) {
                halted = true;
                break;
            }
        }
        assert!(!halted, "a disabled watchpoint must not halt execution even though its expression is true");
    }

    #[test]
    fn build_snapshot_includes_variable_values_from_this_evaluation_cycle() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus_mut().write(0x0200, 0xA2).unwrap(); // LDX #$2A
        cpu.bus_mut().write(0x0201, 0x2A).unwrap();
        cpu.step(None, true);

        let mut watch = WatchData { evaluator: evaluator_with(&["x := X"]), compile_error: None, enabled: vec![true] };
        let snapshot = build_snapshot(Some(&cpu), &mut watch);
        assert_eq!(snapshot.variables.len(), 1);
        assert_eq!(snapshot.variables[0].name, "x");
        assert_eq!(snapshot.variables[0].value, 0x2A);
    }

    #[test]
    fn build_snapshot_omits_variables_on_compile_error() {
        let mut watch = WatchData { evaluator: WatchEvaluator::new(), compile_error: Some("bad".to_string()), enabled: Vec::new() };
        let snapshot = build_snapshot(None, &mut watch);
        assert!(snapshot.variables.is_empty());
    }

    #[test]
    fn build_snapshot_reports_variables_even_when_cpu_is_not_ready() {
        let mut watch = WatchData { evaluator: evaluator_with(&["x := A"]), compile_error: None, enabled: vec![true] };
        let snapshot = build_snapshot(None, &mut watch);
        assert!(snapshot.rows.is_empty());
        assert_eq!(snapshot.variables.len(), 1);
        assert_eq!(snapshot.variables[0].name, "x");
    }

    #[test]
    fn build_snapshot_omits_variable_after_only_assigning_watchpoint_removed() {
        // Regression test for issue #252: a variable introduced by a walrus
        // assignment must disappear from the panel's variables section once
        // the watchpoint that assigned it is removed, not linger with its
        // last value.
        let mut watch = WatchData { evaluator: evaluator_with(&["x := A"]), compile_error: None, enabled: vec![true] };
        watch.evaluator.remove(0);
        watch.enabled.remove(0);
        let snapshot = build_snapshot(None, &mut watch);
        assert!(snapshot.variables.is_empty());
    }

    #[test]
    fn recompile_sources_replaces_one_source_and_preserves_order() {
        let sources = vec!["A == 0".to_string(), "X == 1".to_string(), "Y == 2".to_string()];
        let evaluator = recompile_sources(&sources, &SymbolTable::new()).unwrap();
        let got: Vec<&str> = evaluator.watchpoints().iter().map(|wp| wp.source()).collect();
        assert_eq!(got, sources);
    }

    #[test]
    fn recompile_sources_fails_on_invalid_expression() {
        let sources = vec!["A == 0".to_string(), "@@@".to_string()];
        assert!(recompile_sources(&sources, &SymbolTable::new()).is_err());
    }

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
        save_watchpoints_to(&dir, &evaluator, &[true, true]).unwrap();
        let contents = std::fs::read_to_string(dir.join("watchpoints.emw")).unwrap();
        assert_eq!(contents, "A == 0;\nX == 1;\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_watchpoints_writes_empty_file_when_no_watchpoints() {
        let dir = temp_dir("save-empty");
        save_watchpoints_to(&dir, &WatchEvaluator::new(), &[]).unwrap();
        let contents = std::fs::read_to_string(dir.join("watchpoints.emw")).unwrap();
        assert_eq!(contents, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_watchpoints_writes_enabled_state_as_one_flag_per_line() {
        let dir = temp_dir("save-enabled");
        let evaluator = evaluator_with(&["A == 0", "X == 1", "Y == 2"]);
        save_watchpoints_to(&dir, &evaluator, &[true, false, true]).unwrap();
        let contents = std::fs::read_to_string(dir.join("watchpoints.enabled")).unwrap();
        assert_eq!(contents, "1\n0\n1\n");
        let _ = std::fs::remove_dir_all(&dir);
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
    fn load_watchpoints_defaults_to_enabled_when_enabled_file_is_missing() {
        let dir = temp_dir("load-no-enabled-file");
        // Write only the .emw file, mirroring a watchpoints file saved before
        // enable/disable existed.
        std::fs::write(dir.join("watchpoints.emw"), "A == 0;\nX == 1;\n").unwrap();
        let (reloaded, enabled) = load_watchpoints_from(&dir, &SymbolTable::new()).unwrap();
        assert_eq!(reloaded.watchpoints().len(), 2);
        assert_eq!(enabled, vec![true, true]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_watchpoints_defaults_to_enabled_when_enabled_file_count_mismatches() {
        let dir = temp_dir("load-mismatched-enabled-count");
        std::fs::write(dir.join("watchpoints.emw"), "A == 0;\nX == 1;\n").unwrap();
        std::fs::write(dir.join("watchpoints.enabled"), "0\n").unwrap();
        let (reloaded, enabled) = load_watchpoints_from(&dir, &SymbolTable::new()).unwrap();
        assert_eq!(reloaded.watchpoints().len(), 2);
        assert_eq!(enabled, vec![true, true]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
