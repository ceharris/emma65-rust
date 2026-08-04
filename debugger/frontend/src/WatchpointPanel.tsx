import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/watchpoints.scss";

interface WatchpointRow {
  source: string;
  value: number | null;
  error: string | null;
}

interface WatchpointsSnapshot {
  compile_error: string | null;
  rows: WatchpointRow[];
}

// --- radix cycling ---

type DataRadix = "hex" | "udec" | "sdec" | "oct" | "bin";

const DATA_RADIX_CYCLE: DataRadix[] = ["hex", "udec", "sdec", "oct", "bin"];

const DATA_RADIX_LABEL: Record<DataRadix, string> = {
  hex:  "HEX",
  udec: "DEC",
  sdec: "±DEC",
  oct:  "OCT",
  bin:  "BIN",
};

function formatData(value: number | null, radix: DataRadix): string {
  if (value === null) return "ERR";
  switch (radix) {
    case "hex":  return value.toString(16).toUpperCase().padStart(8, "0");
    case "udec": return value.toString(10);
    case "sdec": return (value | 0).toString(10);
    case "oct":  return value.toString(8);
    case "bin":  return value.toString(2);
  }
}

export default function WatchpointPanel() {
  const [snapshot, setSnapshot] = useState<WatchpointsSnapshot | null>(null);
  const [dataRadix, setDataRadix] = useState<DataRadix>("hex");
  const [expandedIndex, setExpandedIndex] = useState<number | null>(null);

  const fetchWatchpoints = useCallback(async () => {
    try {
      const result = await invoke<WatchpointsSnapshot>("get_watchpoints");
      setSnapshot(result);
    } catch (e) {
      console.error("get_watchpoints failed:", e);
    }
  }, []);

  useEffect(() => {
    fetchWatchpoints();
  }, [fetchWatchpoints]);

  useEffect(() => {
    const unlistenHalted = listen("debugger-halted", () => { fetchWatchpoints(); });
    const unlistenRunStopped = listen("debugger-run-stopped", () => { fetchWatchpoints(); });
    return () => {
      unlistenHalted.then((f) => f());
      unlistenRunStopped.then((f) => f());
    };
  }, [fetchWatchpoints]);

  const cycleDataRadix = useCallback(() => {
    setDataRadix((prevRadix) => {
      const i = DATA_RADIX_CYCLE.indexOf(prevRadix);
      return DATA_RADIX_CYCLE[(i + 1) % DATA_RADIX_CYCLE.length];
    });
  }, []);

  const toggleExpanded = useCallback((index: number) => {
    setExpandedIndex((prev) => (prev === index ? null : index));
  }, []);

  return (
    <div className="watchpoint-panel">
      <div className="watchpoint-header">
        <span className="panel-title">Watchpoints</span>
        <button className="radix-btn" onClick={cycleDataRadix} title="Cycle radix">
          {DATA_RADIX_LABEL[dataRadix]}
        </button>
      </div>
      {snapshot === null ? (
        <span className="watchpoint-empty">Waiting…</span>
      ) : snapshot.compile_error !== null ? (
        <div className="watchpoint-error">{snapshot.compile_error}</div>
      ) : snapshot.rows.length === 0 ? (
        <span className="watchpoint-empty">No watchpoints</span>
      ) : (
        <div className="watchpoint-body">
          {snapshot.rows.map((row, index) => {
            const statusClass = row.error !== null ? "wp-error" : row.value !== 0 ? "wp-true" : "wp-false";
            return (
              <div key={index} className="watchpoint-row">
                <span
                  className={`watchpoint-source${expandedIndex === index ? " expanded" : ""}`}
                  onClick={() => toggleExpanded(index)}
                  title={row.source}
                >
                  {row.source}
                </span>
                <span className="watchpoint-value">{formatData(row.value, dataRadix)}</span>
                <span className={`indicator ${statusClass}`}>●</span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
