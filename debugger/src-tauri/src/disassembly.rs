//! Disassembly panel: run/step/stop controls, breakpoints, and disassembly listing.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use emma65::emulator::{Cpu, CpuLiveSnapshot, Disassembler, RunStopper, StepResult, run_from as exec_run_from, step_into as exec_step_into, step_over_breakpoint as exec_step_over_breakpoint, step_over_subroutine as exec_step_over_subroutine, step_return as exec_step_return};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::cpu_bus::{CpuBusCache, snapshot_cpu_bus};
use crate::memory::MemoryViewAddr;
use crate::registers::{ChangedFlagsState, RegisterSnapshot};
use crate::CpuState;

/// Interval between `debugger-running-tick` events emitted during free-run.
const RUNNING_TICK_INTERVAL_MS: u64 = 100;

/// Holds the disassembler once the session is ready.
pub struct DisassemblerState(pub Mutex<Option<Disassembler>>);

/// Holds the stopper handle while the CPU is free-running; `None` when halted.
pub struct RunStopperState(pub Mutex<Option<RunStopper>>);

/// When the CPU is halted at a breakpoint or watch trigger, holds that PC so
/// the next step command can skip past it. Cleared after each step or reset.
pub struct SkipBreakpointPc(pub Mutex<Option<u16>>);

/// Live CPU snapshot stream published by the run loop during free-run.
///
/// Set when `run_cpu` starts; cleared when the run completes. Commands that
/// need CPU state while running read from this instead of `CpuState`.
pub struct LiveSnapshotRx(pub Mutex<Option<tokio::sync::watch::Receiver<Option<CpuLiveSnapshot>>>>);

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
}

/// Converts the debugger's breakpoint records into the list returned to the frontend.
fn breakpoint_list(bps: &BTreeMap<u16, bool>) -> Vec<BreakpointInfo> {
    bps.iter().map(|(&addr, &enabled)| BreakpointInfo { addr, enabled }).collect()
}

/// A single disassembled line returned to the frontend.
#[derive(Clone, serde::Serialize)]
pub struct DisassembledRow {
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
    /// Comment string
    pub comment: String,
    /// False for invalid opcodes under the active variant.
    pub is_valid: bool,
}

/// Executes a single CPU instruction and returns the updated register snapshot.
///
/// Emits `debugger-halted` with the new PC after the step completes.
#[tauri::command]
pub fn step_into(
    app: AppHandle,
    cpu_state: State<CpuState>,
    changed_flags_state: State<ChangedFlagsState>,
    cpu_bus_cache: State<CpuBusCache>,
    skip_breakpoint_pc: State<SkipBreakpointPc>,
) -> Result<RegisterSnapshot, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;

    let p_before = cpu.registers().p.to_byte();
    let pc = cpu.registers().pc;
    // Skip the breakpoint/watch check only if we are halted at that PC because
    // of a prior breakpoint or watch trigger — not on every step.
    let skip_pc = skip_breakpoint_pc.0.lock().unwrap().take();
    let result = if skip_pc == Some(pc) {
        exec_step_over_breakpoint(cpu, pc)
    } else {
        exec_step_into(cpu)
    };
    let regs = *cpu.registers();
    let changed = p_before ^ regs.p.to_byte();

    *changed_flags_state.0.lock().unwrap() = changed;
    *cpu_bus_cache.0.lock().unwrap() = snapshot_cpu_bus(cpu);

    let cpu_stopped = matches!(result, StepResult::Stopped);
    let cpu_waiting = matches!(result, StepResult::Waiting);
    let breakpoint_hit = matches!(result, StepResult::Breakpoint(_));
    let watch_triggered = matches!(result, StepResult::WatchTriggered { .. } | StepResult::WatchError { .. });

    // Record the halted PC if a breakpoint or watch triggered, so the next
    // step_into call knows to skip the check there.
    *skip_breakpoint_pc.0.lock().unwrap() = if breakpoint_hit || watch_triggered {
        Some(regs.pc)
    } else {
        None
    };

    let snapshot = RegisterSnapshot {
        a: regs.a,
        x: regs.x,
        y: regs.y,
        s: regs.s,
        pc: regs.pc,
        p: regs.p.to_byte(),
        changed_flags: changed,
        cpu_stopped,
        cpu_waiting,
        breakpoint_hit,
    };

    let _ = app.emit("debugger-halted", regs.pc);
    Ok(snapshot)
}

