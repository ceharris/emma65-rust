import {useExecutionContext} from "./ExecutionContext";
import {intervalToSlider, SLIDER_STEPS, useRunControlsContext} from "./RunControlsContext";
import "./styles/run-controls.scss";

/** Auto-step interval bounds in milliseconds, for the speed-input title. */
const INTERVAL_MIN = 0;
const INTERVAL_MAX = 1000;

/**
 * Floating-window height (px) that fits the single control row plus
 * dockview's own floating-titlebar/tab-bar chrome — see `DockLayout.tsx`'s
 * `RUN_CONTROLS_DEFAULT_BOUNDS`. Dockview never auto-resizes a floating
 * group to fit its content and there's no safe way to resize one after
 * creation (see `project_dockview_addfloatinggroup_resize_bug` in memory),
 * so a collapsible Auto-Step drawer that changed the panel's height was a
 * dead end — Auto-Step's controls stay inline on the one row instead.
 */
export const RUN_CONTROLS_HEIGHT = 100;

/**
 * Floating-window width (px) that fits the full set of run controls.
 */
export const RUN_CONTROLS_WIDTH = 400;

/**
 * Minimum content height (px) dockview should allow when this panel is
 * docked: the single control row plus the panel's own vertical padding,
 * without `RUN_CONTROLS_HEIGHT`'s floating-titlebar/tab-bar allowance.
 * Passed as `minimumHeight` on every `addPanel` call that creates this
 * panel (see `DockLayout.tsx`) — without it, dockview's `DockviewGroupPanel`
 * falls back to its own hardcoded 100px group minimum for any panel that
 * doesn't declare one, which is far more than this single-row toolbar needs
 * once docked (issue #401).
 */
export const RUN_CONTROLS_MIN_HEIGHT = 40;

/**
 * Minimum content width (px) dockview should allow when this panel is
 * docked — same value as `RUN_CONTROLS_WIDTH` since that was already tuned
 * to the narrowest width the single-row layout fits without wrapping.
 */
export const RUN_CONTROLS_MIN_WIDTH = RUN_CONTROLS_WIDTH;

/**
 * Floating panel hosting the Run/Stop/Step Into/Step Over/Step Return
 * buttons plus the Auto-Step toggle and speed slider, all on one row,
 * replacing the toolbar that used to live in Disassembly's header.
 */
export default function RunControlsPanel() {
  const { cpuStopped } = useExecutionContext();
  const {
    stepping, isAutoStepping, isFreeRunning, intervalMs, intervalInputValue,
    runCpu, stopCpu, stepInto, stepOver, stepReturn, toggleAutoStep,
    handleSliderChange, handleIntervalInputChange, handleIntervalInputBlur, handleIntervalInputKeyDown,
  } = useRunControlsContext();

  return (
    <div className="run-controls-panel">
      <div className="run-controls-row">
        <div className="exec-controls">
          <button
            className="exec-btn run-btn"
            onClick={runCpu}
            disabled={isFreeRunning || isAutoStepping || stepping || cpuStopped}
            title="Run (F5)"
          >
            <i className="codicon codicon-debug-continue" />
          </button>
          <button
            className="exec-btn step-over-btn"
            onClick={stepOver}
            disabled={stepping || isAutoStepping || isFreeRunning || cpuStopped}
            title="Step Over (F10)"
          >
            <i className="codicon codicon-debug-step-over" />
          </button>
          <button
            className="exec-btn step-into-btn"
            onClick={stepInto}
            disabled={stepping || isAutoStepping || isFreeRunning || cpuStopped}
            title="Step Into (F11)"
          >
            <i className="codicon codicon-debug-step-into" />
          </button>
          <button
            className="exec-btn step-return-btn"
            onClick={stepReturn}
            disabled={stepping || isAutoStepping || isFreeRunning || cpuStopped}
            title="Step Return (Shift+F11)"
          >
            <i className="codicon codicon-debug-step-out" />
          </button>
          <button
            className="exec-btn stop-btn"
            onClick={stopCpu}
            disabled={!isFreeRunning || cpuStopped}
            title="Stop (Shift+F5)"
          >
            <i className="codicon codicon-debug-stop" />
          </button>
        </div>
        <div className="auto-step-control">
          <button
            className={`exec-btn auto-step-btn${isAutoStepping ? " active" : ""}`}
            onClick={toggleAutoStep}
            disabled={isFreeRunning || (stepping && !isAutoStepping) || cpuStopped}
            title="Auto-Step (Ctrl+Shift+F5)"
          >
            <i className={`codicon codicon-${isAutoStepping ? "debug-pause" : "sync"}`} />
          </button>
          <input
            className="speed-slider"
            type="range"
            min={0}
            max={SLIDER_STEPS}
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
  );
}
