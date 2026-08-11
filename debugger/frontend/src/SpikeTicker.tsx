import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

/**
 * Phase 0 spike only (issue #379). Displays the backend `spike-tick` counter
 * so a human can visually confirm no gaps in the sequence across a detach/
 * reattach cycle — the `emit_to`-retargeting prototype (spike question 5).
 */
export default function SpikeTicker() {
  const [n, setN] = useState<number | null>(null);

  useEffect(() => {
    const unlistenPromise = listen<number>("spike-tick", (event) => setN(event.payload));
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  return (
    <div style={{ padding: "4px 8px", fontFamily: "monospace", fontSize: "0.8rem", opacity: 0.8 }}>
      spike-tick: {n ?? "…"}
    </div>
  );
}
