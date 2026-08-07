//! Execution control: continue/pause/step/restart, wired to `emma65::emulator::exec`.
//!
//! Mirrors the Tauri debugger's `disassembly.rs`/`cpu_bus.rs` run/step/reset commands,
//! translated from Tauri commands + `debugger-halted`/`debugger-run-stopped` events to
//! DAP requests + a single `stopped` event per halt.

use std::io::Write;
use std::sync::atomic::AtomicU16;
use std::sync::{Arc, Mutex};

use dap::events::{Event, StoppedEventBody};
use dap::responses::ContinueResponse;
use dap::server::ServerOutput;
use dap::types::StoppedEventReason;
use emma65::emulator::cpu::StepResult;
use emma65::emulator::{
    Cpu, RunStopper, run_from as exec_run_from, step_into as exec_step_into,
    step_over_subroutine as exec_step_over_subroutine, step_return as exec_step_return,
};

/// The emulator has exactly one CPU thread, so every DAP thread-scoped request/event
/// uses this fixed id.
pub const THREAD_ID: i64 = 1;

/// Shared state for the CPU and the in-progress free-run/step, if any.
///
/// `cpu` is `None` while a background operation (`continue`/`next`/`stepOut`) owns
/// the CPU on its own thread; commands that need direct CPU access (`stepIn`,
/// `restart` while halted) fail with "CPU not ready" in that window, matching the
/// Tauri debugger's equivalent commands.
#[derive(Default)]
pub struct ExecState {
    cpu: Arc<Mutex<Option<Cpu>>>,
    run_stopper: Arc<Mutex<Option<RunStopper>>>,
    /// Set by `pause` just before signaling the stop; consumed by the background
    /// operation's completion handler to distinguish a user-requested pause from
    /// any other reason the run/step halted.
    pause_requested: Arc<Mutex<bool>>,
}

impl ExecState {
    /// Installs `cpu` as the current session, replacing any previous one.
    pub fn set_cpu(&self, cpu: Cpu) {
        *self.cpu.lock().unwrap() = Some(cpu);
    }

    /// Removes the current session, e.g. on `disconnect`/`terminate`.
    pub fn clear_cpu(&self) {
        *self.cpu.lock().unwrap() = None;
    }

    /// True once a session has been installed via `set_cpu`.
    pub fn has_cpu(&self) -> bool {
        self.cpu.lock().unwrap().is_some()
    }
}

/// Sends a `stopped` event for the CPU thread with the given `reason`.
pub fn send_stopped<W: Write>(output: &Arc<Mutex<ServerOutput<W>>>, reason: StoppedEventReason) {
    let _ = output.lock().unwrap().send_event(Event::Stopped(StoppedEventBody {
        reason,
        description: None,
        thread_id: Some(THREAD_ID),
        preserve_focus_hint: None,
        text: None,
        all_threads_stopped: Some(true),
        hit_breakpoint_ids: None,
    }));
}

/// Restores `cpu` after a background run/step completes and sends the `stopped` event.
///
/// `result` is `None` only when the operation was interrupted mid-flight (always by
/// `pause` in this story, since nothing else calls `RunStopper::stop`); a user-requested
/// pause always reports as `StoppedEventReason::Pause` regardless of `result`.
fn finish_run<W: Write>(
    state: &ExecState,
    output: &Arc<Mutex<ServerOutput<W>>>,
    cpu: Cpu,
    result: Option<StepResult>,
) {
    *state.run_stopper.lock().unwrap() = None;
    let pause_requested = std::mem::take(&mut *state.pause_requested.lock().unwrap());
    *state.cpu.lock().unwrap() = Some(cpu);

    let reason = if pause_requested {
        StoppedEventReason::Pause
    } else {
        match result {
            Some(StepResult::Reset) => StoppedEventReason::Entry,
            Some(StepResult::Breakpoint(_)) => StoppedEventReason::Breakpoint,
            _ => StoppedEventReason::Step,
        }
    };
    send_stopped(output, reason);
}

/// Starts free-run execution on a dedicated OS thread, mirroring the Tauri
/// debugger's `run_cpu` command.
///
/// Returns immediately with a `ContinueResponse`; the `stopped` event follows once
/// the run halts (breakpoint, watch trigger, error, STP/WAI stall, reset, or `pause`).
pub fn continue_cpu<W: Write + Send + 'static>(
    state: &ExecState,
    runtime: &Arc<tokio::runtime::Runtime>,
    output: Arc<Mutex<ServerOutput<W>>>,
) -> Result<ContinueResponse, String> {
    let cpu = state.cpu.lock().unwrap().take().ok_or("CPU not ready")?;
    let handle = exec_run_from(cpu, None, Arc::new(AtomicU16::new(0)));
    *state.run_stopper.lock().unwrap() = Some(handle.stopper());
    *state.pause_requested.lock().unwrap() = false;

    let cpu_arc = Arc::clone(&state.cpu);
    let stopper_arc = Arc::clone(&state.run_stopper);
    let pause_arc = Arc::clone(&state.pause_requested);
    let runtime = Arc::clone(runtime);
    std::thread::spawn(move || {
        let (result, cpu) = runtime.block_on(handle.take_cpu_with_result());
        let state = ExecState { cpu: cpu_arc, run_stopper: stopper_arc, pause_requested: pause_arc };
        finish_run(&state, &output, cpu, Some(result));
    });

    Ok(ContinueResponse { all_threads_continued: Some(true) })
}

