import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ExecState } from "./DisassemblyPanel";
import { RegisterSnapshot } from "./RegisterPanel";
import "./styles/cpu-bus.scss";

/** Minimum time in ms that the Step indicator remains visible so users can perceive it. */
const STEP_INDICATOR_MIN_MS = 75;

interface CpuBusState {
  irq_active: boolean;
  nmi_pending: boolean;
  cycles: number;
  is_running: boolean;
  cpu_stopped: boolean;
  cpu_waiting: boolean;
}

interface Props {
  /** Current CPU execution state, derived from DisassemblyPanel. */
  execState: ExecState;
  /** Called with the post-reset register snapshot so other panels can update. */
  onReset: (snap: RegisterSnapshot) => void;
}

/** Formats a number with comma thousands separators. */
function formatCycles(n: number): string {
  return n.toLocaleString();
}

export default function CpuBusPanel({ execState, onReset }: Props) {
  const [cpuBus, setCpuBus] = useState<CpuBusState | null>(null);

  // Display state for the Run/Stop/Step indicator. When stepping, hold the
  // "stepping" state for at least STEP_INDICATOR_MIN_MS so the transition is
  // perceptible before snapping back to "stopped".
  const [displayExecState, setDisplayExecState] = useState<ExecState>(execState);
  const stepTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const execStateRef = useRef<ExecState>(execState);
  execStateRef.current = execState;

  // Restart the step flash: show "stepping" immediately and schedule a return
  // to "stopped" after the hold duration. Called on each debugger-halted event
  // while in stepping mode so every auto-step tick produces a visible flash.
  const triggerStepFlash = useCallback(() => {
    setDisplayExecState("stepping");
    if (stepTimerRef.current !== null) clearTimeout(stepTimerRef.current);
    stepTimerRef.current = setTimeout(() => {
      stepTimerRef.current = null;
      // Only snap back to stopped if we're not still in a stepping/running state.
      if (execStateRef.current !== "running") {
        setDisplayExecState("stopped");
      }
    }, STEP_INDICATOR_MIN_MS);
  }, []);

  useEffect(() => {
    if (execState === "stepping") {
      // Each transition into "stepping" triggers a flash. During auto-step the
      // debugger-halted listener (below) drives a fresh flash on every tick.
      triggerStepFlash();
    } else if (execState === "running") {
      // Cancel any pending step hold and go to "running" immediately.
      if (stepTimerRef.current !== null) {
        clearTimeout(stepTimerRef.current);
        stepTimerRef.current = null;
      }
      setDisplayExecState("running");
    } else {
      // Transition to stopped: if a step-hold timer is still pending, let it
      // expire naturally so the blue flash remains visible for the full hold duration.
      if (stepTimerRef.current === null) {
        setDisplayExecState("stopped");
      }
    }
  }, [execState, triggerStepFlash]);

  // Cancel any pending step-hold timer on unmount.
  useEffect(() => {
    return () => {
      if (stepTimerRef.current !== null) clearTimeout(stepTimerRef.current);
    };
  }, []);

  const fetchCpuBus = useCallback(async () => {
    try {
      const result = await invoke<CpuBusState>("get_cpu_bus_state");
      setCpuBus(result);
    } catch (e) {
      console.error("get_cpu_bus_state failed:", e);
    }
  }, []);

  useEffect(() => {
    fetchCpuBus();
  }, [fetchCpuBus]);

  // Re-fetch on halt/run-stopped events; also re-trigger the step flash on
  // each halt so auto-step produces a visible indicator pulse per tick.
  useEffect(() => {
    const unlistenHalted = listen("debugger-halted", () => {
      fetchCpuBus();
      if (execStateRef.current === "stepping") {
        triggerStepFlash();
      }
    });
    const unlistenRunStopped = listen("debugger-run-stopped", () => { fetchCpuBus(); });
    const unlistenTick = listen("debugger-running-tick", () => { fetchCpuBus(); });
    return () => {
      unlistenHalted.then((f) => f());
      unlistenRunStopped.then((f) => f());
      unlistenTick.then((f) => f());
    };
  }, [fetchCpuBus, triggerStepFlash]);

  const handleReset = useCallback(async () => {
    try {
      const snap = await invoke<RegisterSnapshot>("reset_cpu");
      onReset(snap);
    } catch (e) {
      console.error("reset_cpu failed:", e);
    }
  }, [onReset]);

  // Determine Run/Stop/Step/STP/WAI indicator label and color class.
  // STP and WAI override the normal "Stop" state when the CPU has halted
  // due to a STP or WAI instruction.
  let runStopLabel: string;
  let runStopClass: string;
  if (displayExecState === "running") {
    runStopLabel = "Run";
    runStopClass = "indicator-run";
  } else if (displayExecState === "stepping") {
    runStopLabel = "Step";
    runStopClass = "indicator-step";
  } else if (cpuBus?.cpu_stopped) {
    runStopLabel = "STP Executed";
    runStopClass = "indicator-idle";
  } else if (cpuBus?.cpu_waiting) {
    runStopLabel = "WAI Executed";
    runStopClass = "indicator-wai";
  } else {
    runStopLabel = "Stop";
    runStopClass = "indicator-stop";
  }

  return (
    <div className="cpu-bus-panel">
      <span className="panel-title">CPU and Bus</span>
      <div className="cpu-bus-body">
        <div className="cpu-bus-row">
          <span className={`indicator ${runStopClass}`}>●</span>
          <span className="indicator-label">{runStopLabel}</span>
        </div>
        <div className="cpu-bus-row">
          <span className={`indicator ${cpuBus?.nmi_pending ? "indicator-nmi-active" : "indicator-idle"}`}>●</span>
          <span className="indicator-label">NMI</span>
          <span className={`indicator indicator-spaced ${cpuBus?.irq_active ? "indicator-irq-active" : "indicator-idle"}`}>●</span>
          <span className="indicator-label">IRQ</span>
        </div>
        <div className="cpu-bus-row cpu-bus-cycles">
          <span className="cycles-value">{cpuBus !== null ? formatCycles(cpuBus.cycles) : "—"}</span>
          <span className="cycles-label">cycles</span>
        </div>
        <div className="cpu-bus-row cpu-bus-buttons">
          <span className="btn-placeholder" />
          <span className="btn-placeholder" />
          <button
            className="exec-btn reset-btn"
            onClick={handleReset}
            disabled={execState !== "stopped"}
            title="Reset CPU"
          >
            Reset
          </button>
        </div>
      </div>
    </div>
  );
}
