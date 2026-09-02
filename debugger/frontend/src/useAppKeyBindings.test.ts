import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { getCurrentWindow, invoke, resetTauriMocks } from "./test/tauriMock";
import { APP_KEY_BINDINGS, useAppKeyBindings } from "./useAppKeyBindings";

function keydown(init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent("keydown", init);
}

beforeEach(() => {
  resetTauriMocks();
});

describe("APP_KEY_BINDINGS matches predicates", () => {
  const cases: { name: string; index: number; code: string }[] = [
    { name: "Terminal (Ctrl+Shift+T)", index: 0, code: "KeyT" },
    { name: "Display (Ctrl+Shift+D)", index: 1, code: "KeyD" },
    { name: "LED Matrix (Ctrl+Shift+M)", index: 2, code: "KeyM" },
  ];

  for (const { name, index, code } of cases) {
    describe(name, () => {
      it("matches its own Ctrl+Shift+<letter> combo", () => {
        expect(APP_KEY_BINDINGS[index].matches(keydown({ ctrlKey: true, shiftKey: true, code }))).toBe(true);
      });

      it("rejects the combo when Ctrl is missing", () => {
        expect(APP_KEY_BINDINGS[index].matches(keydown({ ctrlKey: false, shiftKey: true, code }))).toBe(false);
      });

      it("rejects the combo when Shift is missing", () => {
        expect(APP_KEY_BINDINGS[index].matches(keydown({ ctrlKey: true, shiftKey: false, code }))).toBe(false);
      });

      it("rejects a different key code", () => {
        expect(
          APP_KEY_BINDINGS[index].matches(keydown({ ctrlKey: true, shiftKey: true, code: "KeyZ" })),
        ).toBe(false);
      });
    });
  }
});

describe("useAppKeyBindings", () => {
  it("skips main-window-accelerator bindings when running in the main window", () => {
    vi.mocked(getCurrentWindow).mockReturnValue({ label: "main" } as ReturnType<typeof getCurrentWindow>);
    renderHook(() => useAppKeyBindings());

    window.dispatchEvent(keydown({ ctrlKey: true, shiftKey: true, code: "KeyT" }));

    expect(invoke).not.toHaveBeenCalled();
  });

  it("reattaches via invoke when Ctrl+Shift+T fires in the detached-Terminal window", () => {
    vi.mocked(getCurrentWindow).mockReturnValue({
      label: "terminal-detached",
    } as ReturnType<typeof getCurrentWindow>);
    vi.mocked(invoke).mockResolvedValue(undefined);
    renderHook(() => useAppKeyBindings());

    window.dispatchEvent(keydown({ ctrlKey: true, shiftKey: true, code: "KeyT" }));

    expect(invoke).toHaveBeenCalledWith("attach_terminal");
  });
});
