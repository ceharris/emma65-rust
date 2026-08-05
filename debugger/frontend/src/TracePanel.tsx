import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { useAppKeyBindings } from "./useAppKeyBindings";
import { resolveTheme, ThemeMode } from "./ThemeContext";
import "./styles/trace.scss";

interface TraceBusOpDto {
  addr: number;
  op: string;
  value: number;
}

interface TraceRowDto {
  seq: number;
  addr: number;
  bytes: string[];
  labels: string[];
  mnemonic: string;
  operand: string;
  comment: string;
  is_valid: boolean;
  cycles: number | null;
  a: number;
  x: number;
  y: number;
  s: number;
  p: number;
  pc: number;
  bus_ops: TraceBusOpDto[];
}

interface TraceWindowPage {
  rows: TraceRowDto[];
  total_rows: number;
}

interface TraceStatus {
  recording: boolean;
  path: string | null;
}

/** Fixed number of rows shown in the windowed log viewport. */
const VIEWPORT_ROWS = 40;

/** `(letter, bit)` pairs for the P register, in NV-BDIZC display order. */
const FLAG_BITS: [string, number][] = [
  ["N", 0x80], ["V", 0x40], ["B", 0x10], ["D", 0x08], ["I", 0x04], ["Z", 0x02], ["C", 0x01],
];

/** Formats P as an 8-char NV-BDIZC field, with a blank where the always-set UNUSED bit would render. */
function formatFlags(p: number): string {
  const chars = FLAG_BITS.map(([c, bit]) => (p & bit ? c : " "));
  chars.splice(2, 0, " ");
  return chars.join("");
}

function formatAddr(addr: number): string {
  return addr.toString(16).toUpperCase().padStart(4, "0");
}

function formatByte(b: number): string {
  return b.toString(16).toUpperCase().padStart(2, "0");
}

function formatBytes(bytes: string[]): string {
  return bytes.map((b) => b.padStart(2, "0")).join(" ").padEnd(8, " ");
}

/** Basename of a file path, for the compact toolbar path display. */
function basename(path: string): string {
  return path.split(/[/\\]/).pop() ?? path;
}

