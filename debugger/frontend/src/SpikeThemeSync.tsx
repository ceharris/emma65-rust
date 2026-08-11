import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { resolveTheme, ThemeMode } from "./ThemeContext";

/**
 * Phase 0 spike only (issue #379). The spike windows are separate documents
 * — React context can't cross a window boundary — so, like `TerminalWindow`/
 * `LogPanel`/`TracePanel`, each one tracks the debugger's selected theme
 * independently rather than inheriting `ThemeProvider`. Missing on the first
 * pass of this spike: `EMMA65_SPIKE_THEME` was hardcoded to `colorScheme:
 * "dark"` and nothing set `data-theme` on this window's document, so the
 * spike windows never actually followed the debugger's selected theme.
 */
export default function SpikeThemeSync({ onChange }: { onChange?: (theme: "dark" | "light") => void }) {
  const [mode, setMode] = useState<ThemeMode>("auto");
  const [prefersDark, setPrefersDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches
  );

  useEffect(() => {
    invoke<ThemeMode>("get_theme").then(setMode).catch((err) => console.error("get_theme failed:", err));

    const unlistenPromise = listen<ThemeMode>("theme-changed", (event) => {
      setMode(event.payload);
    });

    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = (e: MediaQueryListEvent) => setPrefersDark(e.matches);
    media.addEventListener("change", handler);

    return () => {
      unlistenPromise.then((f) => f());
      media.removeEventListener("change", handler);
    };
  }, []);

  const resolvedTheme = resolveTheme(mode, prefersDark);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", resolvedTheme);
    onChange?.(resolvedTheme);
  }, [resolvedTheme, onChange]);

  return null;
}
