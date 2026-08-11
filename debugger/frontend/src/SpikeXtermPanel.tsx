import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

/**
 * Phase 0 spike only (issue #379). A minimal xterm instance with no backend
 * wiring, used to check whether a `ResizeObserver`-driven `fitAddon.fit()`
 * correctly refits when a dockview split is dragged, and when a zero-size
 * inactive tab becomes active (spike question 3). Writes a line every two
 * seconds so a resize's effect on wrapping is visible without typing.
 */
export default function SpikeXtermPanel() {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const term = new Terminal({ cols: 80, rows: 24, fontSize: 14 });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(containerRef.current!);
    fitAddon.fit();
    term.writeln("Drag this panel's split boundary and confirm the terminal refits.");

    const resizeObserver = new ResizeObserver(() => {
      try {
        fitAddon.fit();
      } catch (e) {
        console.error("[spike] fitAddon.fit() failed:", e);
      }
    });
    resizeObserver.observe(containerRef.current!);

    let n = 0;
    const interval = window.setInterval(() => {
      n += 1;
      term.write(`tick ${n}\r\n`);
    }, 2000);

    return () => {
      window.clearInterval(interval);
      resizeObserver.disconnect();
      term.dispose();
    };
  }, []);

  return <div ref={containerRef} style={{ width: "100%", height: "100%" }} />;
}
