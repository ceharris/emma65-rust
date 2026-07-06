import { createContext, useContext, useEffect, useState, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type ThemeMode = "auto" | "dark" | "light";
type ResolvedTheme = "dark" | "light";

interface ThemeContextValue {
  mode: ThemeMode;
  resolvedTheme: ResolvedTheme;
  setTheme: (mode: ThemeMode) => void;
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

/** Resolves a theme mode to an actual dark/light theme given the OS preference. */
export function resolveTheme(mode: ThemeMode, prefersDark: boolean): ResolvedTheme {
  return mode === "auto" ? (prefersDark ? "dark" : "light") : mode;
}

/**
 * Provides the current theme mode and resolved (dark/light) theme, and keeps
 * `document.documentElement`'s `data-theme` attribute in sync so `global.scss`
 * can apply the right palette. Reads the persisted preference on mount,
 * tracks OS changes via `matchMedia` while in Auto mode (for non-CSS
 * consumers like xterm.js — CSS itself reacts to the OS natively), and stays
 * in sync with other windows via the `theme-changed` event.
 */
export function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setMode] = useState<ThemeMode>("auto");
  const [prefersDark, setPrefersDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches
  );

  useEffect(() => {
    invoke<ThemeMode>("get_theme").then(setMode).catch((err) => console.error("get_theme failed:", err));
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<ThemeMode>("theme-changed", (event) => {
      setMode(event.payload);
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (e: MediaQueryListEvent) => setPrefersDark(e.matches);
    media.addEventListener("change", handler);
    return () => media.removeEventListener("change", handler);
  }, []);

  useEffect(() => {
    if (mode === "auto") {
      document.documentElement.removeAttribute("data-theme");
    } else {
      document.documentElement.setAttribute("data-theme", mode);
    }
  }, [mode]);

  const setTheme = (next: ThemeMode) => {
    invoke("set_theme", { mode: next }).catch((err) => console.error("set_theme failed:", err));
  };

  const resolvedTheme = resolveTheme(mode, prefersDark);

  return (
    <ThemeContext.Provider value={{ mode, resolvedTheme, setTheme }}>
      {children}
    </ThemeContext.Provider>
  );
}

/** Returns the current theme mode/resolved theme and a setter. Must be used within a `ThemeProvider`. */
export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) throw new Error("useTheme must be used within a ThemeProvider");
  return ctx;
}
