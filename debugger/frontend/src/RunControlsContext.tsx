import { createContext, useCallback, useContext, useEffect, useRef, useState, ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useExecutionContext } from "./ExecutionContext";

/** CPU execution state, used by StatusBar to display the Run/Stop/Step indicator. */
export type ExecState = "stopped" | "stepping" | "running";

interface RegisterSnapshot {
  a: number; x: number; y: number; s: number;
  pc: number; p: number; changed_flags: number;
  cpu_stopped: boolean;
  cpu_waiting: boolean;
  breakpoint_hit: boolean;
}

/** Auto-step interval bounds in milliseconds. */
const INTERVAL_MIN = 0;
const INTERVAL_MAX = 1000;
const INTERVAL_DEFAULT = 500;

/**
 * Tier definitions for the speed slider: [lo, hi, step_size].
 *
 * Each tier covers a sub-range of the interval domain. Moving the slider
 * thumb by one tick advances the interval by that tier's step size.
 */
const INTERVAL_TIERS: [number, number, number][] = [
  [INTERVAL_MIN, 100, 1],     // 100 steps
  [100, 200, 5],              // 20 steps
  [200, 500, 25],             // 12 steps
  [500, INTERVAL_MAX, 50],    // 10 steps
];
const INTERVAL_TOTAL_STEPS = INTERVAL_TIERS.reduce(
    (sum, [lo, hi, step]) => sum + (hi - lo) / step, 0
); // = 142

const SLIDER_STEPS = INTERVAL_TOTAL_STEPS; // 142 — 1:1 with raw steps

/** Map a slider integer position (0–142) to a millisecond interval. */
function sliderToInterval(pos: number): number {
  let remaining = pos;
  for (const [lo, hi, step] of INTERVAL_TIERS) {
    const tierSteps = (hi - lo) / step;
    if (remaining <= tierSteps) {
      return Math.min(hi, lo + remaining * step);
    }
    remaining -= tierSteps;
  }
  return INTERVAL_MAX;
}

/** Map a millisecond interval back to a slider integer position (0–142). */
function intervalToSlider(ms: number): number {
  let stepsBelow = 0;
  for (const [lo, hi, step] of INTERVAL_TIERS) {
    if (ms <= hi) {
      return stepsBelow + Math.round((ms - lo) / step);
    }
    stepsBelow += (hi - lo) / step;
  }
  return SLIDER_STEPS;
}

interface RunControlsContextValue {
  stepping: boolean;
  isAutoStepping: boolean;
  isFreeRunning: boolean;
  /** True only when the CPU is halted and idle; breakpoints can only be edited then. */
  isStopped: boolean;
  intervalMs: number;
  intervalInputValue: string;
  runCpu: () => Promise<void>;
  stopCpu: () => void;
  stepInto: () => Promise<void>;
  stepOver: () => Promise<void>;
  stepReturn: () => Promise<void>;
  toggleAutoStep: () => void;
  handleSliderChange: (e: React.ChangeEvent<HTMLInputElement>) => void;
  handleIntervalInputChange: (e: React.ChangeEvent<HTMLInputElement>) => void;
  handleIntervalInputBlur: (e: React.FocusEvent<HTMLInputElement>) => void;
  handleIntervalInputKeyDown: (e: React.KeyboardEvent<HTMLInputElement>) => void;
}

const RunControlsContext = createContext<RunControlsContextValue | null>(null);

/** Slider/step bounds exposed to RunControlsPanel for rendering the range input. */
export { SLIDER_STEPS, sliderToInterval, intervalToSlider };

/**
 * Owns Run/Step/Auto-Step state and handlers, shared by the floating Run Controls
 * panel, the native Run menu, and Disassembly's breakpoint-gating logic. Nested
 * inside `ExecutionProvider`; mounted once in the main window only.
 */
