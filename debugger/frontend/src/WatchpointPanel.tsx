import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/watchpoints.scss";

interface WatchpointRow {
  source: string;
  triggered: boolean;
  error: string | null;
}

interface WatchpointsSnapshot {
  compile_error: string | null;
  rows: WatchpointRow[];
}

export default function WatchpointPanel() {
  const [snapshot, setSnapshot] = useState<WatchpointsSnapshot | null>(null);
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

  const toggleExpanded = useCallback((index: number) => {
    setExpandedIndex((prev) => (prev === index ? null : index));
  }, []);

  return (
    <div className="watchpoint-panel">
      <div className="watchpoint-header">
        <span className="panel-title">Watchpoints</span>
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
            const statusClass = row.error !== null ? "wp-error" : row.triggered ? "wp-true" : "wp-false";
            return (
              <div key={index} className="watchpoint-row">
                <span className={`indicator ${statusClass}`}>●</span>
                <span
                  className={`watchpoint-source${expandedIndex === index ? " expanded" : ""}`}
                  onClick={() => toggleExpanded(index)}
                  title={row.source}
                >
                  {row.source}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
