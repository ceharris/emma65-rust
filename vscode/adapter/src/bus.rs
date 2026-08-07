//! NMI/IRQ and bus-signal-snapshot commands, mirroring the Tauri debugger's
//! `cpu_bus.rs`, but reached through DAP's `evaluate` request rather than a
//! native command.
//!
//! `dap = "0.4.1-alpha1"`'s `Command`/`Event`/`ResponseBody` types are closed
//! enums with no support for genuinely custom request names (see
//! `session.rs`'s `TERMINAL_SOCKET_SPEC` doc comment for the full story-8
//! finding that established this). `evaluate` is the one standard request
//! whose arguments have an escape hatch: `EvaluateArguments::context` is an
//! `EvaluateArgumentsContext`, which has an untagged `String` fallback
//! variant alongside its known values (`variables`/`watch`/`repl`/`hover`/
//! `clipboard`). The extension calls `session.customRequest('evaluate', {
//! expression: '', context: <one of the constants below> })`; `handle_evaluate`
//! recognizes those context strings and dispatches to the matching bus/interrupt
//! operation, falling through to `None` (unsupported) for anything else so a real
//! `evaluate` (e.g. from a Debug Console, if one is ever wired up) isn't silently
//! misrouted.
//!
//! Unlike Tauri's always-open CPU/Bus panel, these are one-shot command-palette
//! actions, so there's no live-updating cache to maintain: `get_bus_state` (and the
//! post-action snapshot returned by the other three) reads directly from the CPU
//! when it's halted, and reports [`CpuBusState::running`] (fields unknown, since a
//! background run/step thread owns the CPU) while free-running.

use dap::requests::EvaluateArguments;
use dap::responses::EvaluateResponse;
use dap::types::EvaluateArgumentsContext;
use emma65::emulator::Cpu;

use crate::exec::ExecState;

const TRIGGER_NMI: &str = "emma65.triggerNmi";
const ASSERT_IRQ: &str = "emma65.assertIrq";
const RELEASE_IRQ: &str = "emma65.releaseIrq";
const GET_BUS_STATE: &str = "emma65.getBusState";

/// Snapshot of interrupt-controller and cycle-count state, or `is_running: true` with
/// every other field unknown if a background run/step currently owns the CPU.
struct CpuBusState {
    is_running: bool,
    irq_active: Option<bool>,
    nmi_pending: Option<bool>,
    cycles: Option<u64>,
    cpu_stopped: Option<bool>,
    cpu_waiting: Option<bool>,
}

impl CpuBusState {
    fn running() -> Self {
        CpuBusState {
            is_running: true,
            irq_active: None,
            nmi_pending: None,
            cycles: None,
            cpu_stopped: None,
            cpu_waiting: None,
        }
    }

    fn from_cpu(cpu: &Cpu) -> Self {
        CpuBusState {
            is_running: false,
            irq_active: Some(cpu.interrupts().irq_active()),
            nmi_pending: Some(cpu.interrupts().nmi_pending()),
            cycles: Some(cpu.cycles()),
            cpu_stopped: Some(cpu.is_stopped()),
            cpu_waiting: Some(cpu.is_waiting()),
        }
    }

    /// Renders as the single-line summary DAP's `evaluate` result is meant to carry,
    /// shown to the user via whatever the extension does with it (an information
    /// message, in story 9's command implementations).
    fn describe(&self) -> String {
        if self.is_running {
            return "CPU running (bus signals unavailable while free-running)".to_string();
        }
        format!(
            "IRQ active: {} | NMI pending: {} | Cycles: {} | Stopped: {} | Waiting: {}",
            fmt_bool(self.irq_active),
            fmt_bool(self.nmi_pending),
            self.cycles.map(|c| c.to_string()).unwrap_or_else(|| "?".to_string()),
            fmt_bool(self.cpu_stopped),
            fmt_bool(self.cpu_waiting),
        )
    }
}

fn fmt_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "?",
    }
}

/// Dispatches a recognized `emma65.*` `evaluate` context to its bus/interrupt
/// operation. Returns `None` if `args.context` isn't one of the constants above, so
/// the caller can respond "unsupported request" the same way it does for any other
/// unrecognized command.
pub fn handle_evaluate(state: &ExecState, args: &EvaluateArguments) -> Option<Result<EvaluateResponse, String>> {
    let context = match &args.context {
        Some(EvaluateArgumentsContext::String(context)) => context.as_str(),
        _ => return None,
    };
    let result = match context {
        TRIGGER_NMI => trigger_nmi(state),
        ASSERT_IRQ => assert_irq(state),
        RELEASE_IRQ => release_irq(state),
        GET_BUS_STATE => get_bus_state(state),
        _ => return None,
    };
    Some(result.map(|bus_state| EvaluateResponse { result: bus_state.describe(), ..Default::default() }))
}

/// Latches a pending NMI, mirroring the Tauri debugger's `trigger_nmi` command.
fn trigger_nmi(state: &ExecState) -> Result<CpuBusState, String> {
    if state.with_run_stopper(|stopper| stopper.trigger_nmi()).is_some() {
        return Ok(CpuBusState::running());
    }
    state
        .with_cpu_mut(|cpu| {
            cpu.interrupts_mut().signal_nmi();
            CpuBusState::from_cpu(cpu)
        })
        .ok_or_else(|| "CPU not ready".to_string())
}