/// Toggles a breakpoint at `addr`: adds it (enabled) if not present, removes it entirely if present.
///
/// Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn toggle_breakpoint(
    addr: u16,
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
    Ok(breakpoint_list(&bps))
}

/// Sets an enabled breakpoint at `addr`, re-enabling it if it already existed but was disabled.
///
/// Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn set_breakpoint(
    addr: u16,
    cpu_state: State<CpuState>,
    breakpoint_state: State<BreakpointState>,
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    bps.insert(addr, true);
    cpu.add_breakpoint(addr);
    Ok(breakpoint_list(&bps))
}

/// Removes the breakpoint at `addr` entirely, if any.
///
/// Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn remove_breakpoint(
    addr: u16,
    cpu_state: State<CpuState>,
    breakpoint_state: State<BreakpointState>,
) -> Result<Vec<BreakpointInfo>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let mut bps = breakpoint_state.0.lock().unwrap();
    bps.remove(&addr);
    cpu.remove_breakpoint(addr);
    Ok(breakpoint_list(&bps))
}

/// Disables the breakpoint at `addr` without removing it; execution no longer halts there.
///
/// No-op if there is no breakpoint at `addr`. Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn disable_breakpoint(
    addr: u16,
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
    Ok(breakpoint_list(&bps))
}

/// Re-enables a previously disabled breakpoint at `addr`.
///
/// No-op if there is no breakpoint at `addr`. Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
pub fn enable_breakpoint(
    addr: u16,
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
    Ok(breakpoint_list(&bps))
}

/// Returns the debugger's tracked breakpoint list (including disabled ones), sorted ascending.
#[tauri::command]
pub fn get_breakpoints(breakpoint_state: State<BreakpointState>) -> Vec<BreakpointInfo> {
    breakpoint_list(&breakpoint_state.0.lock().unwrap())
}

/// Returns disassembled instructions starting at `addr`, up to `count` rows.
#[tauri::command]
pub fn get_disassembly(
    addr: u16,
    count: usize,
    cpu_state: State<CpuState>,
    disasm_state: State<DisassemblerState>,
) -> Result<Vec<DisassembledRow>, String> {
    let cpu_guard = cpu_state.0.lock().unwrap();
    let cpu = cpu_guard.as_ref().ok_or("CPU not ready")?;
    let disasm_guard = disasm_state.0.lock().unwrap();
    let disasm = disasm_guard.as_ref().ok_or("Disassembler not ready")?;

    let lines = disasm.disassemble_range(cpu.bus(), addr, 0, count);
    let rows = lines.into_iter().map(|line| DisassembledRow {
        addr: line.addr,
        bytes: line.raw_bytes.iter().map(|b| format!("{b:02X}")).collect(),
        labels: line.labels,
        mnemonic: line.mnemonic.to_string(),
        operand: line.operand_text,
        comment: line.comment_text.unwrap_or("".to_string()),
        is_valid: line.is_valid,
    }).collect();

    Ok(rows)
}

/// Spawns a task that emits `debugger-running-tick` every `RUNNING_TICK_INTERVAL_MS`
/// while `RunStopperState` is set, so panels can refresh live state during any
/// free-running mode (Run, Step Over, Step Return).
///
/// Stops ticking once `RunStopperState` is cleared, which every free-run
/// command does via `finish_run` (or, for `run_cpu`, its completion task).
fn spawn_running_tick(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(RUNNING_TICK_INTERVAL_MS)).await;
            if app.state::<RunStopperState>().0.lock().unwrap().is_none() {
                break;
            }
            let _ = app.emit("debugger-running-tick", ());
        }
    });
}

