//! Breakpoint panel: tracks the debugger's breakpoint set (address, enabled
//! state, resolved symbol label) and mirrors it into the CPU's own breakpoint
//! set. Shared source of truth for the Disassembly panel's gutter and the
//! standalone Breakpoints panel; every mutating command broadcasts
//! `breakpoints-changed` so both stay in sync, and persists the breakpoint
//! set back to the active profile's `breakpoints.json`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use emma65::emulator::{Cpu, SymbolTable};
use tauri::{AppHandle, Emitter, State};

use crate::CpuState;
use crate::profile::ProfileDirState;

/// Debugger-tracked breakpoint records (`addr -> enabled`).
///
/// Source of truth for the UI's breakpoint list. Only currently-enabled addresses are
/// mirrored into the CPU's own breakpoint set via `Cpu::add_breakpoint`/`remove_breakpoint`,
/// so disabling one just stops it from halting execution without any "disabled" concept
/// in the emulator core itself.
pub struct BreakpointState(pub Mutex<BTreeMap<u16, bool>>);

/// A single breakpoint entry returned to the frontend.
#[derive(Clone, serde::Serialize)]
pub struct BreakpointInfo {
    /// Address the breakpoint is set at.
    pub addr: u16,
    /// True if the breakpoint currently halts execution.
    pub enabled: bool,
    /// Symbol name at `addr`, if the symbol table has one.
    pub label: Option<String>,
}

/// Converts the debugger's breakpoint records into the list returned to the frontend.
fn breakpoint_list(bps: &BTreeMap<u16, bool>, symbols: Option<&SymbolTable>) -> Vec<BreakpointInfo> {
    bps.iter()
        .map(|(&addr, &enabled)| {
            let label = symbols.and_then(|s| s.names_for(addr).next().map(String::from));
            BreakpointInfo { addr, enabled, label }
        })
        .collect()
}

/// Broadcasts `list` as a `breakpoints-changed` event, so the Disassembly
/// gutter and the standalone Breakpoints panel stay in sync regardless of
/// which one made the change.
fn emit_breakpoints_changed(app: &AppHandle, list: &[BreakpointInfo]) {
    let _ = app.emit("breakpoints-changed", list);
}

/// Loads `dir/breakpoints.json`: a JSON array of `[addr, enabled]` pairs. A
/// missing or unparseable file means no breakpoints, not an error — matching
/// how a missing `watchpoints.emw` is treated.
pub fn load_breakpoints_from(dir: &Path) -> BTreeMap<u16, bool> {
    let path = dir.join("breakpoints.json");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<Vec<(u16, bool)>>(&contents).map(|pairs| pairs.into_iter().collect()).unwrap_or_default()
}

/// Serializes `breakpoints` to `dir/breakpoints.json` as a JSON array of
/// `[addr, enabled]` pairs, ascending by address.
fn save_breakpoints_to(dir: &Path, breakpoints: &BTreeMap<u16, bool>) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create config directory: {e}"))?;
    let path = dir.join("breakpoints.json");
    let pairs: Vec<(u16, bool)> = breakpoints.iter().map(|(&addr, &enabled)| (addr, enabled)).collect();
    let contents = serde_json::to_string_pretty(&pairs).map_err(|e| e.to_string())?;
    std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

/// Installs `breakpoints`' enabled addresses into `cpu`'s own breakpoint set,
/// so a freshly loaded profile's breakpoints actually halt execution rather
/// than only appearing in the panel and gutter.
pub fn install_breakpoints(cpu: &mut Cpu, breakpoints: &BTreeMap<u16, bool>) {
    for (&addr, &enabled) in breakpoints {
        if enabled {
            cpu.add_breakpoint(addr);
        }
    }
}

/// Broadcasts `breakpoints-changed` for a freshly loaded profile's
/// `breakpoints`, resolving labels from `symbol_table`.
///
/// Used at session load (startup, New/Open Profile, Open Recent) instead of a
/// dedicated "profile changed" event, since the Breakpoints panel and the
/// Disassembly gutter already resync from this broadcast.
pub fn emit_loaded_breakpoints(app: &AppHandle, breakpoints: &BTreeMap<u16, bool>, symbol_table: &SymbolTable) {
    let list = breakpoint_list(breakpoints, Some(symbol_table));
    emit_breakpoints_changed(app, &list);
}

/// Toggles a breakpoint at `addr`: adds it (enabled) if not present, removes it entirely if present.
///
/// Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn toggle_breakpoint(
    addr: u16,
    app: AppHandle,
    cpu_state: State<CpuState>,
    breakpoint_state: State<BreakpointState>,
    profile_dir: State<ProfileDirState>,
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    if bps.remove(&addr).is_some() {
        cpu.remove_breakpoint(addr);
    } else {
        bps.insert(addr, true);
        cpu.add_breakpoint(addr);
    }
    save_breakpoints_to(&profile_dir.0.lock().unwrap().clone(), &bps)?;
    let list = breakpoint_list(&bps, Some(cpu.bus().symbol_table()));
    emit_breakpoints_changed(&app, &list);
    Ok(list)
}