/// Signals the free-running or stepping CPU thread to stop, mirroring the Tauri
/// debugger's `stop_cpu` command.
///
/// Non-blocking; the `stopped` event (with reason `pause`) follows once the
/// in-progress operation's completion handler observes the stop.
pub fn pause(state: &ExecState) -> Result<(), String> {
    let guard = state.run_stopper.lock().unwrap();
    let stopper = guard.as_ref().ok_or("CPU is not running")?;
    *state.pause_requested.lock().unwrap() = true;
    stopper.stop();
    Ok(())
}

/// Executes one logical step, treating JSR as atomic, on a dedicated OS thread,
/// mirroring the Tauri debugger's `step_over` command.
///
/// Returns immediately; the `stopped` event follows once the step (or the
/// subroutine it steps over) completes or is interrupted by `pause`.
pub fn step_over<W: Write + Send + 'static>(
    state: &ExecState,
    output: Arc<Mutex<ServerOutput<W>>>,
) -> Result<(), String> {
    let mut cpu = state.cpu.lock().unwrap().take().ok_or("CPU not ready")?;
    let (stopper, stop_rx, mut cmd_rx) = RunStopper::channel();
    *state.run_stopper.lock().unwrap() = Some(stopper);
    *state.pause_requested.lock().unwrap() = false;

    let cpu_arc = Arc::clone(&state.cpu);
    let stopper_arc = Arc::clone(&state.run_stopper);
    let pause_arc = Arc::clone(&state.pause_requested);
    std::thread::spawn(move || {
        let result =
            exec_step_over_subroutine(&mut cpu, &stop_rx, &mut cmd_rx, None, &Arc::new(AtomicU16::new(0)));
        let state = ExecState { cpu: cpu_arc, run_stopper: stopper_arc, pause_requested: pause_arc };
        finish_run(&state, &output, cpu, result);
    });

    Ok(())
}

/// Executes a single CPU instruction, mirroring the Tauri debugger's `step_into` command.
///
/// Synchronous. Returns the `stopped` reason for the caller to send after
/// acknowledging the request, so the response precedes the event on the wire.
pub fn step_into(state: &ExecState) -> Result<StoppedEventReason, String> {
    let mut guard = state.cpu.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    let result = exec_step_into(cpu);
    drop(guard);

    Ok(match result {
        StepResult::Reset => StoppedEventReason::Entry,
        StepResult::Breakpoint(_) => StoppedEventReason::Breakpoint,
        _ => StoppedEventReason::Step,
    })
}

/// Runs until the current subroutine returns, on a dedicated OS thread, mirroring
/// the Tauri debugger's `step_return` command.
///
/// Returns immediately; the `stopped` event follows once the subroutine returns or
/// the operation is interrupted by `pause`.
pub fn step_return<W: Write + Send + 'static>(
    state: &ExecState,
    output: Arc<Mutex<ServerOutput<W>>>,
) -> Result<(), String> {
    let mut cpu = state.cpu.lock().unwrap().take().ok_or("CPU not ready")?;
    let (stopper, stop_rx, mut cmd_rx) = RunStopper::channel();
    *state.run_stopper.lock().unwrap() = Some(stopper);
    *state.pause_requested.lock().unwrap() = false;

    let cpu_arc = Arc::clone(&state.cpu);
    let stopper_arc = Arc::clone(&state.run_stopper);
    let pause_arc = Arc::clone(&state.pause_requested);
    std::thread::spawn(move || {
        let result = exec_step_return(&mut cpu, &stop_rx, &mut cmd_rx, None, &Arc::new(AtomicU16::new(0)));
        let state = ExecState { cpu: cpu_arc, run_stopper: stopper_arc, pause_requested: pause_arc };
        finish_run(&state, &output, cpu, result);
    });

    Ok(())
}

/// Resets the CPU, mirroring the Tauri debugger's `reset_cpu` command.
///
/// While free-running or stepping, signals a RESET into the running CPU via
/// `RunStopper::trigger_reset` and returns `None`; the operation halts on its own
/// and its completion handler sends the `stopped` event (reason `entry`, like the
/// initial halt-at-reset-vector on `launch`). Otherwise resets directly and returns
/// `Some(StoppedEventReason::Entry)` for the caller to send after acknowledging the
/// request, so the response precedes the event on the wire.
pub fn restart(state: &ExecState) -> Result<Option<StoppedEventReason>, String> {
    let is_running = {
        let guard = state.run_stopper.lock().unwrap();
        if let Some(stopper) = guard.as_ref() {
            stopper.trigger_reset();
            true
        } else {
            false
        }
    };
    if is_running {
        return Ok(None);
    }

    let mut guard = state.cpu.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;
    cpu.reset().map_err(|e| format!("CPU reset failed: {e}"))?;
    drop(guard);

    Ok(Some(StoppedEventReason::Entry))
}
