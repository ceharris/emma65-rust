use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;

use figment::{Figment, providers::{Format, Toml, Env}};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_log::{Target, TargetKind};
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;

use emma65::emulator::{
    run_from as exec_run_from,
    step_over as exec_step_over, step_return as exec_step_return,
    Config, Cpu, CpuLiveSnapshot, DeviceRegistry, Disassembler, EmulatorSession,
    InstantiationContext, IrqSource, PipeTransport, RunStopper, StatusRegister, StepResult,
    TransportSlot,
};

/// Debugger UI theme selection: persisted preference and Tauri commands.
mod theme;

const TERMINAL_WINDOW_LABEL: &str = "terminal";

/// IRQ source identifying the debugger UI's own IRQ toggle control.
///
/// Chosen outside the address range any real device's `IrqSource` can take
/// (`DeviceId`-derived sources are always `<= 0xFFFF`), so it never collides
/// with — or gets silently cleared by — `InterruptController::poll_devices`.
const UI_IRQ_SOURCE: IrqSource = IrqSource(u32::MAX);

/// Interval between `debugger-running-tick` events emitted during free-run.
const RUNNING_TICK_INTERVAL_MS: u64 = 100;

/// Holds the tx end of the remote pipe so `write_terminal` can send bytes to the console.
pub struct TerminalTx(pub Mutex<File>);

/// One-shot sender signalling that the terminal window is ready to receive output.
pub struct TerminalReadyTx(pub Mutex<Option<oneshot::Sender<()>>>);

/// Holds the CPU once the session is ready.
pub struct CpuState(pub Mutex<Option<Cpu>>);

/// Holds the disassembler once the session is ready.
pub struct DisassemblerState(pub Mutex<Option<Disassembler>>);

/// Bitmask of P-register bits that changed on the most recent step.
///
/// Reset to 0 on session start; updated by `step_into` and read by `get_registers`.
pub struct ChangedFlagsState(pub Mutex<u8>);

/// Payload emitted to the frontend on the `session-status` event.
#[derive(Clone, serde::Serialize)]
pub struct SessionStatus {
    /// Human-readable status message.
    pub message: String,
    /// True if the session was constructed successfully.
    pub ok: bool,
}

/// Holds the last emitted session status so late-connecting frontends can retrieve it.
pub struct SessionStatusState(pub Mutex<Option<SessionStatus>>);

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

/// Cached CPU/bus state (IRQ, NMI, cycle count) for use when the CPU is free-running.
///
/// Updated every time the CPU is available: after each step, reset, or run completion.
pub struct CpuBusCache(pub Mutex<CpuBusSnapshot>);

/// Snapshot of CPU/bus signals and cycle counter.
#[derive(Clone, serde::Serialize)]
pub struct CpuBusSnapshot {
    /// True if any device is currently asserting IRQ.
    pub irq_active: bool,
    /// True if an NMI is pending (latched but not yet serviced).
    pub nmi_pending: bool,
    /// Total CPU cycles executed since the last reset.
    pub cycles: u64,
    /// True when the CPU executed STP and is halted until reset.
    pub cpu_stopped: bool,
    /// True when the CPU executed WAI and is waiting for an interrupt.
    pub cpu_waiting: bool,
}

/// Combined CPU/bus state returned by `get_cpu_bus_state`.
#[derive(Clone, serde::Serialize)]
pub struct CpuBusState {
    /// True if any device is currently asserting IRQ.
    pub irq_active: bool,
    /// True if an NMI is pending (latched but not yet serviced).
    pub nmi_pending: bool,
    /// Total CPU cycles executed since the last reset.
    pub cycles: u64,
    /// True while the CPU is free-running (run_cpu, step_over, or step_return in progress).
    pub is_running: bool,
    /// True when the CPU executed STP and is halted until reset.
    pub cpu_stopped: bool,
    /// True when the CPU executed WAI and is waiting for an interrupt.
    pub cpu_waiting: bool,
}

