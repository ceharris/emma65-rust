import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { Terminal, ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import "@xterm/xterm/css/xterm.css";
import { APP_KEY_BINDINGS } from "./useAppKeyBindings";
import { useTheme } from "./ThemeContext";

const XTERM_DARK_THEME: ITheme = {
  background: "#1e1e1e",
  foreground: "#d4d4d4",
  cursor: "#d4d4d4",
  // xterm falls back to a default that isn't visible against every
  // background it ships with, so this must be set explicitly — matches
  // $dark-palette's bg-selected in global.scss.
  selectionBackground: "#094771",
};

const XTERM_LIGHT_THEME: ITheme = {
  background: "#ffffff",
  foreground: "#1e1e1e",
  cursor: "#1e1e1e",
  // See XTERM_DARK_THEME — matches $light-palette's bg-selected.
  selectionBackground: "#cce4f7",
};

/**
 * The dock panel hosting the emulator's console. Shaped for reuse: it reads
 * the theme via `useTheme()` rather than tracking it independently, so both
 * the main window's docked instance and the detached-window host
 * (`terminal-detached.tsx`, issue #385) just need their own `ThemeProvider`
 * ancestor, the same way `main.tsx` wraps `App`. Global key bindings
 * (`useAppKeyBindings`) are installed by the host document, not here — the
 * main window installs them once at `App.tsx`'s root, and
 * `terminal-detached.tsx` does the same for its own window.
 */
export default function TerminalPanel() {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const { resolvedTheme } = useTheme();

  useEffect(() => {
    if (termRef.current) {
      termRef.current.options.theme = resolvedTheme === "dark" ? XTERM_DARK_THEME : XTERM_LIGHT_THEME;
    }
  }, [resolvedTheme]);

  // Not yet a live preference (see `UiConfig::terminal_scrollback`'s doc
  // comment) — fetched once and applied to the already-constructed
  // terminal, same pattern as the theme-sync effect above. xterm.js
  // supports changing `options.scrollback` on a live instance, trimming or
  // growing its buffer accordingly, so this doesn't need to gate the
  // terminal's initial construction below on the fetch completing first.
  useEffect(() => {
    invoke<number>("get_terminal_scrollback")
      .then((lines) => {
        if (termRef.current) termRef.current.options.scrollback = lines;
      })
      .catch((err) => console.error("get_terminal_scrollback failed:", err));
  }, []);

  useEffect(() => {
    const monoFont =
      getComputedStyle(document.documentElement).getPropertyValue('--font-mono').trim()
      || 'monospace';
    const term = new Terminal({
      cols: 80,
      rows: 24,
      theme: resolvedTheme === "dark" ? XTERM_DARK_THEME : XTERM_LIGHT_THEME,
      fontFamily: monoFont,
      fontSize: 14,
    });
    termRef.current = term;

    // Let app-wide shortcuts (e.g. Ctrl+Shift+T/Ctrl+Shift+Y) bypass xterm's
    // own key handling — otherwise xterm treats them as terminal control
    // input and stops the keydown from ever reaching the window-level
    // listener. Ctrl+Q is deliberately NOT one of these (issue #351): it's
    // scoped to the main window only, so xterm is free to treat it as XON
    // here, same as any other terminal.
    //
    // Ctrl+Shift+C/V are handled here rather than via APP_KEY_BINDINGS because
    // they need direct access to this xterm instance (selection, paste), not
    // just a backend command to invoke.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type === "keydown" && e.ctrlKey && e.shiftKey && e.code === "KeyC") {
        // Without preventDefault, the underlying textarea's native paste/copy
        // key binding (e.g. GTK's own Ctrl+Shift+C/V editing commands) fires
        // in addition to this handler, double-actioning the clipboard.
        e.preventDefault();
        const selection = term.getSelection();
        if (selection) {
          writeText(selection).catch((err) => console.error("copy to clipboard failed:", err));
        }
        return false;
      }
      if (e.type === "keydown" && e.ctrlKey && e.shiftKey && e.code === "KeyV") {
        e.preventDefault();
        readText()
          .then((text) => {
            if (text) term.paste(text);
          })
          .catch((err) => console.error("paste from clipboard failed:", err));
        return false;
      }
      return !APP_KEY_BINDINGS.some((b) => b.matches(e));
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(containerRef.current!);
    fitAddon.fit();

    // A dockview split drag resizes this panel's container without resizing
    // the OS window, so window-resize alone (the old standalone Terminal
    // window's refit trigger) wouldn't cover it here — this observer handles
    // both split-drag and inactive→active tab transitions.
    const resizeObserver = new ResizeObserver(() => fitAddon.fit());
    resizeObserver.observe(containerRef.current!);

    term.onData((data) => {
      const bytes = Array.from(new TextEncoder().encode(data));
      invoke("write_terminal", { bytes }).catch(() => {});
    });

    // Replays recent console output before registering the live listener,
    // so a freshly (re)mounted terminal — the very first one at startup, or
    // any later detach/reattach cycle (issue #385) — catches up to the
    // current session instead of starting blank. A byte emitted live in the
    // narrow gap between the history fetch resolving and the listener
    // actually registering could in principle be missed; accepted as the
    // same class of narrow race already tolerated elsewhere in #385 (e.g.
    // the detach/reattach `emit_to`-retarget race).
    const attachOutputListener = () =>
      listen<number[]>("terminal-output", (event) => {
        term.write(new Uint8Array(event.payload));
      });
    const unlistenPromise = invoke<number[]>("get_terminal_history")
      .then((history) => {
        if (history.length > 0) term.write(new Uint8Array(history));
      })
      .catch((err) => console.error("get_terminal_history failed:", err))
      .then(attachOutputListener);

    return () => {
      unlistenPromise.then((f) => f());
      resizeObserver.disconnect();
      termRef.current = null;
      term.dispose();
    };
  }, []);

  return <div ref={containerRef} className="terminal-container" />;
}
