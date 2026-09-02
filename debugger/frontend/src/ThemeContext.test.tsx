import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { emitMockEvent, invoke, resetTauriMocks } from "./test/tauriMock";
import { resolveTheme, ThemeMode, ThemeProvider, useTheme } from "./ThemeContext";

/** Installs a minimal `matchMedia` stub (jsdom has none) and returns a way to fire OS-preference changes. */
function mockMatchMedia(initialMatches: boolean) {
  const listeners = new Set<(e: MediaQueryListEvent) => void>();
  const mql = {
    matches: initialMatches,
    media: "(prefers-color-scheme: dark)",
    addEventListener: (_type: string, handler: (e: MediaQueryListEvent) => void) => {
      listeners.add(handler);
    },
    removeEventListener: (_type: string, handler: (e: MediaQueryListEvent) => void) => {
      listeners.delete(handler);
    },
  };
  window.matchMedia = vi.fn().mockReturnValue(mql) as typeof window.matchMedia;
  return {
    fireChange(matches: boolean) {
      mql.matches = matches;
      for (const handler of listeners) handler({ matches } as MediaQueryListEvent);
    },
  };
}

beforeEach(() => {
  resetTauriMocks();
  document.documentElement.removeAttribute("data-theme");
});

describe("resolveTheme", () => {
  it("auto follows the OS preference", () => {
    expect(resolveTheme("auto", true)).toBe("dark");
    expect(resolveTheme("auto", false)).toBe("light");
  });

  it("dark/light modes are fixed regardless of OS preference", () => {
    expect(resolveTheme("dark", false)).toBe("dark");
    expect(resolveTheme("light", true)).toBe("light");
  });
});

describe("useTheme", () => {
  it("throws when used outside a ThemeProvider", () => {
    expect(() => renderHook(() => useTheme())).toThrow(/must be used within a ThemeProvider/);
  });
});

describe("ThemeProvider", () => {
  it("resolves the persisted mode via get_theme and syncs data-theme", async () => {
    mockMatchMedia(false);
    vi.mocked(invoke).mockResolvedValueOnce("dark" as ThemeMode);
    const { result } = renderHook(() => useTheme(), { wrapper: ThemeProvider });

    await waitFor(() => expect(result.current.mode).toBe("dark"));

    expect(invoke).toHaveBeenCalledWith("get_theme");
    expect(result.current.resolvedTheme).toBe("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  });

  it("auto mode removes data-theme and resolves from the OS preference", async () => {
    mockMatchMedia(true);
    vi.mocked(invoke).mockResolvedValueOnce("auto" as ThemeMode);
    document.documentElement.setAttribute("data-theme", "light");
    const { result } = renderHook(() => useTheme(), { wrapper: ThemeProvider });

    await waitFor(() => expect(result.current.mode).toBe("auto"));

    expect(result.current.resolvedTheme).toBe("dark");
    expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
  });

  it("setTheme invokes set_theme with the requested mode", async () => {
    mockMatchMedia(false);
    vi.mocked(invoke).mockResolvedValueOnce("auto" as ThemeMode);
    const { result } = renderHook(() => useTheme(), { wrapper: ThemeProvider });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("get_theme"));

    act(() => {
      result.current.setTheme("dark");
    });

    expect(invoke).toHaveBeenCalledWith("set_theme", { mode: "dark" });
  });

  it("updates mode when a theme-changed event arrives", async () => {
    mockMatchMedia(false);
    vi.mocked(invoke).mockResolvedValueOnce("auto" as ThemeMode);
    const { result } = renderHook(() => useTheme(), { wrapper: ThemeProvider });
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("get_theme"));

    act(() => {
      emitMockEvent("theme-changed", "light" as ThemeMode);
    });

    expect(result.current.mode).toBe("light");
  });
});