/// A single disassembled line returned to the frontend.
#[derive(Clone, serde::Serialize)]
pub struct DisassembledRow {
    /// Instruction address.
    pub addr: u16,
    /// Raw bytes as hex strings, e.g. ["4C", "00", "06"].
    pub bytes: Vec<String>,
    /// Mnemonic string, e.g. "JMP".
    pub mnemonic: String,
    /// Formatted operand text, e.g. "$0600".
    pub operand: String,
    /// False for invalid opcodes under the active variant.
    pub is_valid: bool,
}

/// Register snapshot returned to the frontend.
#[derive(Clone, serde::Serialize)]
pub struct RegisterSnapshot {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub s: u8,
    pub pc: u16,
    /// Processor status byte.
    pub p: u8,
    /// Bitmask of P-register bits that changed on the most recent step (0 on initial load).
    pub changed_flags: u8,
    /// True when the CPU executed STP and is now halted; auto-step should stop.
    pub cpu_stopped: bool,
    /// True when the CPU executed WAI and is waiting for an interrupt.
    pub cpu_waiting: bool,
    /// True when the post-step PC matches a breakpoint address; auto-step should stop.
    pub breakpoint_hit: bool,
}

/// Loads emulator config from `~/.emma/debugger/default/emulator.toml`,
/// builds the session with an injected pipe transport for the console,
/// and returns the session along with the remote end of the pipe.
async fn load_session() -> Result<(EmulatorSession, PipeTransport), String> {
    let config_path = theme::debugger_config_dir()?.join("emulator.toml");

    let config: Config = Figment::new()
        .merge(Toml::file(&config_path))
        .merge(Env::prefixed("EMMA65_").map(|k| k.as_str().replace('_', "-").into()))
        .extract()
        .map_err(|e| format!("Configuration error: {e}"))?;

    let (local, remote) = PipeTransport::pair()
        .map_err(|e| format!("Failed to create console transport: {e}"))?;

    let transport_slot: TransportSlot = std::sync::Arc::new(Mutex::new(Some(Box::new(local))));
    let context = InstantiationContext {
        clock_hz: config.clock_speed_hz,
        error_sender: None,
        console_transport: Some(transport_slot),
    };

    let registry = DeviceRegistry::with_builtins();
    let session = config.build_with_context(&registry, context).await
        .map_err(|e| format!("Failed to build emulator session: {e}"))?;

    Ok((session, remote))
}

/// Tokio task that reads bytes from the remote pipe rx and emits `terminal-output` events.
async fn run_terminal_bridge(rx: File, app: AppHandle) {
    let async_rx = match AsyncFd::new(rx) {
        Ok(fd) => fd,
        Err(e) => { eprintln!("terminal bridge: AsyncFd::new failed: {e}"); return; }
    };
    let mut buf = [0u8; 256];
    loop {
        let mut guard = match async_rx.readable().await {
            Ok(g) => g,
            Err(_) => break,
        };
        match guard.try_io(|fd| fd.get_ref().read(&mut buf)) {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                let bytes: Vec<u8> = buf[..n].to_vec();
                let _ = app.emit_to(TERMINAL_WINDOW_LABEL, "terminal-output", bytes);
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Ok(Err(_)) => break,
            Err(_would_block) => continue,
        }
    }
}

