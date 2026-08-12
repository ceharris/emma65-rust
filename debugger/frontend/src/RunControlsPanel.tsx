import { useCallback, useState } from "react";
import type { IDockviewPanelProps } from "dockview-react";
import { useExecutionContext } from "./ExecutionContext";
import { SLIDER_STEPS, intervalToSlider, useRunControlsContext } from "./RunControlsContext";
import "./styles/run-controls.scss";

/** Auto-step interval bounds in milliseconds, for the speed-input title. */
const INTERVAL_MIN = 0;
const INTERVAL_MAX = 1000;

/**
 * Floating-window height (px) that fits just the button row plus dockview's
 * own floating-titlebar/tab-bar chrome — the collapsed default (see
 * `DockLayout.tsx`'s `RUN_CONTROLS_DEFAULT_BOUNDS`) and the toggle target
 * whenever the Auto-Step drawer closes.
 */
export const RUN_CONTROLS_COLLAPSED_HEIGHT = 100;
/** Floating-window height (px) that also fits the Auto-Step speed row. */
const RUN_CONTROLS_EXPANDED_HEIGHT = 135;

/**
 * Floating panel hosting the Run/Stop/Step Into/Step Over/Step Return buttons
 * and a collapsible Auto-Step drawer (toggle + speed slider), replacing the
 * toolbar that used to live in Disassembly's header.
 */
export default function RunControlsPanel({ api, containerApi }: IDockviewPanelProps) {
  const { cpuStopped } = useExecutionContext();
  const {
    stepping, isAutoStepping, isFreeRunning, intervalMs, intervalInputValue,
    runCpu, stopCpu, stepInto, stepOver, stepReturn, toggleAutoStep,
    handleSliderChange, handleIntervalInputChange, handleIntervalInputBlur, handleIntervalInputKeyDown,
  } = useRunControlsContext();
  const [autoStepExpanded, setAutoStepExpanded] = useState(false);

  // Dockview never auto-resizes a floating group to fit its content, so
  // leaving the window sized for the expanded drawer permanently left a
  // large blank gap below the button row whenever it was collapsed. The
  // only documented way to resize an *already*-floating group without
  // recreating (and so losing the state of) its React content is to
  // re-float it in place: `addFloatingGroup` reuses the same underlying
  // group/panel when passed a group that's already floating, it just tears
  // down and rebuilds the floating window chrome around it. Only fires while
  // actually floating — a user who's dragged the panel into the dock
  // shouldn't have it yanked back out just from toggling this, and a docked
  // group's size is the surrounding split layout's business, not ours.
  // Deliberately *not* run on mount/restore: that would silently override a
  // size the user picked by hand-dragging the window's own resize handles,
  // undoing the cross-restart size persistence every other dock panel gets.
  const toggleAutoStepDrawer = useCallback(() => {
    setAutoStepExpanded((prev) => {
      const next = !prev;
      if (api.group.api.location.type === "floating") {
        const box = api.group.api.boundingBox;
        if (box) {
          containerApi.addFloatingGroup(api.group, {
            x: box.left,
            y: box.top,
            width: box.width,
            height: next ? RUN_CONTROLS_EXPANDED_HEIGHT : RUN_CONTROLS_COLLAPSED_HEIGHT,
          });
        }
      }
      return next;
    });
  }, [api, containerApi]);

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
            onClick={toggleAutoStepDrawer}
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
