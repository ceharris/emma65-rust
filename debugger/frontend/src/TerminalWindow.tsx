import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { Terminal, ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import "@xterm/xterm/css/xterm.css";
import { useAppKeyBindings, APP_KEY_BINDINGS } from "./useAppKeyBindings";
import { resolveTheme, ThemeMode } from "./ThemeContext";

const XTERM_DARK_THEME: ITheme = {
  background: "#1e1e1e",
  foreground: "#d4d4d4",
  cursor: "#d4d4d4",
};

const XTERM_LIGHT_THEME: ITheme = {
  background: "#ffffff",
  foreground: "#1e1e1e",
  cursor: "#1e1e1e",
};

export default function TerminalWindow() {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const [mode, setMode] = useState<ThemeMode>("auto");
  const [prefersDark, setPrefersDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches
  );

  useAppKeyBindings();

  // This window has its own document — React context can't cross the window
  // boundary — so it tracks the theme mode independently, mirroring ThemeProvider.
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
    if (termRef.current) {
      termRef.current.options.theme = resolvedTheme === "dark" ? XTERM_DARK_THEME : XTERM_LIGHT_THEME;
    }
  }, [resolvedTheme]);

  useEffect(() => {
    const monoFont =
      getComputedStyle(document.documentElement).getPropertyValue('--font-mono').trim()
      || 'monospace';
    const term = new Terminal({
      cols: 80,
      rows: 24,
      theme: resolveTheme(mode, prefersDark) === "dark" ? XTERM_DARK_THEME : XTERM_LIGHT_THEME,
      fontFamily: monoFont,
      fontSize: 14,
    });
    termRef.current = term;

    // Let app-wide shortcuts (e.g. Ctrl+Q) bypass xterm's own key handling —
    // otherwise xterm treats them as terminal control input (Ctrl+Q is XON)
    // and stops the keydown from ever reaching the window-level listener.
    //
    // Ctrl+Shift+C/V are handled here rather than via APP_KEY_BINDINGS because
    // they need direct access to this xterm instance (selection, paste), not
    // just a backend command to invoke.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type === "keydown" && e.ctrlKey && e.shiftKey && e.code === "KeyC") {
        const selection = term.getSelection();
        if (selection) {
          writeText(selection).catch((err) => console.error("copy to clipboard failed:", err));
        }
        return false;
      }
      if (e.type === "keydown" && e.ctrlKey && e.shiftKey && e.code === "KeyV") {
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

    term.onData((data) => {
      const bytes = Array.from(new TextEncoder().encode(data));
      invoke("write_terminal", { bytes }).catch(() => {});
    });

    // Register the output listener, then signal the backend that we are ready.
    // The backend will not start the CPU until it receives this signal.
    const unlistenPromise = listen<number[]>("terminal-output", (event) => {
      term.write(new Uint8Array(event.payload));
    }).then((unlisten) => {
      invoke("terminal_ready").catch(() => {});
      return unlisten;
    });

    return () => {
      unlistenPromise.then((f) => f());
      termRef.current = null;
      term.dispose();
    };
  }, []);

  return <div ref={containerRef} className="terminal-container" />;
}