/// Tauri command: called by the terminal window once its event listener is registered.
#[tauri::command]
fn terminal_ready(state: State<TerminalReadyTx>) {
    if let Some(tx) = state.0.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

/// Tauri command: send bytes typed in the terminal to the emulated console.
#[tauri::command]
fn write_terminal(bytes: Vec<u8>, state: State<TerminalTx>) -> Result<(), String> {
    let mut tx = state.0.lock().unwrap();
    tx.write_all(&bytes).map_err(|e| e.to_string())
}

/// Exits the application cleanly.
#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

/// Returns the current session status, or `None` if not yet determined.
#[tauri::command]
fn get_session_status(state: State<SessionStatusState>) -> Option<SessionStatus> {
    state.0.lock().unwrap().clone()
}

/// Executes a single CPU instruction and returns the updated register snapshot.
///
/// Emits `debugger-halted` with the new PC after the step completes.
#[tauri::command]
fn step_into(
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
        cpu.step_over_breakpoint(pc)
    } else {
        cpu.step()
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

/// Resets the CPU (reads reset vector, reinitializes registers) and returns the
/// post-reset register snapshot.
///
/// Resets `ChangedFlagsState` to 0 and emits `debugger-halted` with the new PC,
/// then emits `debugger-cpu-reset` so the frontend can stop auto-step if active.
#[tauri::command]
fn reset_cpu(
    app: AppHandle,
    cpu_state: State<CpuState>,
    changed_flags_state: State<ChangedFlagsState>,
    cpu_bus_cache: State<CpuBusCache>,
    skip_breakpoint_pc: State<SkipBreakpointPc>,
) -> Result<RegisterSnapshot, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;

    cpu.reset().map_err(|e| e.to_string())?;
    // Clear any NMI/IRQ state the debugger UI itself introduced, so the
    // NMI/IRQ trigger controls stay in sync with a freshly reset CPU.
    cpu.interrupts_mut().release_irq(UI_IRQ_SOURCE);
    cpu.interrupts_mut().take_nmi();
    let regs = *cpu.registers();

    *changed_flags_state.0.lock().unwrap() = 0;
    *cpu_bus_cache.0.lock().unwrap() = snapshot_cpu_bus(cpu);
    *skip_breakpoint_pc.0.lock().unwrap() = None;

    let snapshot = RegisterSnapshot {
        a: regs.a,
        x: regs.x,
        y: regs.y,
        s: regs.s,
        pc: regs.pc,
        p: regs.p.to_byte(),
        changed_flags: 0,
        cpu_stopped: false,
        cpu_waiting: false,
        breakpoint_hit: false,
    };

    let _ = app.emit("debugger-halted", regs.pc);
    let _ = app.emit("debugger-cpu-reset", ());
    Ok(snapshot)
}

/// Identifies which CPU register a `set_register` call targets.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum RegisterField {
    A,
    X,
    Y,
    S,
    Pc,
    P,
}

/// Validates that `value` fits in a `u8`, for the byte-sized register fields.
fn single_byte(value: u32, field: RegisterField) -> Result<u8, String> {
    value.try_into().map_err(|_| format!("{field:?} value out of range: must be 0-255"))
}

/// Sets a single CPU register to `value`, interpreted per `field`'s width.
///
/// Only callable while the CPU is stopped (not free-running). Emits
/// `debugger-halted` with the (possibly unchanged) PC so the disassembly view
/// re-centers and the stack view refreshes, covering PC/S edits.
#[tauri::command]
fn set_register(
    app: AppHandle,
    field: RegisterField,
    value: u32,
    cpu_state: State<CpuState>,
    changed_flags_state: State<ChangedFlagsState>,
    cpu_bus_cache: State<CpuBusCache>,
) -> Result<RegisterSnapshot, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;

    let p_before = cpu.registers().p.to_byte();

    match field {
        RegisterField::A => cpu.registers_mut().a = single_byte(value, field)?,
        RegisterField::X => cpu.registers_mut().x = single_byte(value, field)?,
        RegisterField::Y => cpu.registers_mut().y = single_byte(value, field)?,
        RegisterField::S => cpu.registers_mut().s = single_byte(value, field)?,
        RegisterField::P => {
            cpu.registers_mut().p = StatusRegister::from_byte(single_byte(value, field)?) | StatusRegister::UNUSED;
        }
        RegisterField::Pc => {
            cpu.registers_mut().pc = value.try_into().map_err(|_| "Pc value out of range: must be 0-65535".to_string())?;
        }
    }

    let regs = *cpu.registers();
    let changed = p_before ^ regs.p.to_byte();
    *changed_flags_state.0.lock().unwrap() = changed;
    *cpu_bus_cache.0.lock().unwrap() = snapshot_cpu_bus(cpu);

    let snapshot = RegisterSnapshot {
        a: regs.a,
        x: regs.x,
        y: regs.y,
        s: regs.s,
        pc: regs.pc,
        p: regs.p.to_byte(),
        changed_flags: changed,
        cpu_stopped: cpu.is_stopped(),
        cpu_waiting: cpu.is_waiting(),
        breakpoint_hit: false,
    };

    let _ = app.emit("debugger-halted", regs.pc);
    Ok(snapshot)
}

