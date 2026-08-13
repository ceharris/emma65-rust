import {useExecutionContext} from "./ExecutionContext";
import {intervalToSlider, SLIDER_STEPS, useRunControlsContext} from "./RunControlsContext";
import "./styles/run-controls.scss";

/** Auto-step interval bounds in milliseconds, for the speed-input title. */
const INTERVAL_MIN = 0;
const INTERVAL_MAX = 1000;

/**
 * Fixed content height (px) for this panel while docked: the single control
 * row plus the panel's own vertical padding, with a little breathing room
 * (issue #421's tailored default). Passed as both `minimumHeight` and
 * `maximumHeight` on every docked `addPanel` call that creates this panel
 * (see `DockLayout.tsx`), locking it at this height — a single-row toolbar
 * never needs more or less, so leaving it resizable only wastes, or steals
 * from Disassembly's column, space that never gets used (issue #424).
 * Without an explicit `minimumHeight` below dockview's own hardcoded 100px
 * `DockviewGroupPanel` group-minimum fallback, this would also be far more
 * than this toolbar needs (issue #401). Dockview never auto-resizes a
 * floating group to fit its content and there's no safe way to resize one
 * after creation (see `project_dockview_addfloatinggroup_resize_bug` in
 * memory), so a collapsible Auto-Step drawer that changed the panel's height
 * was a dead end — Auto-Step's controls stay inline on the one row instead.
 */
export const RUN_CONTROLS_DOCKED_HEIGHT = 70;

/**
 * Minimum content width (px) dockview should allow for this panel — the
 * narrowest width the single-row layout (buttons plus the Auto-Step
 * slider/input) fits without wrapping. Passed as `minimumWidth` alongside
 * `RUN_CONTROLS_DOCKED_HEIGHT`. Unlike height, there's no matching maximum:
 * while docked, width is shared with Disassembly's column via the sash
 * between columns, so capping it here would cap Disassembly's width too.
 */
export const RUN_CONTROLS_MIN_WIDTH = 400;

/**
 * Dock panel (docked at the bottom of Disassembly's column by default,
 * issue #402; can also float) hosting the Run/Stop/Step Into/Step Over/Step
 * Return buttons plus the Auto-Step toggle and speed slider, all on one
 * row, replacing the toolbar that used to live in Disassembly's header.
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
