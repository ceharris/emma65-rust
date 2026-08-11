import {useEffect, useRef, useState} from "react";
import {listen} from "@tauri-apps/api/event";
import {invoke} from "@tauri-apps/api/core";
import {useAppKeyBindings} from "./useAppKeyBindings";
import {resolveTheme, ThemeMode} from "./ThemeContext";
import "./styles/log.scss";

interface LogRecordDto {
  timestamp_ms: number;
  cycles: number;
  level: string;
  category: string;
  message: string;
}

interface LogRow extends LogRecordDto {
  id: number;
}

/** Cap on the in-memory row buffer, mirroring the backend's `LOG_BUFFER_CAPACITY`. */
const MAX_RECORDS = 5000;

/** Formats `timestamp_ms` as a local-time `HH:MM:SS.mmm` string. */
function formatTimestamp(timestampMs: number): string {
  const d = new Date(timestampMs);
  const pad2 = (n: number) => n.toString().padStart(2, "0");
  const pad3 = (n: number) => n.toString().padStart(3, "0");
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}.${pad3(d.getMilliseconds())}`;
}

/** Maps a level string to its color class suffix. */
function levelClass(level: string): string {
  switch (level) {
    case "WARN":
      return "log-level-warn";
    case "ERROR":
      return "log-level-error";
    default:
      return "log-level-info";
  }
}

function LogColGroup() {
  return (
    <colgroup>
      <col className="col-timestamp" />
      <col className="col-cycles" />
      <col className="col-level" />
      <col className="col-category" />
      <col />
    </colgroup>
  );
}

export default function LogPanel() {
  const [mode, setMode] = useState<ThemeMode>("auto");
  const [prefersDark, setPrefersDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches
  );
  const [records, setRecords] = useState<LogRow[]>([]);

  // Client-side sequential id counter for stable React keys — the DTO itself carries no
  // sequence number, and timestamp_ms alone isn't guaranteed unique across records.
  const nextIdRef = useRef(0);

  useAppKeyBindings();

  // This window has its own document — React context can't cross the window
  // boundary — so it tracks the theme mode independently, mirroring TracePanel.
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
  }, [resolvedTheme]);

  // Hydrate history on mount, then append live records as they're pushed from the backend.
  useEffect(() => {
    invoke<LogRecordDto[]>("get_log_records")
      .then((history) => {
        const rows = history.map((rec) => ({...rec, id: nextIdRef.current++}));
        setRecords(rows.slice(-MAX_RECORDS));
      })
      .catch((err) => console.error("get_log_records failed:", err));

    const unlistenPromise = listen<LogRecordDto>("log-record", (event) => {
      const row: LogRow = {...event.payload, id: nextIdRef.current++};
      setRecords((prev) => {
        const next = [...prev, row];
        return next.length > MAX_RECORDS ? next.slice(next.length - MAX_RECORDS) : next;
      });
    });

    return () => {
      unlistenPromise.then((f) => f());
    };
  }, []);

  // Auto-scroll: always keep the newest record in view, matching the issue's literal wording
  // ("the table scrolls if needed such that newly added messages are visible") — no manual
  // scroll-back/jump feature exists here to conflict with unconditionally following the tail.
  const logAreaRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = logAreaRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [records]);

  return (
    <div className="log-panel">
      <div className="log-log-area" ref={logAreaRef}>
        <table className="log-table">
          <LogColGroup />
          <thead>
            <tr>
              <th>Timestamp</th>
              <th>Cycles</th>
              <th>Level</th>
              <th>Category</th>
              <th>Message</th>
            </tr>
          </thead>
          <tbody>
            {records.length === 0 ? (
              <tr className="log-empty-row">
                <td colSpan={5} className="log-empty">
                  No log messages yet.
                </td>
              </tr>
            ) : (
              records.map((row) => (
                <tr key={row.id} className="log-row">
                  <td className="log-timestamp">{formatTimestamp(row.timestamp_ms)}</td>
                  <td className="log-cycles">{row.cycles}</td>
                  <td className={`log-level ${levelClass(row.level)}`}>{row.level}</td>
                  <td className="log-category">{row.category}</td>
                  <td className="log-message">{row.message}</td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
