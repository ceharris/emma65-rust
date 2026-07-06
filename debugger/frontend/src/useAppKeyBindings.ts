import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

interface AppKeyBinding {
  matches: (e: KeyboardEvent) => boolean;
  command: string;
}

/**
 * Key bindings effective in every debugger window (main + terminal).
 *
 * `Backquote` (rather than checking `e.key` for "`") is used for the terminal
 * toggle since `e.key` reports the shifted character (e.g. "~" on a US
 * layout) when Shift is held, but `e.code` is layout- and shift-independent.
 */
const APP_KEY_BINDINGS: AppKeyBinding[] = [
  { matches: (e) => e.key === "q" && (e.ctrlKey || e.metaKey), command: "quit" },
  { matches: (e) => e.ctrlKey && e.shiftKey && e.code === "Backquote", command: "toggle_terminal_visibility" },
];

/** Installs the app-wide key bindings above in the current window. */
export function useAppKeyBindings() {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const binding = APP_KEY_BINDINGS.find((b) => b.matches(e));
      if (!binding) return;
      e.preventDefault();
      invoke(binding.command).catch((err) => console.error(`${binding.command} failed:`, err));
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);
}