export default function TracePanel() {
  const [mode, setMode] = useState<ThemeMode>("auto");
  const [prefersDark, setPrefersDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches
  );
  const [recording, setRecording] = useState(false);
  const [tracePath, setTracePath] = useState<string | null>(null);
  const [paused, setPaused] = useState(false);
  const [cpuRunning, setCpuRunning] = useState(false);
  const [rows, setRows] = useState<TraceRowDto[]>([]);
  const [startRow, setStartRow] = useState(0);
  const [totalRows, setTotalRows] = useState(0);
  const [selectedSeq, setSelectedSeq] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Refs mirror state so event listeners and callbacks always see the latest
  // values without needing to be re-created (same pattern as DisassemblyPanel).
  const recordingRef = useRef(false);
  const pausedRef = useRef(false);
  const totalRowsRef = useRef(0);
  const startRowRef = useRef(0);
  recordingRef.current = recording;
  pausedRef.current = paused;
  totalRowsRef.current = totalRows;
  startRowRef.current = startRow;

  useAppKeyBindings();

  // This window has its own document — React context can't cross the window
  // boundary — so it tracks the theme mode independently, mirroring TerminalWindow.
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

  /** Fetch `VIEWPORT_ROWS` rows starting at `start` and replace the displayed window. */
  const fetchWindow = useCallback(async (start: number) => {
    try {
      const page = await invoke<TraceWindowPage>("get_trace_window", { startRow: start, count: VIEWPORT_ROWS });
      setRows(page.rows);
      setTotalRows(page.total_rows);
      setStartRow(start);
    } catch (e) {
      console.error("get_trace_window failed:", e);
    }
  }, []);

  /** Re-fetches the tail window, following the live end of the recording. */
  const fetchTail = useCallback(() => {
    return fetchWindow(Math.max(0, totalRowsRef.current - VIEWPORT_ROWS));
  }, [fetchWindow]);

  // Restore toolbar state on mount (e.g. the window was hidden and reopened
  // mid-recording), mirroring App.tsx's get_session_status restore-on-load.
  useEffect(() => {
    invoke<TraceStatus>("get_trace_status")
      .then((status) => {
        setRecording(status.recording);
        setTracePath(status.path);
        if (status.path) fetchTail();
      })
      .catch((e) => console.error("get_trace_status failed:", e));
  }, [fetchTail]);

  // Track whether the CPU is currently free-running, to gate Record/Stop/Pause
  // the same way other panels gate controls on execState !== "stopped".
  useEffect(() => {
    const unlistenTick = listen("debugger-running-tick", () => setCpuRunning(true));
    const unlistenRunStopped = listen("debugger-run-stopped", () => setCpuRunning(false));
    return () => {
      unlistenTick.then((f) => f());
      unlistenRunStopped.then((f) => f());
    };
  }, []);

  // Live-follow: while recording and not paused, re-fetch the tail window on
  // every halt (each step) and run-stop (end of a free run).
  useEffect(() => {
    const unlistenHalted = listen("debugger-halted", () => {
      if (recordingRef.current && !pausedRef.current) fetchTail();
    });
    const unlistenRunStopped = listen("debugger-run-stopped", () => {
      if (recordingRef.current && !pausedRef.current) fetchTail();
    });
    return () => {
      unlistenHalted.then((f) => f());
      unlistenRunStopped.then((f) => f());
    };
  }, [fetchTail]);

  const handleRecord = useCallback(async () => {
    const path = await save({ filters: [{ name: "Trace Files", extensions: ["trace"] }] });
    if (!path) return;
    try {
      await invoke("record_trace", { path });
      setRecording(true);
      setTracePath(path);
      setPaused(false);
      setSelectedSeq(null);
      await fetchWindow(0);
    } catch (e) {
      setError(String(e));
    }
  }, [fetchWindow]);

  const handleStop = useCallback(async () => {
    try {
      await invoke("stop_trace");
      setRecording(false);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  /** Pause is frontend-only: the file keeps writing, only tail-following stops. */
  const togglePause = useCallback(() => {
    setPaused((prev) => {
      const next = !prev;
      if (!next) fetchTail();
      return next;
    });
  }, [fetchTail]);

  const scrollBy = useCallback(
    (delta: number) => {
      const maxStart = Math.max(0, totalRowsRef.current - 1);
      const next = Math.max(0, Math.min(maxStart, startRowRef.current + delta));
      fetchWindow(next);
    },
    [fetchWindow],
  );

  const handleWheel = useCallback(
    (e: React.WheelEvent) => {
      e.preventDefault();
      scrollBy(e.deltaY > 0 ? 1 : -1);
    },
    [scrollBy],
  );

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (document.activeElement instanceof HTMLInputElement) return;
      if (e.key === "ArrowDown") scrollBy(1);
      else if (e.key === "ArrowUp") scrollBy(-1);
      else if (e.key === "PageDown") scrollBy(VIEWPORT_ROWS);
      else if (e.key === "PageUp") scrollBy(-VIEWPORT_ROWS);
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [scrollBy]);

  // Custom scrollbar: the log is a fixed-size window fetched from the
  // backend (not a real scrollable DOM list, since total_rows can be huge),
  // so a native scrollbar has nothing to attach to. This track+thumb gives
  // the same "see how much history there is, and jump anywhere in it" UX.
  const scrollbarTrackRef = useRef<HTMLDivElement | null>(null);

  /** Maps a track-relative Y coordinate to a row and, if it differs, fetches that window. */
  const jumpToClientY = useCallback(
    (clientY: number) => {
      const track = scrollbarTrackRef.current;
      const total = totalRowsRef.current;
      const maxStart = Math.max(0, total - VIEWPORT_ROWS);
      if (!track || maxStart <= 0) return;
      const rect = track.getBoundingClientRect();
      const thumbFrac = Math.max(0.04, Math.min(1, VIEWPORT_ROWS / total));
      const usable = rect.height * (1 - thumbFrac);
      const thumbHalf = (rect.height * thumbFrac) / 2;
      const frac = usable > 0 ? Math.max(0, Math.min(1, (clientY - rect.top - thumbHalf) / usable)) : 0;
      const next = Math.round(frac * maxStart);
      if (next !== startRowRef.current) fetchWindow(next);
    },
    [fetchWindow],
  );

  const handleThumbMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const onMove = (ev: MouseEvent) => jumpToClientY(ev.clientY);
      const onUp = () => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [jumpToClientY],
  );

  const handleTrackMouseDown = useCallback(
    (e: React.MouseEvent) => {
      jumpToClientY(e.clientY);
    },
    [jumpToClientY],
  );

  const selectedRow = rows.find((r) => r.seq === selectedSeq) ?? null;
  const controlsDisabled = cpuRunning;
  const maxStart = Math.max(0, totalRows - VIEWPORT_ROWS);
  const thumbHeightPct = totalRows > 0 ? Math.max(4, Math.min(100, (VIEWPORT_ROWS / totalRows) * 100)) : 100;
  const thumbTopPct = maxStart > 0 ? (startRow / maxStart) * (100 - thumbHeightPct) : 0;

  return (
    <div className="trace-panel">
      <div className="trace-toolbar">
        <button
          className="trace-btn trace-record-btn"
          onClick={handleRecord}
          disabled={recording || controlsDisabled}
          title="Record trace to a new file"
        >
          <i className="codicon codicon-record" />
        </button>
        <button
          className="trace-btn trace-stop-btn"
          onClick={handleStop}
          disabled={!recording || controlsDisabled}
          title="Stop recording"
        >
          <i className="codicon codicon-debug-stop" />
        </button>
        <button
          className={`trace-btn trace-pause-btn${paused ? " active" : ""}`}
          onClick={togglePause}
          disabled={!recording || controlsDisabled}
          title={paused ? "Resume following the live tail" : "Pause (stop following the live tail)"}
        >
          <i className={`codicon codicon-${paused ? "play" : "debug-pause"}`} />
        </button>
        <span className="trace-path">{tracePath ? basename(tracePath) : "No trace recorded"}</span>
        <span className="trace-row-count">{totalRows} rows</span>
      </div>
      <div className="trace-log-area">
        <div className="trace-log" onWheel={handleWheel}>
          <table className="trace-table">
            <colgroup>
              <col className="col-seq" />
              <col className="col-cyc" />
              <col className="col-reg" />
              <col className="col-reg" />
              <col className="col-reg" />
              <col className="col-reg" />
              <col className="col-reg" />
              <col className="col-flags" />
              <col className="col-addr" />
              <col className="col-bytes" />
              <col className="col-mnemonic" />
              <col className="col-operand" />
              <col />
            </colgroup>
            <thead>
              <tr>
                <th>Seq#</th>
                <th>Cyc</th>
                <th>A</th>
                <th>X</th>
                <th>Y</th>
                <th>S</th>
                <th>P</th>
                <th>Flags</th>
                <th>Addr</th>
                <th>Bytes</th>
                <th>Instr</th>
                <th>Operand</th>
                <th>Comment</th>
              </tr>
            </thead>
            <tbody>
              {rows.length === 0 ? (
                <tr className="trace-empty-row">
                  <td colSpan={13} className="trace-empty">
                    {recording ? "Waiting for instructions…" : "Not recording"}
                  </td>
                </tr>
              ) : (
                rows.flatMap((row) => {
                  const labelRows = row.labels.map((label, i) => (
                    <tr key={`${row.seq}-label-${i}`} className="trace-row trace-label-row">
                      <td colSpan={8} />
                      <td colSpan={5} className="trace-label">
                        {label}:
                      </td>
                    </tr>
                  ));
                  const instrRow = (
                    <tr
                      key={row.seq}
                      className={`trace-row${row.seq === selectedSeq ? " selected" : ""}${row.is_valid ? "" : " invalid-op"}`}
                      onClick={() => setSelectedSeq(row.seq)}
                    >
                      <td className="trace-seq">{row.seq}</td>
                      <td className="trace-cyc">{row.cycles ?? ""}</td>
                      <td className="trace-reg">{formatByte(row.a)}</td>
                      <td className="trace-reg">{formatByte(row.x)}</td>
                      <td className="trace-reg">{formatByte(row.y)}</td>
                      <td className="trace-reg">{formatByte(row.s)}</td>
                      <td className="trace-reg">{formatByte(row.p)}</td>
                      <td className="trace-flags">{formatFlags(row.p)}</td>
                      <td className="trace-addr">{formatAddr(row.addr)}</td>
                      <td className="trace-bytes">{formatBytes(row.bytes)}</td>
                      <td className="trace-mnemonic">{row.mnemonic}</td>
                      <td className="trace-operand">{row.operand}</td>
                      <td className="trace-comment">{row.comment}</td>
                    </tr>
                  );
                  return [...labelRows, instrRow];
                })
              )}
            </tbody>
          </table>
        </div>
        {totalRows > VIEWPORT_ROWS && (
          <div className="trace-scrollbar" ref={scrollbarTrackRef} onMouseDown={handleTrackMouseDown}>
            <div
              className="trace-scrollbar-thumb"
              style={{ height: `${thumbHeightPct}%`, top: `${thumbTopPct}%` }}
              onMouseDown={handleThumbMouseDown}
            />
          </div>
        )}
      </div>
      <div className="trace-detail">
        {selectedRow ? (
          selectedRow.bus_ops.length === 0 ? (
            <span className="trace-detail-empty">No bus operations recorded for this instruction.</span>
          ) : (
            selectedRow.bus_ops.map((op, i) => (
              <div key={i} className="trace-detail-row">
                <span className="trace-detail-addr">{formatAddr(op.addr)}</span>
                <span className="trace-detail-op">{op.op}</span>
                <span className="trace-detail-value">{formatByte(op.value)}</span>
              </div>
            ))
          )
        ) : (
          <span className="trace-detail-empty">Select a row to see its bus operations.</span>
        )}
      </div>
      {error && (
        <div className="trace-error-backdrop" onClick={() => setError(null)}>
          <div className="trace-error-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="trace-error-title">Trace Error</div>
            <div className="trace-error-message">{error}</div>
            <div className="trace-error-buttons">
              <button className="trace-error-btn" onClick={() => setError(null)}>
                Dismiss
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
