import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

export default function TerminalWindow() {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "q" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        getCurrentWindow().close();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  useEffect(() => {
    const term = new Terminal({
      cols: 80,
      rows: 24,
      theme: {
        background: "#1e1e1e",
        foreground: "#d4d4d4",
        cursor: "#d4d4d4",
      },
      fontFamily: "monospace",
      fontSize: 14,
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
      term.dispose();
    };
  }, []);

  return <div ref={containerRef} className="terminal-container" />;
}
