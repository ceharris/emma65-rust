import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emitTo } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** Label of the main window, per `MAIN_WINDOW_LABEL` in `debugger/src-tauri/src/lib.rs`. */
const MAIN_WINDOW_LABEL = "main";

/** Label of the detached-Terminal window, per `TERMINAL_DETACHED_WINDOW_LABEL` in `debugger/src-tauri/src/terminal.rs`. */
const TERMINAL_DETACHED_WINDOW_LABEL = "terminal-detached";

export interface AppKeyBinding {
  matches: (e: KeyboardEvent) => boolean;
  /** Runs the binding's action. Called with `preventDefault()` already applied to the event. */
  run: () => void;
  /**
   * True if this binding's shortcut is also a native menu accelerator (see
   * `menu.rs`) that fires the same action from the main window. The main
   * window's copy of this handler skips such bindings, since the native
   * accelerator + `on_menu_event` already covers it there — running it again
   * from here would double-fire. A window without the app menu would still
   * rely on this handler as its only path (see Terminal's pre-#384 history).
   */
  hasMainWindowAccelerator?: boolean;
}

/**
 * Reveals `panelId`'s dock tab in the main window by emitting `reveal-panel`
 * there — used for Trace, Log, and (since #384) Terminal, which are all
 * dockview panels rather than their own window, so there's no `AppHandle`
 * window label to show()/hide() any more. The main window's own copy is
 * skipped in favor of the native accelerator (see `hasMainWindowAccelerator`
 * above); a future detached-window host for Terminal (#385) would need this
 * cross-window emit either way, since it wouldn't have direct access to the
 * main window's dockview instance (separate webview, separate JS runtime).
 */
function revealPanel(panelId: "trace" | "log" | "terminal") {
  emitTo(MAIN_WINDOW_LABEL, "reveal-panel", panelId).catch((err) =>
    console.error(`emitTo reveal-panel(${panelId}) failed:`, err),
  );
}

/**
 * Key bindings effective in every debugger window that installs this hook —
 * the main window (`App.tsx`) and the detached-Terminal window
 * (`terminal-detached.tsx`, since #385).
 *
 * Terminal was originally Ctrl+Shift+` (VS Code's terminal-toggle shortcut),
 * but GTK can't deliver a working native menu accelerator for Shift+backtick
 * (see the long comment in `menu.rs`), so it was moved to the letter-based
 * Ctrl+Shift+T, bumping the previous Trace binding to Ctrl+Shift+Y.
 *
 * Exported so `TerminalPanel` can exclude these combos from xterm's own key
 * handling via `attachCustomKeyEventHandler` — xterm otherwise treats
 * Ctrl+Shift+letter combos as terminal control input and stops them from
 * ever bubbling to the window-level listener below.
 */
export const APP_KEY_BINDINGS: AppKeyBinding[] = [
  {
    matches: (e) => e.ctrlKey && e.shiftKey && e.code === "KeyT",
    // From the detached-Terminal window itself (issue #385), Ctrl+Shift+T
    // reattaches rather than revealing — there's no dock tab to reveal from
    // in that window, and it's a much more useful binding there than a
    // no-op. `attach_terminal` is the same reattach path the window's
    // native close button and the Window > "Attach Terminal" menu item use.
    run: () => {
      if (getCurrentWindow().label === TERMINAL_DETACHED_WINDOW_LABEL) {
        invoke("attach_terminal").catch((err) => console.error("attach_terminal failed:", err));
      } else {
        revealPanel("terminal");
      }
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
