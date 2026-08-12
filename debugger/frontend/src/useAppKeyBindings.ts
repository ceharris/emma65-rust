import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emitTo } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** Label of the main window, per `MAIN_WINDOW_LABEL` in `debugger/src-tauri/src/lib.rs`. */
const MAIN_WINDOW_LABEL = "main";

export interface AppKeyBinding {
  matches: (e: KeyboardEvent) => boolean;
  /** Runs the binding's action. Called with `preventDefault()` already applied to the event. */
  run: () => void;
  /**
   * True if this binding's shortcut is also a native menu accelerator (see
   * `menu.rs`) that fires the same action from the main window. The main
   * window's copy of this handler skips such bindings, since the native
   * accelerator + `on_menu_event` already covers it there — running it again
   * from here would double-fire. Windows without the app menu (Terminal)
   * still rely on this handler as their only path.
   */
  hasMainWindowAccelerator?: boolean;
}

/**
 * Reveals `panelId`'s dock tab in the main window by emitting `reveal-panel`
 * there — used for both Trace and Log, which (since #383) are dockview
 * panels rather than their own window, so there's no `AppHandle` window
 * label to show()/hide() any more. Works from any window: the main window's
 * own copy is skipped in favor of the native accelerator (see
 * `hasMainWindowAccelerator` above), but Terminal's copy has no direct
 * access to the main window's dockview instance (separate webview, separate
 * JS runtime) and needs this cross-window emit either way.
 */
function revealPanel(panelId: "trace" | "log") {
  emitTo(MAIN_WINDOW_LABEL, "reveal-panel", panelId).catch((err) =>
    console.error(`emitTo reveal-panel(${panelId}) failed:`, err),
  );
}

/**
 * Key bindings effective in every debugger window (main + terminal).
 *
 * Terminal was originally Ctrl+Shift+` (VS Code's terminal-toggle shortcut),
 * but GTK can't deliver a working native menu accelerator for Shift+backtick
 * (see the long comment in `menu.rs`), so it was moved to the letter-based
 * Ctrl+Shift+T, bumping the previous Trace binding to Ctrl+Shift+Y.
 *
 * Exported so `TerminalWindow` can exclude these combos from xterm's own key
 * handling via `attachCustomKeyEventHandler` — xterm otherwise treats
 * Ctrl+Shift+letter combos as terminal control input and stops them from
 * ever bubbling to the window-level listener below.
 */
export const APP_KEY_BINDINGS: AppKeyBinding[] = [
  {
    matches: (e) => e.ctrlKey && e.shiftKey && e.code === "KeyT",
    run: () => {
      invoke("toggle_terminal_visibility").catch((err) =>
        console.error("toggle_terminal_visibility failed:", err),
      );
    },
    hasMainWindowAccelerator: true,
  },
  {
    matches: (e) => e.ctrlKey && e.shiftKey && e.code === "KeyY",
    run: () => revealPanel("trace"),
    hasMainWindowAccelerator: true,
  },
  {
    matches: (e) => e.ctrlKey && e.shiftKey && e.code === "KeyL",
    run: () => revealPanel("log"),
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
      binding.run();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);
}