/// Starts free-run execution on a dedicated OS thread.
///
/// Takes the CPU out of `CpuState` and passes it to `exec::run_from`. Spawns a
/// background task that awaits the run completing, then restores the CPU to
/// `CpuState`, emits `debugger-halted` with the final PC, and emits
/// `debugger-run-stopped` with a full register snapshot.
#[tauri::command]
pub fn run_cpu(
    app: AppHandle,
    cpu_state: State<CpuState>,
    run_stopper_state: State<RunStopperState>,
    skip_breakpoint_pc: State<SkipBreakpointPc>,
    mem_view_addr: State<MemoryViewAddr>,
) -> Result<(), String> {
    let cpu = cpu_state.0.lock().unwrap().take().ok_or("CPU not ready")?;
    let skip_pc = skip_breakpoint_pc.0.lock().unwrap().take();
    let handle = exec_run_from(cpu, skip_pc, Arc::clone(&mem_view_addr.0));
    let stopper = handle.stopper();
    *run_stopper_state.0.lock().unwrap() = Some(stopper);
    *app.state::<LiveSnapshotRx>().0.lock().unwrap() =
        Some(handle.subscribe_live());

    spawn_running_tick(app.clone());

    tauri::async_runtime::spawn(async move {
        let (result, cpu) = handle.take_cpu_with_result().await;
        let pc = cpu.registers().pc;
        let result = Some(result);
        let (cpu_stopped, cpu_waiting, breakpoint_hit, skip_pc) = flags_from_result(&result, pc);
        finish_run(&app, cpu, 0, cpu_stopped, cpu_waiting, breakpoint_hit, skip_pc);
    });

    Ok(())
}

/// Signals the free-running CPU thread to stop.
///
/// Non-blocking. The background task spawned by `run_cpu` handles CPU recovery
/// and emits `debugger-run-stopped` when the thread exits.
#[tauri::command]
pub fn stop_cpu(run_stopper_state: State<RunStopperState>) -> Result<(), String> {
    let guard = run_stopper_state.0.lock().unwrap();
    let stopper = guard.as_ref().ok_or("CPU is not running")?;
    stopper.stop();
    Ok(())
}

/// Executes one step treating JSR as atomic, then emits the result as an event.
///
/// Returns immediately; the blocking work runs on a dedicated thread. The Stop
/// button (via `stop_cpu`) can interrupt the operation mid-subroutine. Emits
/// `debugger-run-stopped` with the final snapshot when done.
#[tauri::command]
pub fn step_over(
    app: AppHandle,
    cpu_state: State<CpuState>,
    run_stopper_state: State<RunStopperState>,
    skip_breakpoint_pc: State<SkipBreakpointPc>,
    mem_view_addr: State<MemoryViewAddr>,
) -> Result<(), String> {
    // Consume the skip state; exec_step_over handles the skip internally.
    skip_breakpoint_pc.0.lock().unwrap().take();
    let cpu = cpu_state.0.lock().unwrap().take().ok_or("CPU not ready")?;
    let p_before = cpu.registers().p.to_byte();
    let (stopper, stop_rx) = RunStopper::channel();
    *run_stopper_state.0.lock().unwrap() = Some(stopper);

    let (live_tx, live_rx) = tokio::sync::watch::channel(None);
    *app.state::<LiveSnapshotRx>().0.lock().unwrap() = Some(live_rx);
    spawn_running_tick(app.clone());

    let addr_arc = Arc::clone(&mem_view_addr.0);
    std::thread::spawn(move || {
        let mut cpu = cpu;
        let result = exec_step_over_subroutine(&mut cpu, &stop_rx, Some(&live_tx), &addr_arc);
        let pc = cpu.registers().pc;
        let changed = p_before ^ cpu.registers().p.to_byte();
        let (cpu_stopped, cpu_waiting, breakpoint_hit, skip_pc) = flags_from_result(&result, pc);
        finish_run(&app, cpu, changed, cpu_stopped, cpu_waiting, breakpoint_hit, skip_pc);
    });

    Ok(())
}

