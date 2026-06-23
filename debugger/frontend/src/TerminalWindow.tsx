import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

export default function TerminalWindow() {
  const containerRef = useRef<HTMLDivElement>(null);

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

    const unlistenPromise = listen<number[]>("terminal-output", (event) => {
      term.write(new Uint8Array(event.payload));
    });

    // Signal the backend that the listener is registered and output can begin.
    unlistenPromise.then(() => invoke("terminal_ready").catch(() => {}));

    return () => {
      unlistenPromise.then((f) => f());
      term.dispose();
    };
  }, []);

  return <div ref={containerRef} className="terminal-container" />;
}