export function RunControlsProvider({ children }: { children: ReactNode }) {
  const { onStep, onExecStateChange, cpuStopped } = useExecutionContext();

  const [stepping, setStepping] = useState(false);
  const [isAutoStepping, setIsAutoStepping] = useState(false);
  const [isFreeRunning, setIsFreeRunning] = useState(false);
  const [intervalMs, setIntervalMs] = useState(INTERVAL_DEFAULT);
  const [intervalInputValue, setIntervalInputValue] = useState(String(INTERVAL_DEFAULT));

  const steppingRef = useRef(false);
  const isAutoSteppingRef = useRef(false);
  const isFreeRunningRef = useRef(false);
  const stoppingRef = useRef(false);
  const intervalMsRef = useRef(INTERVAL_DEFAULT);
  const autoStepTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  steppingRef.current = stepping;
  isAutoSteppingRef.current = isAutoStepping;
  isFreeRunningRef.current = isFreeRunning;
  intervalMsRef.current = intervalMs;

  const isStopped = !stepping && !isAutoStepping && !isFreeRunning;

  // Reset free-run/stopping state when a free run halts, whether from a
  // breakpoint, STP/WAI, or an explicit Stop. DisassemblyPanel also listens
  // for this event (to call onStep for its own row display) — deliberate
  // dual subscription, not duplication to clean up later.
  useEffect(() => {
    const unlistenPromise = listen<RegisterSnapshot>("debugger-run-stopped", () => {
      setIsFreeRunning(false);
      isFreeRunningRef.current = false;
      stoppingRef.current = false;
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  /** Execute one step using the named command and return the snapshot. Clears stepping lock on completion. */
  const doStep = useCallback(async (command: string = "step_into"): Promise<RegisterSnapshot | null> => {
    if (steppingRef.current) return null;
    setStepping(true);
    try {
      const snap = await invoke<RegisterSnapshot>(command);
      onStep(snap);
      return snap;
    } catch (e) {
      console.error(`${command} failed:`, e);
      return null;
    } finally {
      setStepping(false);
    }
  }, [onStep]);

  /** Single manual step (F11). */
  const stepInto = useCallback(async () => {
    if (isAutoSteppingRef.current || isFreeRunningRef.current) return;
    await doStep("step_into");
  }, [doStep]);

  /** Step over the current instruction, treating JSR as atomic (F10). */
  const stepOver = useCallback(async () => {
    if (isAutoSteppingRef.current || isFreeRunningRef.current || steppingRef.current) return;
    setIsFreeRunning(true);
    isFreeRunningRef.current = true;
    try {
      await invoke("step_over");
    } catch (e) {
      setIsFreeRunning(false);
      isFreeRunningRef.current = false;
      console.error("step_over failed:", e);
    }
  }, []);

  /** Run until the current subroutine returns (Shift+F11). */
  const stepReturn = useCallback(async () => {
    if (isAutoSteppingRef.current || isFreeRunningRef.current || steppingRef.current) return;
    setIsFreeRunning(true);
    isFreeRunningRef.current = true;
    try {
      await invoke("step_return");
    } catch (e) {
      setIsFreeRunning(false);
      isFreeRunningRef.current = false;
      console.error("step_return failed:", e);
    }
  }, []);

  /** Cancel any pending auto-step timer. */
  const clearAutoStepTimer = useCallback(() => {
    if (autoStepTimerRef.current !== null) {
      clearTimeout(autoStepTimerRef.current);
      autoStepTimerRef.current = null;
    }
  }, []);

  /** Schedule the next auto-step tick after the current interval. */
  const scheduleNextTick = useCallback(() => {
    clearAutoStepTimer();
    autoStepTimerRef.current = setTimeout(async () => {
      if (!isAutoSteppingRef.current) return;
      const snap = await doStep("step_into");
      if (snap?.cpu_stopped || snap?.cpu_waiting || snap?.breakpoint_hit) {
        setIsAutoStepping(false);
        return;
      }
      if (isAutoSteppingRef.current) {
        scheduleNextTick();
      }
    }, intervalMsRef.current);
  }, [doStep, clearAutoStepTimer]);

  /** Toggle auto-step on/off. */
  const toggleAutoStep = useCallback(() => {
    if (isFreeRunningRef.current) return;
    setIsAutoStepping((prev) => {
      const next = !prev;
      if (next) {
        // Starting — schedule first tick immediately.
        isAutoSteppingRef.current = true;
        scheduleNextTick();
      } else {
        isAutoSteppingRef.current = false;
        clearAutoStepTimer();
      }
      return next;
    });
  }, [scheduleNextTick, clearAutoStepTimer]);

  /** Start free-run execution (F5). Stops auto-step first if active. */
  const runCpu = useCallback(async () => {
    if (isFreeRunningRef.current || steppingRef.current) return;
    if (isAutoSteppingRef.current) {
      isAutoSteppingRef.current = false;
      setIsAutoStepping(false);
      clearAutoStepTimer();
    }
    setIsFreeRunning(true);
    isFreeRunningRef.current = true;
    try {
      await invoke("run_cpu");
    } catch (e) {
      setIsFreeRunning(false);
      isFreeRunningRef.current = false;
      console.error("run_cpu failed:", e);
    }
  }, [clearAutoStepTimer]);

  /** Signal the free-running CPU to stop (Shift+F5). */
  const stopCpu = useCallback(() => {
    if (!isFreeRunningRef.current || stoppingRef.current) return;
    stoppingRef.current = true;
    invoke("stop_cpu").catch((e) => {
      console.error("stop_cpu failed:", e);
      stoppingRef.current = false;
    });
  }, []);

  // Clean up timer on unmount.
  useEffect(() => {
    return () => clearAutoStepTimer();
  }, [clearAutoStepTimer]);

  // Notify ExecutionContext of execution state changes so StatusBar can update its indicator.
  useEffect(() => {
    if (stepping || isAutoStepping) {
      onExecStateChange("stepping");
    } else if (isFreeRunning) {
      onExecStateChange("running");
    } else {
      onExecStateChange("stopped");
    }
  }, [stepping, isAutoStepping, isFreeRunning, onExecStateChange]);

  // Stop auto-step when the CPU is reset from StatusBar.
  useEffect(() => {
    const unlistenPromise = listen("debugger-cpu-reset", () => {
      if (isAutoSteppingRef.current) {
        isAutoSteppingRef.current = false;
        setIsAutoStepping(false);
        clearAutoStepTimer();
      }
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, [clearAutoStepTimer]);

  // A native Run-menu accelerator or click (see `on_menu_event` in `lib.rs`)
  // reaches this handler the same way a menu-driven panel reveal does — as
  // an event targeted at the main window. Dispatching to the exact same
  // functions the floating panel's own buttons call means there's only ever
  // one implementation of each action.
  useEffect(() => {
    const unlistenPromise = listen<string>("run-menu-action", (event) => {
      switch (event.payload) {
        case "run-cpu": runCpu(); break;
        case "stop-cpu": stopCpu(); break;
        case "step-into": stepInto(); break;
        case "step-over": stepOver(); break;
        case "step-return": stepReturn(); break;
        case "toggle-auto-step": toggleAutoStep(); break;
      }
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, [runCpu, stopCpu, stepInto, stepOver, stepReturn, toggleAutoStep]);

  // Keeps the native Run menu's enabled state in lockstep with the floating
  // panel's own button `disabled` logic (lifted verbatim from
  // RunControlsPanel.tsx's JSX). Guarded by `lastEnabledRef` so an
  // auto-step timer tick — which touches `stepping` every cycle without
  // changing any of these booleans — doesn't invoke the command needlessly.
  const lastEnabledRef = useRef<string | null>(null);
  useEffect(() => {
    const flags = {
      run: !(isFreeRunning || isAutoStepping || stepping || cpuStopped),
      stop: isFreeRunning && !cpuStopped,
      step_into: !(stepping || isAutoStepping || isFreeRunning || cpuStopped),
      step_over: !(stepping || isAutoStepping || isFreeRunning || cpuStopped),
      step_return: !(stepping || isAutoStepping || isFreeRunning || cpuStopped),
      toggle_auto_step: !(isFreeRunning || (stepping && !isAutoStepping) || cpuStopped),
    };
    const key = JSON.stringify(flags);
    if (key === lastEnabledRef.current) return;
    lastEnabledRef.current = key;
    invoke("set_run_controls_enabled", { flags }).catch((e) =>
      console.error("set_run_controls_enabled failed:", e),
    );
  }, [stepping, isAutoStepping, isFreeRunning, cpuStopped]);

  // Keeps the native File > Reload Profile item's enabled state in lockstep
  // with `isStopped` (issue #547) — unlike the Run menu's items above, this
  // doesn't also gate on `cpuStopped` (STP halt): reloading is safe whenever
  // the CPU isn't actively running/stepping, matching how the Memory and
  // Assembler menus interpret "the CPU must be stopped" for their own
  // items. Lives here (rather than in a panel, like those two) since Reload
  // Profile isn't owned by any one panel — this provider is already the
  // established place a menu-item's enabled state is pushed globally.
  const lastReloadEnabledRef = useRef<boolean | null>(null);
  useEffect(() => {
    if (lastReloadEnabledRef.current === isStopped) return;
    lastReloadEnabledRef.current = isStopped;
    invoke("set_reload_profile_menu_enabled", { enabled: isStopped }).catch((e) =>
      console.error("set_reload_profile_menu_enabled failed:", e),
    );
  }, [isStopped]);

  const handleSliderChange = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const pos = parseInt(e.target.value, 10);
    const ms = sliderToInterval(pos);
    setIntervalMs(ms);
    setIntervalInputValue(String(ms));
  }, []);

  const commitIntervalInput = useCallback((raw: string) => {
    const parsed = parseInt(raw, 10);
    const clamped = isNaN(parsed) ? INTERVAL_DEFAULT : Math.min(INTERVAL_MAX, Math.max(INTERVAL_MIN, parsed));
    // Snap to the nearest tier step by round-tripping through the slider mapping.
    const snapped = sliderToInterval(intervalToSlider(clamped));
    setIntervalMs(snapped);
    setIntervalInputValue(String(snapped));
  }, []);

  const handleIntervalInputChange = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    setIntervalInputValue(e.target.value);
  }, []);

  const handleIntervalInputBlur = useCallback((e: React.FocusEvent<HTMLInputElement>) => {
    commitIntervalInput(e.target.value);
  }, [commitIntervalInput]);

  const handleIntervalInputKeyDown = useCallback((e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      commitIntervalInput((e.target as HTMLInputElement).value);
      (e.target as HTMLInputElement).blur();
    }
  }, [commitIntervalInput]);

  return (
    <RunControlsContext.Provider
      value={{
        stepping,
        isAutoStepping,
        isFreeRunning,
        isStopped,
        intervalMs,
        intervalInputValue,
        runCpu,
        stopCpu,
        stepInto,
        stepOver,
        stepReturn,
        toggleAutoStep,
        handleSliderChange,
        handleIntervalInputChange,
        handleIntervalInputBlur,
        handleIntervalInputKeyDown,
      }}
    >
      {children}
    </RunControlsContext.Provider>
  );
}

/** Returns the shared run/step/auto-step state and handlers. Must be used within a `RunControlsProvider`. */
export function useRunControlsContext(): RunControlsContextValue {
  const ctx = useContext(RunControlsContext);
  if (!ctx) throw new Error("useRunControlsContext must be used within a RunControlsProvider");
  return ctx;
}