/// Runs until the current subroutine returns, then emits the result as an event.
///
/// Returns immediately; the blocking work runs on a dedicated thread. The Stop
/// button (via `stop_cpu`) can interrupt the operation before the return. Emits
/// `debugger-run-stopped` with the final snapshot when done.
#[tauri::command]
pub fn step_return(
    app: AppHandle,
    cpu_state: State<CpuState>,
    run_stopper_state: State<RunStopperState>,
    skip_breakpoint_pc: State<SkipBreakpointPc>,
    mem_view_addr: State<MemoryViewAddr>,
) -> Result<(), String> {
    // Consume the skip state; exec_step_return handles the skip internally.
    skip_breakpoint_pc.0.lock().unwrap().take();
    let cpu = cpu_state.0.lock().unwrap().take().ok_or("CPU not ready")?;
    let p_before = cpu.registers().p.to_byte();
    let (stopper, stop_rx) = RunStopper::channel();
    *run_stopper_state.0.lock().unwrap() = Some(stopper);

    let (live_tx, live_rx) = tokio::sync::watch::channel(None);
    *app.state::<LiveSnapshotRx>().0.lock().unwrap() = Some(live_rx);
    spawn_running_tick(app.clone());

    let addr_arc = Arc::clone(&mem_view_addr.0);
    std::thread::spawn(move || {
        let mut cpu = cpu;
        let result = exec_step_return(&mut cpu, &stop_rx, Some(&live_tx), &addr_arc);
        let pc = cpu.registers().pc;
        let changed = p_before ^ cpu.registers().p.to_byte();
        let (cpu_stopped, cpu_waiting, breakpoint_hit, skip_pc) = flags_from_result(&result, pc);
        finish_run(&app, cpu, changed, cpu_stopped, cpu_waiting, breakpoint_hit, skip_pc);
    });

    Ok(())
}

/// Extracts the execution-result flags from an optional `StepResult`.
///
/// Returns `(cpu_stopped, cpu_waiting, breakpoint_hit, skip_pc)` where
/// `skip_pc` is `Some(pc)` when a breakpoint or watch triggered at `pc`.
fn flags_from_result(result: &Option<StepResult>, pc: u16) -> (bool, bool, bool, Option<u16>) {
    let (cpu_stopped, cpu_waiting, breakpoint_hit) = match result {
        Some(r) => (
            matches!(r, StepResult::Stopped),
            matches!(r, StepResult::Waiting),
            matches!(r, StepResult::Breakpoint(_)),
        ),
        None => (false, false, false),
    };
    let watch_triggered = matches!(
        result,
        Some(StepResult::WatchTriggered { .. } | StepResult::WatchError { .. })
    );
    let skip_pc = if breakpoint_hit || watch_triggered { Some(pc) } else { None };
    (cpu_stopped, cpu_waiting, breakpoint_hit, skip_pc)
}

/// Restores CPU state after a threaded run completes and emits the halt events.
///
/// Writes `changed_flags`, the CPU-bus cache, clears the run-stopper, records the
/// skip-breakpoint PC if applicable, restores the CPU into `CpuState`, then emits
/// `debugger-halted` and `debugger-run-stopped`.
fn finish_run(
    app: &AppHandle,
    cpu: Cpu,
    changed_flags: u8,
    cpu_stopped: bool,
    cpu_waiting: bool,
    breakpoint_hit: bool,
    skip_pc: Option<u16>,
) {
    let regs = *cpu.registers();
    *app.state::<ChangedFlagsState>().0.lock().unwrap() = changed_flags;
    *app.state::<CpuBusCache>().0.lock().unwrap() = snapshot_cpu_bus(&cpu);
    *app.state::<RunStopperState>().0.lock().unwrap() = None;
    *app.state::<SkipBreakpointPc>().0.lock().unwrap() = skip_pc;
    *app.state::<LiveSnapshotRx>().0.lock().unwrap() = None;
    *app.state::<CpuState>().0.lock().unwrap() = Some(cpu);

    let snapshot = RegisterSnapshot {
        a: regs.a, x: regs.x, y: regs.y, s: regs.s,
        pc: regs.pc, p: regs.p.to_byte(),
        changed_flags,
        cpu_stopped,
        cpu_waiting,
        breakpoint_hit,
    };
    let _ = app.emit("debugger-halted", regs.pc);
    let _ = app.emit("debugger-run-stopped", snapshot);
}