/// Latches a pending NMI. Only callable while the CPU is stopped (not free-running).
#[tauri::command]
fn trigger_nmi(
    cpu_state: State<CpuState>,
    cpu_bus_cache: State<CpuBusCache>,
) -> Result<CpuBusState, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    cpu.interrupts_mut().signal_nmi();
    Ok(refresh_cpu_bus_cache(cpu, &cpu_bus_cache))
}

/// Asserts the IRQ line from the debugger UI's own IRQ source. Only callable
/// while the CPU is stopped (not free-running).
#[tauri::command]
fn assert_irq(
    cpu_state: State<CpuState>,
    cpu_bus_cache: State<CpuBusCache>,
) -> Result<CpuBusState, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    cpu.interrupts_mut().assert_irq(UI_IRQ_SOURCE);
    Ok(refresh_cpu_bus_cache(cpu, &cpu_bus_cache))
}

/// Releases the IRQ line from the debugger UI's own IRQ source. Only callable
/// while the CPU is stopped (not free-running).
#[tauri::command]
fn release_irq(
    cpu_state: State<CpuState>,
    cpu_bus_cache: State<CpuBusCache>,
) -> Result<CpuBusState, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    cpu.interrupts_mut().release_irq(UI_IRQ_SOURCE);
    Ok(refresh_cpu_bus_cache(cpu, &cpu_bus_cache))
}

/// Refreshes `CpuBusCache` from `cpu` and returns the corresponding `CpuBusState`.
///
/// `is_running` is always `false` here: `CpuState` only holds `Some(Cpu)` while
/// the CPU is not free-running (it's taken by the run loop otherwise).
fn refresh_cpu_bus_cache(cpu: &Cpu, cpu_bus_cache: &State<CpuBusCache>) -> CpuBusState {
    let snap = snapshot_cpu_bus(cpu);
    *cpu_bus_cache.0.lock().unwrap() = snap.clone();
    CpuBusState {
        irq_active: snap.irq_active,
        nmi_pending: snap.nmi_pending,
        cycles: snap.cycles,
        is_running: false,
        cpu_stopped: snap.cpu_stopped,
        cpu_waiting: snap.cpu_waiting,
    }
}

/// Returns a register snapshot of the current CPU state without stepping.
///
/// Falls back to the live snapshot channel when the CPU is free-running
/// (i.e. `CpuState` is `None`). `changed_flags` is 0 during free-run.
#[tauri::command]
fn get_registers(
    cpu_state: State<CpuState>,
    changed_flags_state: State<ChangedFlagsState>,
    live_snapshot_rx: State<LiveSnapshotRx>,
) -> Result<RegisterSnapshot, String> {
    let guard = cpu_state.0.lock().unwrap();
    if let Some(cpu) = guard.as_ref() {
        let regs = cpu.registers();
        let changed_flags = *changed_flags_state.0.lock().unwrap();
        return Ok(RegisterSnapshot {
            a: regs.a,
            x: regs.x,
            y: regs.y,
            s: regs.s,
            pc: regs.pc,
            p: regs.p.to_byte(),
            changed_flags,
            cpu_stopped: cpu.is_stopped(),
            cpu_waiting: cpu.is_waiting(),
            breakpoint_hit: false,
        });
    }
    // CPU is free-running — read from the live snapshot channel.
    let live = live_snapshot_rx.0.lock().unwrap()
        .as_ref()
        .and_then(|rx| rx.borrow().clone())
        .ok_or("CPU not ready")?;
    let regs = &live.registers;
    Ok(RegisterSnapshot {
        a: regs.a,
        x: regs.x,
        y: regs.y,
        s: regs.s,
        pc: regs.pc,
        p: regs.p.to_byte(),
        changed_flags: 0,
        cpu_stopped: false,
        cpu_waiting: false,
        breakpoint_hit: false,
    })
}

/// Stack snapshot returned to the frontend.
///
/// Covers the full stack page so the frontend can render any window within it.
#[derive(Clone, serde::Serialize)]
pub struct StackSnapshot {
    /// Current stack pointer (0x00–0xFF, page 1 offset).
    pub s: u8,
    /// All 256 bytes of the stack page (0x0100–0x01FF).
    pub page: Vec<u8>,
}

