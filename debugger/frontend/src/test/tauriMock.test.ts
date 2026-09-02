import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { emitMockEvent, resetTauriMocks } from "./tauriMock";

beforeEach(() => {
  resetTauriMocks();
});

describe("tauriMock", () => {
  it("mocks invoke with a configurable resolved value", async () => {
    vi.mocked(invoke).mockResolvedValueOnce("ok");
    await expect(invoke("some_command")).resolves.toBe("ok");
    expect(invoke).toHaveBeenCalledWith("some_command");
  });

  it("defaults getCurrentWindow to the main window label", () => {
    expect(getCurrentWindow().label).toBe("main");
  });

  it("delivers emitMockEvent to handlers registered via listen", async () => {
    const received: unknown[] = [];
    await listen("breakpoints-changed", (event) => received.push(event.payload));

    emitMockEvent("breakpoints-changed", { address: 0x1000 });

    expect(received).toEqual([{ address: 0x1000 }]);
  });

  it("stops delivering events after the unlisten function runs", async () => {
    const received: unknown[] = [];
    const unlisten = await listen("breakpoints-changed", (event) => received.push(event.payload));
    unlisten();

    emitMockEvent("breakpoints-changed", { address: 0x2000 });

    expect(received).toEqual([]);
  });

  it("resolves emitTo without a backing implementation", async () => {
    await expect(emitTo("main", "reveal-panel", "terminal")).resolves.toBeUndefined();
    expect(emitTo).toHaveBeenCalledWith("main", "reveal-panel", "terminal");
  });
});
