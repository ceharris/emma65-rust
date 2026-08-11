import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** Label of the main window, per `MAIN_WINDOW_LABEL` in `debugger/src-tauri/src/lib.rs`. */
const MAIN_WINDOW_LABEL = "main";

export interface AppKeyBinding {
  matches: (e: KeyboardEvent) => boolean;
  command: string;
  /**
   * True if this binding's shortcut is also a native menu accelerator (see
   * `menu.rs`) that fires the same command from the main window. The main
   * window's copy of this handler skips such bindings, since the native
   * accelerator + `on_menu_event` already covers it there — invoking again
   * from here would double-toggle the target window. Windows without the app
   * menu (Terminal/Trace/Log) still rely on this handler as their only path.
   */
  hasMainWindowAccelerator?: boolean;
}

/**
 * Key bindings effective in every debugger window (main + terminal).
 *
 * `Backquote` (rather than checking `e.key` for "`") is used for the terminal
 * toggle since `e.key` reports the shifted character (e.g. "~" on a US
 * layout) when Shift is held, but `e.code` is layout- and shift-independent.
 *
 * Exported so `TerminalWindow` can exclude these combos from xterm's own key
 * handling via `attachCustomKeyEventHandler` — xterm otherwise treats
 * Ctrl+Shift+letter combos as terminal control input and stops them from
 * ever bubbling to the window-level listener below.
 */
export const APP_KEY_BINDINGS: AppKeyBinding[] = [
  {
    matches: (e) => e.ctrlKey && e.shiftKey && e.code === "Backquote",
    command: "toggle_terminal_visibility",
    hasMainWindowAccelerator: true,
  },
  {
    matches: (e) => e.ctrlKey && e.shiftKey && e.code === "KeyT",
    command: "toggle_trace_visibility",
    hasMainWindowAccelerator: true,
  },
  {
    matches: (e) => e.ctrlKey && e.shiftKey && e.code === "KeyL",
    command: "toggle_log_visibility",
    hasMainWindowAccelerator: true,
  },
];

/** Installs the app-wide key bindings above in the current window. */
export function useAppKeyBindings() {
  useEffect(() => {
    const isMainWindow = getCurrentWindow().label === MAIN_WINDOW_LABEL;
    const handler = (e: KeyboardEvent) => {
      const binding = APP_KEY_BINDINGS.find(
        (b) => b.matches(e) && !(isMainWindow && b.hasMainWindowAccelerator),
      );
      if (!binding) return;
      e.preventDefault();
      invoke(binding.command).catch((err) => console.error(`${binding.command} failed:`, err));
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);
}
