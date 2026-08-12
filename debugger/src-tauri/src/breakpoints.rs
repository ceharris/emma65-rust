//! Breakpoint panel: tracks the debugger's breakpoint set (address, enabled
//! state, resolved symbol label) and mirrors it into the CPU's own breakpoint
//! set. Shared source of truth for the Disassembly panel's gutter and the
//! standalone Breakpoints panel; every mutating command broadcasts
//! `breakpoints-changed` so both stay in sync.

use std::collections::BTreeMap;
use std::sync::Mutex;

use emma65::emulator::SymbolTable;
use tauri::{AppHandle, Emitter, State};

use crate::CpuState;

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

/// Toggles a breakpoint at `addr`: adds it (enabled) if not present, removes it entirely if present.
///
/// Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn toggle_breakpoint(
    addr: u16,
    app: AppHandle,
    cpu_state: State<CpuState>,
    breakpoint_state: State<BreakpointState>,
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
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    bps.insert(addr, true);
    cpu.add_breakpoint(addr);
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
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    bps.remove(&addr);
    cpu.remove_breakpoint(addr);
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
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    if let Some(enabled) = bps.get_mut(&addr) {
        *enabled = false;
        cpu.remove_breakpoint(addr);
    }
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
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    if let Some(enabled) = bps.get_mut(&addr) {
        *enabled = true;
        cpu.add_breakpoint(addr);
    }
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
