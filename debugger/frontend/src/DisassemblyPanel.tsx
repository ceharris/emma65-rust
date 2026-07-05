import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/disassembly.scss";

interface DisassembledRow {
  addr: number;
  bytes: string[];
  mnemonic: string;
  operand: string;
  is_valid: boolean;
}

interface RegisterSnapshot {
  a: number; x: number; y: number; s: number;
  pc: number; p: number; changed_flags: number;
  cpu_stopped: boolean;
  breakpoint_hit: boolean;
}

interface Props {
  /** Called with the post-step register snapshot so RegisterPanel can update immediately. */
  onStep: (snap: RegisterSnapshot) => void;
}

/** Rows to pre-fetch beyond the visible window so scrolling has buffer. */
const FETCH_ROWS = 48;
/** When PC index reaches within this many rows of the bottom, extend the list. */
const SCROLL_EDGE = 6;

/** Auto-step interval bounds in milliseconds. */
const INTERVAL_MIN = 10;
const INTERVAL_MAX = 5000;
const INTERVAL_DEFAULT = 500;

/**
 * Tier definitions for the speed slider: [lo, hi, step_size].
 *
 * Each tier covers a sub-range of the interval domain. Moving the slider
 * thumb by one tick advances the interval by that tier's step size.
 */
const INTERVAL_TIERS: [number, number, number][] = [
  [INTERVAL_MIN, 500, 25],   // 19 steps
  [500, 1000, 50],            // 10 steps
  [1000, 2000, 100],          // 10 steps
  [2000, INTERVAL_MAX, 250],  // 12 steps
];

const INTERVAL_TOTAL_STEPS = INTERVAL_TIERS.reduce(
  (sum, [lo, hi, step]) => sum + (hi - lo) / step, 0
);

const SLIDER_STEPS = 100;

/** Map a slider integer position (0–100) to a millisecond interval. */
function sliderToInterval(pos: number): number {
  let remaining = Math.round((pos / SLIDER_STEPS) * INTERVAL_TOTAL_STEPS);
  for (const [lo, hi, step] of INTERVAL_TIERS) {
    const tierSteps = (hi - lo) / step;
    if (remaining <= tierSteps) {
      return Math.min(hi, lo + remaining * step);
    }
    remaining -= tierSteps;
  }
  return INTERVAL_MAX;
}

/** Map a millisecond interval back to a slider integer position (0–100). */
function intervalToSlider(ms: number): number {
  const totalSteps = INTERVAL_TOTAL_STEPS;
  const sliderSteps = SLIDER_STEPS;

  let stepsBelow = 0;
  for (const [lo, hi, step] of INTERVAL_TIERS) {
    if (ms <= hi) {
      const offset = Math.round((ms - lo) / step);
      return Math.round(((stepsBelow + offset) / totalSteps) * sliderSteps);
    }
    stepsBelow += (hi - lo) / step;
  }
  return sliderSteps;
}