/// Sets an enabled breakpoint at `addr`, re-enabling it if it already existed but was disabled.
///
/// Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn set_breakpoint(
    addr: u16,
    app: AppHandle,
    cpu_state: State<CpuState>,
    breakpoint_state: State<BreakpointState>,
    profile_dir: State<ProfileDirState>,
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    bps.insert(addr, true);
    cpu.add_breakpoint(addr);
    save_breakpoints_to(&profile_dir.0.lock().unwrap().clone(), &bps)?;
    let list = breakpoint_list(&bps, Some(cpu.bus().symbol_table()));
    emit_breakpoints_changed(&app, &list);
    Ok(list)
}

/// Removes the breakpoint at `addr` entirely, if any.
///
/// Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn remove_breakpoint(
    addr: u16,
    app: AppHandle,
    cpu_state: State<CpuState>,
    breakpoint_state: State<BreakpointState>,
    profile_dir: State<ProfileDirState>,
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    bps.remove(&addr);
    cpu.remove_breakpoint(addr);
    save_breakpoints_to(&profile_dir.0.lock().unwrap().clone(), &bps)?;
    let list = breakpoint_list(&bps, Some(cpu.bus().symbol_table()));
    emit_breakpoints_changed(&app, &list);
    Ok(list)
}

/// Disables the breakpoint at `addr` without removing it; execution no longer halts there.
///
/// No-op if there is no breakpoint at `addr`. Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn disable_breakpoint(
    addr: u16,
    app: AppHandle,
    cpu_state: State<CpuState>,
    breakpoint_state: State<BreakpointState>,
    profile_dir: State<ProfileDirState>,
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    if let Some(enabled) = bps.get_mut(&addr) {
        *enabled = false;
        cpu.remove_breakpoint(addr);
    }
    save_breakpoints_to(&profile_dir.0.lock().unwrap().clone(), &bps)?;
    let list = breakpoint_list(&bps, Some(cpu.bus().symbol_table()));
    emit_breakpoints_changed(&app, &list);
    Ok(list)
}

/// Re-enables a previously disabled breakpoint at `addr`.
///
/// No-op if there is no breakpoint at `addr`. Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn enable_breakpoint(
    addr: u16,
    app: AppHandle,
    cpu_state: State<CpuState>,
    breakpoint_state: State<BreakpointState>,
    profile_dir: State<ProfileDirState>,
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    if let Some(enabled) = bps.get_mut(&addr) {
        *enabled = true;
        cpu.add_breakpoint(addr);
    }
    save_breakpoints_to(&profile_dir.0.lock().unwrap().clone(), &bps)?;
    let list = breakpoint_list(&bps, Some(cpu.bus().symbol_table()));
    emit_breakpoints_changed(&app, &list);
    Ok(list)
}

/// Returns the debugger's tracked breakpoint list (including disabled ones), sorted ascending.
#[tauri::command]
pub fn get_breakpoints(cpu_state: State<CpuState>, breakpoint_state: State<BreakpointState>) -> Vec<BreakpointInfo> {
    let cpu_guard = cpu_state.0.lock().unwrap();
    let symbols = cpu_guard.as_ref().map(|cpu| cpu.bus().symbol_table());
    breakpoint_list(&breakpoint_state.0.lock().unwrap(), symbols)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a fresh, uniquely-named temp directory for one test's config files.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("emma65-breakpoints-test-{name}-{:?}", std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_breakpoints_returns_empty_when_file_missing() {
        let dir = temp_dir("load-missing");
        assert!(load_breakpoints_from(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_breakpoints_round_trips_through_load_breakpoints() {
        let dir = temp_dir("round-trip");
        let mut bps = BTreeMap::new();
        bps.insert(0x0200, true);
        bps.insert(0xC000, false);
        save_breakpoints_to(&dir, &bps).unwrap();
        let reloaded = load_breakpoints_from(&dir);
        assert_eq!(reloaded, bps);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_breakpoints_writes_an_empty_json_array_when_no_breakpoints() {
        let dir = temp_dir("save-empty");
        save_breakpoints_to(&dir, &BTreeMap::new()).unwrap();
        let contents = std::fs::read_to_string(dir.join("breakpoints.json")).unwrap();
        assert_eq!(contents, "[]");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_breakpoints_returns_empty_when_file_is_unparseable() {
        let dir = temp_dir("load-malformed");
        std::fs::write(dir.join("breakpoints.json"), "not json").unwrap();
        assert!(load_breakpoints_from(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_breakpoints_adds_only_enabled_addresses() {
        let mut bus = emma65::emulator::Bus::config()
            .ram_with_fill(emma65::emulator::AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        bus.write(0xFFFC, 0x00).unwrap();
        bus.write(0xFFFD, 0x02).unwrap();
        let mut cpu = Cpu::builder(emma65::emulator::CpuVariant::Wdc65C02).bus(bus).build().unwrap();
        cpu.reset().unwrap();

        let mut bps = BTreeMap::new();
        bps.insert(0x0200, true);
        bps.insert(0x0300, false);
        install_breakpoints(&mut cpu, &bps);

        assert!(cpu.breakpoints().contains(&0x0200));
        assert!(!cpu.breakpoints().contains(&0x0300));
    }
}