/// Asserts the IRQ line from the extension's `IrqSource`, mirroring the Tauri
/// debugger's `assert_irq` command.
///
/// While free-running, uses `RunStopper::assert_irq_pulse` (a one-shot pulse that
/// auto-releases once serviced) rather than a sustained level — the same fix Tauri's
/// `assert_irq` applies (see #261) to avoid re-entering an ISR on every instruction
/// after `RTI` until the extension gets around to calling `releaseIrq`.
fn assert_irq(state: &ExecState) -> Result<CpuBusState, String> {
    let source = state.ui_irq_source().ok_or("CPU not ready")?;
    if state.with_run_stopper(|stopper| stopper.assert_irq_pulse(source)).is_some() {
        return Ok(CpuBusState::running());
    }
    state
        .with_cpu_mut(|cpu| {
            cpu.interrupts_mut().assert_irq(source);
            CpuBusState::from_cpu(cpu)
        })
        .ok_or_else(|| "CPU not ready".to_string())
}

/// Releases the IRQ line from the extension's `IrqSource`, mirroring the Tauri
/// debugger's `release_irq` command.
fn release_irq(state: &ExecState) -> Result<CpuBusState, String> {
    let source = state.ui_irq_source().ok_or("CPU not ready")?;
    if state.with_run_stopper(|stopper| stopper.release_irq(source)).is_some() {
        return Ok(CpuBusState::running());
    }
    state
        .with_cpu_mut(|cpu| {
            cpu.interrupts_mut().release_irq(source);
            CpuBusState::from_cpu(cpu)
        })
        .ok_or_else(|| "CPU not ready".to_string())
}

/// Returns the current bus/interrupt signal snapshot, mirroring the Tauri debugger's
/// `get_cpu_bus_state` command.
fn get_bus_state(state: &ExecState) -> Result<CpuBusState, String> {
    match state.with_cpu(CpuBusState::from_cpu) {
        Some(snapshot) => Ok(snapshot),
        None if state.is_running() => Ok(CpuBusState::running()),
        None => Err("CPU not ready".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emma65::emulator::{AddressRange, Bus, CpuVariant, RunStopper};

    fn make_cpu() -> Cpu {
        let bus = Bus::config().ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0).unwrap().build();
        Cpu::builder(CpuVariant::Wdc65C02).bus(bus).build().unwrap()
    }

    fn evaluate(context: &str) -> EvaluateArguments {
        EvaluateArguments {
            context: Some(EvaluateArgumentsContext::String(context.to_string())),
            ..Default::default()
        }
    }

    #[test]
    fn handle_evaluate_returns_none_for_an_unrecognized_context() {
        let state = ExecState::default();
        assert!(handle_evaluate(&state, &evaluate("something.else")).is_none());
    }

    #[test]
    fn handle_evaluate_returns_none_when_context_is_absent() {
        let state = ExecState::default();
        let args = EvaluateArguments { context: None, ..Default::default() };
        assert!(handle_evaluate(&state, &args).is_none());
    }

    #[test]
    fn get_bus_state_reports_cpu_not_ready_before_launch() {
        let state = ExecState::default();
        let result = handle_evaluate(&state, &evaluate(GET_BUS_STATE)).unwrap();
        assert_eq!(result.unwrap_err(), "CPU not ready");
    }

    #[test]
    fn get_bus_state_reports_running_while_a_background_run_owns_the_cpu() {
        // `cpu` stays `None` here, matching the real state during a background
        // run/step: the CPU is taken out of `ExecState` for the duration.
        let state = ExecState::default();
        let (stopper, _stop_rx, _cmd_rx) = RunStopper::channel();
        state.install_run_stopper_for_test(stopper);

        let result = handle_evaluate(&state, &evaluate(GET_BUS_STATE)).unwrap().unwrap();
        assert_eq!(result.result, "CPU running (bus signals unavailable while free-running)");
    }

    #[test]
    fn get_bus_state_reports_the_snapshot_while_halted() {
        let state = ExecState::default();
        state.set_cpu(make_cpu());

        let result = handle_evaluate(&state, &evaluate(GET_BUS_STATE)).unwrap().unwrap();
        assert_eq!(result.result, "IRQ active: no | NMI pending: no | Cycles: 0 | Stopped: no | Waiting: no");
    }

    #[test]
    fn trigger_nmi_latches_a_pending_nmi() {
        let state = ExecState::default();
        state.set_cpu(make_cpu());

        let result = handle_evaluate(&state, &evaluate(TRIGGER_NMI)).unwrap().unwrap();
        assert!(result.result.contains("NMI pending: yes"));
    }

    #[test]
    fn assert_irq_reports_cpu_not_ready_before_launch() {
        let state = ExecState::default();
        let result = handle_evaluate(&state, &evaluate(ASSERT_IRQ)).unwrap();
        assert_eq!(result.unwrap_err(), "CPU not ready");
    }

    #[test]
    fn assert_then_release_irq_round_trips_through_the_cpu() {
        let state = ExecState::default();
        state.set_cpu(make_cpu());
        state.set_ui_irq_source(emma65::emulator::IrqSource(1));

        let asserted = handle_evaluate(&state, &evaluate(ASSERT_IRQ)).unwrap().unwrap();
        assert!(asserted.result.contains("IRQ active: yes"));

        let released = handle_evaluate(&state, &evaluate(RELEASE_IRQ)).unwrap().unwrap();
        assert!(released.result.contains("IRQ active: no"));
    }
}