export default function DisassemblyPanel({ onStep }: Props) {
  const [rows, setRows] = useState<DisassembledRow[]>([]);
  const [currentPc, setCurrentPc] = useState<number | null>(null);
  const [stepping, setStepping] = useState(false);
  const [isAutoStepping, setIsAutoStepping] = useState(false);
  const [intervalMs, setIntervalMs] = useState(INTERVAL_DEFAULT);
  const [intervalInputValue, setIntervalInputValue] = useState(String(INTERVAL_DEFAULT));
  const [breakpoints, setBreakpoints] = useState<Set<number>>(new Set());

  // Keep refs so callbacks see the latest values without being re-created.
  const rowsRef = useRef<DisassembledRow[]>([]);
  const pcRef = useRef<number | null>(null);
  const steppingRef = useRef(false);
  const isAutoSteppingRef = useRef(false);
  const intervalMsRef = useRef(INTERVAL_DEFAULT);
  const autoStepTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  rowsRef.current = rows;
  pcRef.current = currentPc;
  steppingRef.current = stepping;
  isAutoSteppingRef.current = isAutoStepping;
  intervalMsRef.current = intervalMs;

  const handleToggleBreakpoint = useCallback(async (addr: number) => {
    try {
      const updated = await invoke<number[]>("toggle_breakpoint", { addr });
      setBreakpoints(new Set(updated));
    } catch (e) {
      console.error("toggle_breakpoint failed:", e);
    }
  }, []);

  /** Fetch `FETCH_ROWS` instructions starting at `addr` and replace the row list. */
  const fetchFrom = useCallback(async (addr: number) => {
    try {
      const result = await invoke<DisassembledRow[]>("get_disassembly", {
        addr,
        count: FETCH_ROWS,
      });
      setRows(result);
    } catch (e) {
      console.error("get_disassembly failed:", e);
    }
  }, []);

  /** Append enough new rows so at least `FETCH_ROWS` exist past the current PC index. */
  const extendFrom = useCallback(async (fromAddr: number) => {
    try {
      const extra = await invoke<DisassembledRow[]>("get_disassembly", {
        addr: fromAddr,
        count: FETCH_ROWS,
      });
      setRows((prev) => {
        // Deduplicate by addr: keep all prev rows, then append rows whose addr isn't already present.
        const seen = new Set(prev.map((r) => r.addr));
        const newRows = extra.filter((r) => !seen.has(r.addr));
        return [...prev, ...newRows];
      });
    } catch (e) {
      console.error("get_disassembly (extend) failed:", e);
    }
  }, []);

  const handleHalted = useCallback(async (newPc: number) => {
    const current = rowsRef.current;

    // Find where the new PC sits in the current row list.
    const pcIndex = current.findIndex((r) => r.addr === newPc);

    if (pcIndex === -1) {
      // PC is not in the current row list (jump to a new location) — fetch fresh.
      await fetchFrom(newPc);
      setCurrentPc(newPc);
      return;
    }

    // PC is in the list. If it's approaching the bottom edge, extend the list.
    if (pcIndex >= current.length - SCROLL_EDGE) {
      const lastRow = current[current.length - 1];
      // Start fetching from just after the last known row.
      const nextAddr = lastRow.addr + lastRow.bytes.length;
      extendFrom(nextAddr);
    }

    setCurrentPc(newPc);
  }, [fetchFrom, extendFrom]);

  useEffect(() => {
    const unlistenPromise = listen<number>("debugger-halted", (event) => {
      handleHalted(event.payload);
    });

    // Proactively fetch on mount: the initial `debugger-halted` event can fire
    // before our listener is registered (listen() is async), leaving rows empty.
    invoke<RegisterSnapshot>("get_registers")
      .then((snap) => { if (rowsRef.current.length === 0) handleHalted(snap.pc); })
      .catch(() => {});

    invoke<number[]>("get_breakpoints")
      .then((list) => setBreakpoints(new Set(list)))
      .catch(() => {});

    return () => { unlistenPromise.then((f) => f()); };
  }, [handleHalted]);

  // Scroll the current-PC row into view whenever it changes.
  const pcRowRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    pcRowRef.current?.scrollIntoView({ block: "nearest" });
  }, [currentPc]);

  /** Execute one step and return the snapshot. Clears stepping lock on completion. */
  const doStep = useCallback(async (): Promise<RegisterSnapshot | null> => {
    if (steppingRef.current) return null;
    setStepping(true);
    try {
      const snap = await invoke<RegisterSnapshot>("step_into");
      onStep(snap);
      return snap;
    } catch (e) {
      console.error("step_into failed:", e);
      return null;
    } finally {
      setStepping(false);
    }
  }, [onStep]);

  /** Single manual step (F11). */
  const stepInto = useCallback(async () => {
    if (isAutoSteppingRef.current) return;
    await doStep();
  }, [doStep]);

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
      const snap = await doStep();
      if (snap?.cpu_stopped || snap?.breakpoint_hit) {
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

  /** Reset the CPU, stopping auto-step first if active (Shift+F5). */
  const resetCpu = useCallback(async () => {
    if (steppingRef.current) return;
    if (isAutoSteppingRef.current) {
      isAutoSteppingRef.current = false;
      setIsAutoStepping(false);
      clearAutoStepTimer();
    }
    try {
      const snap = await invoke<RegisterSnapshot>("reset_cpu");
      onStep(snap);
    } catch (e) {
      console.error("reset_cpu failed:", e);
    }
  }, [onStep, clearAutoStepTimer]);

  // Clean up timer on unmount.
  useEffect(() => {
    return () => clearAutoStepTimer();
  }, [clearAutoStepTimer]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "F11" && !e.shiftKey) {
        e.preventDefault();
        stepInto();
      }
      if (e.key === "F5" && e.shiftKey && !e.ctrlKey) {
        e.preventDefault();
        resetCpu();
      }
      if (e.key === "F5" && e.ctrlKey && e.shiftKey) {
        e.preventDefault();
        toggleAutoStep();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [stepInto, resetCpu, toggleAutoStep]);

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

  const formatAddr = (addr: number) =>
    addr.toString(16).toUpperCase().padStart(4, "0");

  const formatBytes = (bytes: string[]) =>
    bytes.map((b) => b.padStart(2, "0")).join(" ").padEnd(8, " ");

  return (
    <div className="disassembly-panel">
      <div className="disassembly-header">
        <div className="disassembly-toolbar">
          <span className="panel-title">Disassembly</span>
          <div className="exec-controls">
            <button
              className="exec-btn step-into-btn"
              onClick={stepInto}
              disabled={stepping || isAutoStepping}
              title="Step Into (F11)"
            >
              Step Into
            </button>
            <button
              className="exec-btn reset-btn"
              onClick={resetCpu}
              title="Reset CPU (Shift+F5)"
            >
              Reset
            </button>
          </div>
        </div>
        <div className="disassembly-toolbar">
          <div className="auto-step-control">
            <button
              className={`exec-btn auto-step-btn${isAutoStepping ? " active" : ""}`}
              onClick={toggleAutoStep}
              disabled={stepping && !isAutoStepping}
              title="Auto-Step (Ctrl+Shift+F5)"
            >
              {isAutoStepping ? "Stop" : "Auto-Step"}
            </button>
            <input
              className="speed-slider"
              type="range"
              min={0}
              max={100}
              value={intervalToSlider(intervalMs)}
              onChange={handleSliderChange}
              title="Step interval"
            />
            <input
              className="speed-input"
              type="text"
              inputMode="numeric"
              value={intervalInputValue}
              onChange={handleIntervalInputChange}
              onBlur={handleIntervalInputBlur}
              onKeyDown={handleIntervalInputKeyDown}
              title={`Step interval in ms (${INTERVAL_MIN}–${INTERVAL_MAX})`}
            />
            <span className="speed-unit">ms</span>
          </div>
        </div>
      </div>
      <div className="disassembly-body">
        {rows.length === 0 ? (
          <span className="disassembly-empty">Waiting for session…</span>
        ) : (
          rows.map((row) => {
            const isCurrent = row.addr === currentPc;
            const hasBreakpoint = breakpoints.has(row.addr);
            return (
              <div
                key={row.addr}
                ref={isCurrent ? pcRowRef : null}
                className={[
                  "disasm-row",
                  isCurrent ? "current-pc" : "",
                  row.is_valid ? "" : "invalid-op",
                ]
                  .filter(Boolean)
                  .join(" ")}
              >
                <span
                  className={`disasm-gutter${hasBreakpoint ? " breakpoint" : ""}`}
                  onClick={() => handleToggleBreakpoint(row.addr)}
                  title={hasBreakpoint ? "Remove breakpoint" : "Set breakpoint"}
                >
                  {hasBreakpoint ? "●" : ""}
                </span>
                <span className="disasm-addr">{formatAddr(row.addr)}</span>
                <span className="disasm-bytes">{formatBytes(row.bytes)}</span>
                <span className="disasm-mnemonic">{row.mnemonic}</span>
                {row.operand && (
                  <span className="disasm-operand">{row.operand}</span>
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
