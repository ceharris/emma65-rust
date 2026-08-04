import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ExecState } from "./DisassemblyPanel";
import "./styles/watchpoints.scss";

interface WatchpointRow {
  source: string;
  triggered: boolean;
  error: string | null;
  enabled: boolean;
}

interface WatchpointsSnapshot {
  compile_error: string | null;
  rows: WatchpointRow[];
}

/** State for the add-watchpoint popover; null means closed. */
interface AddDialogState {
  /** Controlled value of the expression input. */
  value: string;
  /** Compilation or validation error; empty string means no error. */
  error: string;
}

interface Props {
  /** Current CPU execution state; add/remove are only allowed while stopped. */
  execState: ExecState;
}

export default function WatchpointPanel({ execState }: Props) {
  const [snapshot, setSnapshot] = useState<WatchpointsSnapshot | null>(null);
  const [expandedIndex, setExpandedIndex] = useState<number | null>(null);
  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);
  const [addDialog, setAddDialog] = useState<AddDialogState | null>(null);

  const canEdit = execState === "stopped" && snapshot?.compile_error == null;

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

  /** Click on a row's source text selects it and toggles its expanded state. */
  const handleRowClick = useCallback((index: number) => {
    setSelectedIndex(index);
    setExpandedIndex((prev) => (prev === index ? null : index));
  }, []);

  /** Removes the watchpoint at `index` and clears selection/expansion. */
  const removeWatchpointAt = useCallback(async (index: number) => {
    try {
      const result = await invoke<WatchpointsSnapshot>("remove_watchpoint", { index });
      setSnapshot(result);
      setSelectedIndex(null);
      setExpandedIndex(null);
    } catch (e) {
      console.error("remove_watchpoint failed:", e);
    }
  }, []);

  /** Toggles the enabled state of the watchpoint at `index`. */
  const toggleWatchpointAt = useCallback(async (index: number) => {
    try {
      const result = await invoke<WatchpointsSnapshot>("toggle_watchpoint", { index });
      setSnapshot(result);
    } catch (e) {
      console.error("toggle_watchpoint failed:", e);
    }
  }, []);

  /** Delete key removes the selected watchpoint while the CPU is stopped. */
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (document.activeElement instanceof HTMLInputElement) return;
      if (!canEdit || addDialog || selectedIndex === null) return;
      if (e.key === "Delete") {
        e.preventDefault();
        removeWatchpointAt(selectedIndex);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [canEdit, addDialog, selectedIndex, removeWatchpointAt]);

  /** Validates the input, invokes add_watchpoint, and closes the popover on success. */
  const commitAddWatchpoint = useCallback(async () => {
    if (!addDialog) return;
    const source = addDialog.value.trim();
    if (!source) {
      setAddDialog((d) => d && { ...d, error: "Enter a watchpoint expression" });
      return;
    }
    try {
      const result = await invoke<WatchpointsSnapshot>("add_watchpoint", { source });
      setSnapshot(result);
      setAddDialog(null);
    } catch (e) {
      setAddDialog((d) => d && { ...d, error: String(e) });
    }
  }, [addDialog]);

  /** Dismiss the add popover on Escape while it is open. */
  useEffect(() => {
    if (!addDialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setAddDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [addDialog]);

  return (
    <div className="watchpoint-panel">
      <div className="watchpoint-header">
        <span className="panel-title">Watchpoints</span>
        <button
          className="watchpoint-add-btn"
          onClick={() => setAddDialog({ value: "", error: "" })}
          disabled={!canEdit}
          title={canEdit ? "Add watchpoint" : "Stop the CPU to edit watchpoints"}
        >
          +
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
            const statusClass = !row.enabled ? "wp-disabled" : row.error !== null ? "wp-error" : row.triggered ? "wp-true" : "wp-false";
            return (
              <div
                key={index}
                className={`watchpoint-row${selectedIndex === index ? " selected" : ""}${row.enabled ? "" : " disabled"}`}
              >
                <span
                  className={`indicator ${statusClass}${canEdit ? "" : " readonly"}`}
                  onClick={() => canEdit && toggleWatchpointAt(index)}
                  title={canEdit ? (row.enabled ? "Disable watchpoint" : "Enable watchpoint") : "Stop the CPU to edit watchpoints"}
                >
                  {row.enabled ? "●" : "⊘"}
                </span>
                <span
                  className={`watchpoint-source${expandedIndex === index ? " expanded" : ""}`}
                  onClick={() => handleRowClick(index)}
                  title={row.source}
                >
                  {row.source}
                </span>
                <button
                  className="watchpoint-remove-btn"
                  onClick={() => removeWatchpointAt(index)}
                  disabled={!canEdit}
                  title={canEdit ? "Remove watchpoint" : "Stop the CPU to edit watchpoints"}
                >
                  ×
                </button>
              </div>
            );
          })}
        </div>
      )}

      {addDialog && (
        <div
          className="wp-add-backdrop"
          onClick={() => setAddDialog(null)}
        >
          <div className="wp-add-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="wp-add-title">Add Watchpoint</div>

            <div className="wp-add-field">
              <input
                className={`wp-add-input${addDialog.error ? " invalid" : ""}`}
                autoFocus
                spellCheck={false}
                placeholder="e.g. A == $42"
                value={addDialog.value}
                onChange={(e) =>
                  setAddDialog((d) => d && { ...d, value: e.target.value, error: "" })
                }
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitAddWatchpoint(); }
                  if (e.key === "Escape") { e.preventDefault(); setAddDialog(null); }
                }}
              />
            </div>

            {addDialog.error && (
              <div className="wp-add-error">{addDialog.error}</div>
            )}

            <div className="wp-add-buttons">
              <button
                className="wp-add-btn-action wp-add-btn-cancel"
                onClick={() => setAddDialog(null)}
              >
                Cancel
              </button>
              <button
                className="wp-add-btn-action wp-add-btn-ok"
                onClick={commitAddWatchpoint}
              >
                OK
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
