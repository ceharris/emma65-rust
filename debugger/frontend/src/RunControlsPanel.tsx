import { useState } from "react";
import { useExecutionContext } from "./ExecutionContext";
import { SLIDER_STEPS, intervalToSlider, useRunControlsContext } from "./RunControlsContext";
import "./styles/run-controls.scss";

/** Auto-step interval bounds in milliseconds, for the speed-input title. */
const INTERVAL_MIN = 0;
const INTERVAL_MAX = 1000;

/**
 * Floating panel hosting the Run/Stop/Step Into/Step Over/Step Return buttons
 * and a collapsible Auto-Step drawer (toggle + speed slider), replacing the
 * toolbar that used to live in Disassembly's header.
 */
export default function RunControlsPanel() {
  const { cpuStopped } = useExecutionContext();
  const {
    stepping, isAutoStepping, isFreeRunning, intervalMs, intervalInputValue,
    runCpu, stopCpu, stepInto, stepOver, stepReturn, toggleAutoStep,
    handleSliderChange, handleIntervalInputChange, handleIntervalInputBlur, handleIntervalInputKeyDown,
  } = useRunControlsContext();
  const [autoStepExpanded, setAutoStepExpanded] = useState(false);

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
        <div className="auto-step-toggle">
          <button
            className={`exec-btn auto-step-btn${isAutoStepping ? " active" : ""}`}
            onClick={toggleAutoStep}
            disabled={isFreeRunning || (stepping && !isAutoStepping) || cpuStopped}
            title="Auto-Step (Ctrl+Shift+F5)"
          >
            <i className={`codicon codicon-${isAutoStepping ? "debug-pause" : "sync"}`} />
          </button>
          <button
            className="exec-btn disclosure-btn"
            onClick={() => setAutoStepExpanded((prev) => !prev)}
            title={autoStepExpanded ? "Hide Auto-Step speed" : "Show Auto-Step speed"}
          >
            <i className={`codicon codicon-chevron-${autoStepExpanded ? "up" : "down"}`} />
          </button>
        </div>
      </div>
      {autoStepExpanded && (
        <div className="run-controls-row auto-step-control">
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
      )}
    </div>
  );
}
