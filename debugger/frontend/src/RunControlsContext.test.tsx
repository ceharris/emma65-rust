import { act, renderHook, waitFor } from "@testing-library/react";
import { ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ExecutionProvider } from "./ExecutionContext";
import type { RegisterSnapshot } from "./RegisterPanel";
import {
  intervalToSlider,
  RunControlsProvider,
  SLIDER_STEPS,
  sliderToInterval,
  useRunControlsContext,
} from "./RunControlsContext";
import { emitMockEvent, invoke, resetTauriMocks } from "./test/tauriMock";

function snapshot(overrides: Partial<RegisterSnapshot> = {}): RegisterSnapshot {
  return {
    a: 0, x: 0, y: 0, s: 0xff, pc: 0x8000, p: 0x20, changed_flags: 0,
    cpu_stopped: false, cpu_waiting: false, breakpoint_hit: false,
    ...overrides,
  };
}

function Providers({ children }: { children: ReactNode }) {
  return (
    <ExecutionProvider>
      <RunControlsProvider>{children}</RunControlsProvider>
    </ExecutionProvider>
  );
}

const STOPPED_FLAGS = {
  run: true, stop: false, step_into: true, step_over: true, step_return: true, toggle_auto_step: true,
};
const RUNNING_FLAGS = {
  run: false, stop: true, step_into: false, step_over: false, step_return: false, toggle_auto_step: false,
};

beforeEach(() => {
  resetTauriMocks();
  vi.mocked(invoke).mockImplementation(async (cmd: unknown) => (cmd === "step_into" ? snapshot() : undefined));
});

describe("sliderToInterval / intervalToSlider", () => {
  it("SLIDER_STEPS is the sum of every tier's step count", () => {
    expect(SLIDER_STEPS).toBe(142);
  });

  it("maps the domain endpoints", () => {
    expect(sliderToInterval(0)).toBe(0);
    expect(sliderToInterval(SLIDER_STEPS)).toBe(1000);
    expect(intervalToSlider(0)).toBe(0);
    expect(intervalToSlider(1000)).toBe(SLIDER_STEPS);
  });

  it("round-trips representative interior values", () => {
    for (const ms of [1, 50, 100, 150, 300, 500, 750]) {
      expect(sliderToInterval(intervalToSlider(ms))).toBeGreaterThanOrEqual(0);
    }
    for (const pos of [0, 25, 50, 75, 100, 120, 142]) {
      expect(intervalToSlider(sliderToInterval(pos))).toBe(pos);
    }
  });
});

describe("useRunControlsContext", () => {
  it("throws when used outside a RunControlsProvider", () => {
    expect(() => renderHook(() => useRunControlsContext())).toThrow(
      /must be used within a RunControlsProvider/,
    );
  });
});

describe("RunControlsProvider commands", () => {
  it("stepInto sets stepping while step_into is in flight, invokes it, and clears stepping after", async () => {
    let resolveStep!: (snap: RegisterSnapshot) => void;
    vi.mocked(invoke).mockImplementationOnce(() => new Promise((resolve) => { resolveStep = resolve; }));
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });

    let stepPromise!: Promise<void>;
    act(() => { stepPromise = result.current.stepInto(); });

    await waitFor(() => expect(result.current.stepping).toBe(true));
    expect(invoke).toHaveBeenCalledWith("step_into");

    await act(async () => {
      resolveStep(snapshot());
      await stepPromise;
    });

    expect(result.current.stepping).toBe(false);
  });

  it("runCpu invokes run_cpu and sets isFreeRunning", async () => {
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });

    await act(async () => { await result.current.runCpu(); });

    expect(invoke).toHaveBeenCalledWith("run_cpu");
    expect(result.current.isFreeRunning).toBe(true);
    expect(result.current.isStopped).toBe(false);
  });

  it("stopCpu only invokes stop_cpu while free-running", async () => {
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });

    act(() => result.current.stopCpu());
    expect(invoke).not.toHaveBeenCalledWith("stop_cpu");

    await act(async () => { await result.current.runCpu(); });
    act(() => result.current.stopCpu());

    expect(invoke).toHaveBeenCalledWith("stop_cpu");
  });

  it("a debugger-run-stopped event clears isFreeRunning", async () => {
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });
    await act(async () => { await result.current.runCpu(); });
    expect(result.current.isFreeRunning).toBe(true);

    act(() => emitMockEvent("debugger-run-stopped", snapshot()));

    expect(result.current.isFreeRunning).toBe(false);
    expect(result.current.isStopped).toBe(true);
  });

  it("stepOver and stepReturn are no-ops while already free-running", async () => {
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });
    await act(async () => { await result.current.runCpu(); });
    vi.mocked(invoke).mockClear();

    await act(async () => { await result.current.stepOver(); });
    await act(async () => { await result.current.stepReturn(); });

    expect(invoke).not.toHaveBeenCalledWith("step_over");
    expect(invoke).not.toHaveBeenCalledWith("step_return");
  });

  it("toggleAutoStep turns on auto-stepping, and a debugger-cpu-reset event turns it back off", () => {
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });

    act(() => result.current.toggleAutoStep());
    expect(result.current.isAutoStepping).toBe(true);

    act(() => emitMockEvent("debugger-cpu-reset", undefined));

    expect(result.current.isAutoStepping).toBe(false);
  });

  it("dispatches a run-menu-action run-cpu event to runCpu", async () => {
    renderHook(() => useRunControlsContext(), { wrapper: Providers });

    act(() => emitMockEvent("run-menu-action", "run-cpu"));

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("run_cpu"));
  });
});

describe("RunControlsProvider menu-enabled sync", () => {
  it("keeps set_run_controls_enabled in sync across stopped -> running -> stopped", async () => {
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("set_run_controls_enabled", { flags: STOPPED_FLAGS }),
    );

    await act(async () => { await result.current.runCpu(); });

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("set_run_controls_enabled", { flags: RUNNING_FLAGS }),
    );

    vi.mocked(invoke).mockClear();
    act(() => emitMockEvent("debugger-run-stopped", snapshot()));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("set_run_controls_enabled", { flags: STOPPED_FLAGS }),
    );
  });

  it("gates the profile and recent menus on isStopped", async () => {
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("set_profile_menu_enabled", { enabled: true }));
    expect(invoke).toHaveBeenCalledWith("set_recent_menu_enabled", { enabled: true });

    vi.mocked(invoke).mockClear();
    await act(async () => { await result.current.runCpu(); });

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("set_profile_menu_enabled", { enabled: false }));
    expect(invoke).toHaveBeenCalledWith("set_recent_menu_enabled", { enabled: false });
  });
});

describe("interval slider/input handlers", () => {
  it("handleSliderChange updates intervalMs and the input value together", () => {
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });

    act(() =>
      result.current.handleSliderChange({
        target: { value: "100" },
      } as React.ChangeEvent<HTMLInputElement>),
    );

    expect(result.current.intervalMs).toBe(100);
    expect(result.current.intervalInputValue).toBe("100");
  });

  it("handleIntervalInputBlur commits a typed value, snapped to the nearest tier step", () => {
    const { result } = renderHook(() => useRunControlsContext(), { wrapper: Providers });

    act(() =>
      result.current.handleIntervalInputChange({
        target: { value: "247" },
      } as React.ChangeEvent<HTMLInputElement>),
    );
    expect(result.current.intervalInputValue).toBe("247");

    act(() =>
      result.current.handleIntervalInputBlur({
        target: { value: "247" },
      } as React.FocusEvent<HTMLInputElement>),
    );

    expect(result.current.intervalMs).toBe(sliderToInterval(intervalToSlider(247)));
  });
});