/// Returns the current stack pointer and the full stack page (0x0100–0x01FF).
///
/// Falls back to the live snapshot channel when the CPU is free-running.
/// Reads are performed via `Bus::peek_range` so no device side effects occur.
#[tauri::command]
fn get_stack(
    cpu_state: State<CpuState>,
    live_snapshot_rx: State<LiveSnapshotRx>,
) -> Result<StackSnapshot, String> {
    let guard = cpu_state.0.lock().unwrap();
    if let Some(cpu) = guard.as_ref() {
        let s = cpu.registers().s;
        let mut page = vec![0u8; 256];
        cpu.bus().peek_range(0x0100, &mut page).map_err(|e| e.to_string())?;
        return Ok(StackSnapshot { s, page });
    }
    // CPU is free-running — read from the live snapshot channel.
    let live = live_snapshot_rx.0.lock().unwrap()
        .as_ref()
        .and_then(|rx| rx.borrow().clone())
        .ok_or("CPU not ready")?;
    Ok(StackSnapshot { s: live.registers.s, page: live.stack_page })
}

/// Toggles a breakpoint at `addr` on the CPU: adds it if not present, removes it if present.
///
/// Returns the updated breakpoint list, sorted ascending.
#[tauri::command]
fn toggle_breakpoint(addr: u16, cpu_state: State<CpuState>) -> Result<Vec<u16>, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    if !cpu.remove_breakpoint(addr) {
        cpu.add_breakpoint(addr);
    }
    let mut list: Vec<u16> = cpu.breakpoints().iter().copied().collect();
    list.sort_unstable();
    Ok(list)
}

/// Returns the CPU's current breakpoint address list, sorted ascending.
#[tauri::command]
fn get_breakpoints(cpu_state: State<CpuState>) -> Result<Vec<u16>, String> {
    let guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_ref().ok_or("CPU not ready")?;
    let mut list: Vec<u16> = cpu.breakpoints().iter().copied().collect();
    list.sort_unstable();
    Ok(list)
}

/// Returns 256 bytes of memory starting at `addr` (address AND'ed with 0xfff0 for paragraph alignment).
///
/// Reads are performed via `Bus::peek_range` so no device side effects occur.
#[tauri::command]
fn get_memory(addr: u16, cpu_state: State<CpuState>) -> Result<Vec<u8>, String> {
    let guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_ref().ok_or("CPU not ready")?;
    let page_start = addr & 0xfff0;
    let mut buf = vec![0u8; 256];
    cpu.bus()
        .peek_range(page_start, &mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Returns disassembled instructions starting at `addr`, up to `count` rows.
#[tauri::command]
fn get_disassembly(
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
        mnemonic: line.mnemonic.to_string(),
        operand: line.operand_text,
        is_valid: line.is_valid,
    }).collect();

    Ok(rows)
}

/// Starts free-run execution on a dedicated OS thread.
///
/// Takes the CPU out of `CpuState` and passes it to `exec::run_from`. Spawns a
/// background task that awaits the run completing, then restores the CPU to
/// `CpuState`, emits `debugger-halted` with the final PC, and emits
/// `debugger-run-stopped` with a full register snapshot.
#[tauri::command]
fn run_cpu(
    app: AppHandle,
    cpu_state: State<CpuState>,
    run_stopper_state: State<RunStopperState>,
    skip_breakpoint_pc: State<SkipBreakpointPc>,
) -> Result<(), String> {
    let cpu = cpu_state.0.lock().unwrap().take().ok_or("CPU not ready")?;
    let skip_pc = skip_breakpoint_pc.0.lock().unwrap().take();
    let handle = exec_run_from(cpu, skip_pc);
    let stopper = handle.stopper();
    *run_stopper_state.0.lock().unwrap() = Some(stopper);
    *app.state::<LiveSnapshotRx>().0.lock().unwrap() =
        Some(handle.subscribe_live());

    // Periodic refresh: emit debugger-running-tick every 500ms while the CPU
    // is free-running so all panels can update their display.
    let tick_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(RUNNING_TICK_INTERVAL_MS)).await;
            // Stop ticking once RunStopperState is cleared (run completed).
            if tick_app.state::<RunStopperState>().0.lock().unwrap().is_none() {
                break;
            }
            let _ = tick_app.emit("debugger-running-tick", ());
        }
    });

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
fn stop_cpu(run_stopper_state: State<RunStopperState>) -> Result<(), String> {
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
fn step_over(
    app: AppHandle,
    cpu_state: State<CpuState>,
    run_stopper_state: State<RunStopperState>,
    skip_breakpoint_pc: State<SkipBreakpointPc>,
) -> Result<(), String> {
    // Consume the skip state; exec_step_over handles the skip internally.
    skip_breakpoint_pc.0.lock().unwrap().take();
    let cpu = cpu_state.0.lock().unwrap().take().ok_or("CPU not ready")?;
    let p_before = cpu.registers().p.to_byte();
    let (stopper, stop_rx) = RunStopper::channel();
    *run_stopper_state.0.lock().unwrap() = Some(stopper);

    std::thread::spawn(move || {
        let mut cpu = cpu;
        let result = exec_step_over(&mut cpu, &stop_rx);
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
fn step_return(
    app: AppHandle,
    cpu_state: State<CpuState>,
    run_stopper_state: State<RunStopperState>,
    skip_breakpoint_pc: State<SkipBreakpointPc>,
) -> Result<(), String> {
    // Consume the skip state; exec_step_return handles the skip internally.
    skip_breakpoint_pc.0.lock().unwrap().take();
    let cpu = cpu_state.0.lock().unwrap().take().ok_or("CPU not ready")?;
    let p_before = cpu.registers().p.to_byte();
    let (stopper, stop_rx) = RunStopper::channel();
    *run_stopper_state.0.lock().unwrap() = Some(stopper);

    std::thread::spawn(move || {
        let mut cpu = cpu;
        let result = exec_step_return(&mut cpu, &stop_rx);
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

/// Snapshots the interrupt controller state and cycle count from a live CPU.
fn snapshot_cpu_bus(cpu: &Cpu) -> CpuBusSnapshot {
    CpuBusSnapshot {
        irq_active: cpu.interrupts().irq_active(),
        nmi_pending: cpu.interrupts().nmi_pending(),
        cycles: cpu.cycles(),
        cpu_stopped: cpu.is_stopped(),
        cpu_waiting: cpu.is_waiting(),
    }
}

/// Returns the current CPU/bus signals and cycle count, plus whether the CPU is free-running.
///
/// IRQ, NMI, and cpu_stopped/waiting values come from the cache updated after each step or run
/// completion. Cycles are read from the live snapshot channel during free-run so the counter
/// updates at the tick rate rather than only at halt.
#[tauri::command]
fn get_cpu_bus_state(
    cpu_bus_cache: State<CpuBusCache>,
    run_stopper_state: State<RunStopperState>,
    live_snapshot_rx: State<LiveSnapshotRx>,
) -> CpuBusState {
    let snap = cpu_bus_cache.0.lock().unwrap().clone();
    let is_running = run_stopper_state.0.lock().unwrap().is_some();
    let cycles = if is_running {
        live_snapshot_rx.0.lock().unwrap()
            .as_ref()
            .and_then(|rx| rx.borrow().as_ref().map(|s| s.cycles))
            .unwrap_or(snap.cycles)
    } else {
        snap.cycles
    };
    CpuBusState {
        irq_active: snap.irq_active,
        nmi_pending: snap.nmi_pending,
        cycles,
        is_running,
        cpu_stopped: snap.cpu_stopped,
        cpu_waiting: snap.cpu_waiting,
    }
}

fn emit_status(app: &AppHandle, status: SessionStatus) {
    app.state::<SessionStatusState>().0.lock().unwrap().replace(status.clone());
    let _ = app.emit("session-status", status);
}

/// Toggles the terminal window's visibility. Bound to Ctrl+Shift+` in both the
/// main and terminal windows (see `useAppKeyBindings.ts`), so the frontend
/// doesn't need to track visibility state itself.
#[tauri::command]
fn toggle_terminal_visibility(app: AppHandle) -> Result<(), String> {
    let window = app.get_webview_window(TERMINAL_WINDOW_LABEL)
        .ok_or_else(|| "terminal window not found".to_string())?;
    let visible = window.is_visible().map_err(|e| e.to_string())?;
    if visible { window.hide() } else { window.show() }.map_err(|e| e.to_string())
}

/// Shows the terminal window (created hidden at startup, per `tauri.conf.json`).
///
/// On the webkit2gtk backend, a window's webview doesn't realize — and its JS
/// never runs — until the window is actually mapped, so this must happen
/// before awaiting the terminal's ready handshake. The window can still be
/// hidden again afterward via `toggle_terminal_visibility`.
fn show_terminal_window(app: &AppHandle) -> Result<(), String> {
    app.get_webview_window(TERMINAL_WINDOW_LABEL)
        .ok_or_else(|| "terminal window not found".to_string())?
        .show()
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (ready_tx, ready_rx) = oneshot::channel::<()>();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::LogDir { file_name: None }),
                    Target::new(TargetKind::Webview),
                ])
                .build(),
        )
        .manage(SessionStatusState(Mutex::new(None)))
        .manage(TerminalReadyTx(Mutex::new(Some(ready_tx))))
        // TerminalTx is registered after setup; commands are only called after the
        // terminal window is open, so it will always be present by then.
        .manage(CpuState(Mutex::new(None)))
        .manage(DisassemblerState(Mutex::new(None)))
        .manage(ChangedFlagsState(Mutex::new(0)))
        .manage(RunStopperState(Mutex::new(None)))
        .manage(SkipBreakpointPc(Mutex::new(None)))
        .manage(LiveSnapshotRx(Mutex::new(None)))
        .manage(CpuBusCache(Mutex::new(CpuBusSnapshot {
            irq_active: false,
            nmi_pending: false,
            cycles: 0,
            cpu_stopped: false,
            cpu_waiting: false,
        })))
        .manage(theme::UiConfigState(Mutex::new(theme::load_ui_config())))
        .invoke_handler(tauri::generate_handler![
            quit,
            toggle_terminal_visibility,
            get_session_status,
            write_terminal,
            terminal_ready,
            run_cpu,
            stop_cpu,
            step_into,
            step_over,
            step_return,
            reset_cpu,
            set_register,
            trigger_nmi,
            assert_irq,
            release_irq,
            get_registers,
            get_disassembly,
            get_memory,
            get_stack,
            toggle_breakpoint,
            get_breakpoints,
            get_cpu_bus_state,
            theme::get_theme,
            theme::set_theme,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match load_session().await {
                    Ok((session, remote)) => {
                        let (remote_rx, remote_tx) = remote.into_split();

                        // Register the tx side so write_terminal can use it.
                        handle.manage(TerminalTx(Mutex::new(remote_tx)));

                        let mut cpu = session.cpu;
                        let variant = cpu.variant();

                        if let Err(e) = cpu.reset() {
                            emit_status(&handle, SessionStatus {
                                message: format!("CPU reset failed: {e}"),
                                ok: false,
                            });
                            return;
                        }

                        let initial_pc = cpu.registers().pc;
                        let disasm = Disassembler::new(variant);
                        *handle.state::<DisassemblerState>().0.lock().unwrap() = Some(disasm);
                        *handle.state::<CpuBusCache>().0.lock().unwrap() = snapshot_cpu_bus(&cpu);
                        *handle.state::<CpuState>().0.lock().unwrap() = Some(cpu);

                        emit_status(&handle, SessionStatus {
                            message: "Emulator session ready".to_string(),
                            ok: true,
                        });

                        // Show the terminal window (created hidden at startup) so its
                        // webview realizes and runs; the user can hide it again afterward
                        // with Ctrl+Shift+` (see `toggle_terminal_visibility`).
                        if let Err(e) = show_terminal_window(&handle) {
                            eprintln!("Failed to show terminal window: {e}");
                            return;
                        }

                        // Wait for the terminal window to signal it is ready.
                        let _ = ready_rx.await;

                        // Start the terminal bridge.
                        let bridge_handle = handle.clone();
                        tauri::async_runtime::spawn(async move {
                            run_terminal_bridge(remote_rx, bridge_handle).await;
                        });

                        // Emit the initial halted state so the frontend can render the
                        // disassembly view immediately on first load.
                        let _ = handle.emit("debugger-halted", initial_pc);
                    }
                    Err(message) => {
                        emit_status(&handle, SessionStatus { message, ok: false });
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
