import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ExecState } from "./DisassemblyPanel";
import { DataRadix, formatDataRadix, parseIntegerInput, RadixButton, toUnsignedInRange, useDataRadix } from "./RadixControl";
import "./styles/watchpoints.scss";

interface WatchpointRow {
  source: string;
  triggered: boolean;
  error: string | null;
  enabled: boolean;
}

/** One watch variable's name and current value, as last assigned by `:=`. */
interface VariableRow {
  name: string;
  value: number;
}

interface WatchpointsSnapshot {
  compile_error: string | null;
  rows: WatchpointRow[];
  variables: VariableRow[];
}

/** State for the add-watchpoint popover; null means closed. */
interface AddDialogState {
  /** Controlled value of the expression input. */
  value: string;
  /** Compilation or validation error; empty string means no error. */
  error: string;
}

/** State for the edit-watchpoint popover; null means closed. */
interface EditDialogState {
  /** Index of the watchpoint being edited. */
  index: number;
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
  const panelRef = useRef<HTMLDivElement>(null);
  const [snapshot, setSnapshot] = useState<WatchpointsSnapshot | null>(null);
  const [expandedIndex, setExpandedIndex] = useState<number | null>(null);
  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);
  const [addDialog, setAddDialog] = useState<AddDialogState | null>(null);
  const [editDialog, setEditDialog] = useState<EditDialogState | null>(null);
  const [variablesExpanded, setVariablesExpanded] = useState(true);
  const variablesExpandedInitialized = useRef(false);
  const [varRadix, cycleVarRadix] = useDataRadix("hex");
  const [editingVarName, setEditingVarName] = useState<string | null>(null);
  const [varEditValue, setVarEditValue] = useState("");
  const [varEditInvalid, setVarEditInvalid] = useState(false);

  const canEdit = execState === "stopped" && snapshot?.compile_error == null;

  const fetchWatchpoints = useCallback(async () => {
    try {
      const result = await invoke<WatchpointsSnapshot>("get_watchpoints");
      setSnapshot(result);
      // Collapse the variables section by default only if it starts out
      // empty; once initialized, later fetches never override the user's
      // manual expand/collapse choice.
      if (!variablesExpandedInitialized.current) {
        variablesExpandedInitialized.current = true;
        setVariablesExpanded(result.variables.length > 0);
      }
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

  /** Clears the row-selection highlight once the user's focus moves outside the panel. */
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) {
        setSelectedIndex(null);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
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
      if (!canEdit || addDialog || editDialog || selectedIndex === null) return;
      if (e.key === "Delete") {
        e.preventDefault();
        removeWatchpointAt(selectedIndex);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [canEdit, addDialog, editDialog, selectedIndex, removeWatchpointAt]);

  /** Double-click on a row's source text opens the edit popover, seeded with its current expression. */
  const openEditDialog = useCallback(
    (index: number, source: string) => {
      if (!canEdit) return;
      setEditDialog({ index, value: source, error: "" });
    },
    [canEdit],
  );

  /** Validates the input, invokes edit_watchpoint, and closes the popover on success. */
  const commitEditWatchpoint = useCallback(async () => {
    if (!editDialog) return;
    const source = editDialog.value.trim();
    if (!source) {
      setEditDialog((d) => d && { ...d, error: "Enter a watchpoint expression" });
      return;
    }
    try {
      const result = await invoke<WatchpointsSnapshot>("edit_watchpoint", { index: editDialog.index, source });
      setSnapshot(result);
      setEditDialog(null);
    } catch (e) {
      setEditDialog((d) => d && { ...d, error: String(e) });
    }
  }, [editDialog]);

  /** Dismiss the edit popover on Escape while it is open. */
  useEffect(() => {
    if (!editDialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setEditDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [editDialog]);

  /** Double-click on a variable's value opens an inline edit input, seeded with its current display text. */
  const beginVarEdit = useCallback(
    (name: string, currentText: string) => {
      if (!canEdit) return;
      setEditingVarName(name);
      setVarEditValue(currentText);
      setVarEditInvalid(false);
    },
    [canEdit],
  );

  const cancelVarEdit = useCallback(() => {
    setEditingVarName(null);
    setVarEditInvalid(false);
  }, []);

  /** Parses the input using the same radix/prefix rules as register editing, invokes set_watch_variable, and closes the input on success. */
  const commitVarEdit = useCallback(async (name: string, radix: DataRadix) => {
    const parsed = parseIntegerInput(varEditValue, radix);
    const value = parsed === null ? null : toUnsignedInRange(parsed, 32, true);
    if (value === null) {
      setVarEditInvalid(true);
      return;
    }
    try {
      const result = await invoke<WatchpointsSnapshot>("set_watch_variable", { name, value });
      setSnapshot(result);
      setEditingVarName(null);
      setVarEditInvalid(false);
    } catch (e) {
      console.error("set_watch_variable failed:", e);
      setVarEditInvalid(true);
    }
  }, [varEditValue]);

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
    <div className="watchpoint-panel" ref={panelRef}>
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
                  onDoubleClick={() => openEditDialog(index, row.source)}
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

      {snapshot !== null && snapshot.compile_error === null && (
        <div className="wp-vars-section">
          <div
            className="wp-vars-header"
            onClick={() => setVariablesExpanded((e) => !e)}
          >
            <i className={`codicon codicon-chevron-${variablesExpanded ? "down" : "right"}`} />
            <span className="wp-vars-title">Variables</span>
            <RadixButton radix={varRadix} onCycle={cycleVarRadix} stopPropagation />
          </div>
          {variablesExpanded && (
            snapshot.variables.length === 0 ? (
              <span className="wp-vars-empty">No variables</span>
            ) : (
              <div className="wp-vars-body">
                {snapshot.variables.map((v) => (
                  <div key={v.name} className="wp-vars-row">
                    <span className="wp-vars-name">{v.name}</span>
                    {editingVarName === v.name ? (
                      <input
                        className={`wp-vars-edit-input${varEditInvalid ? " invalid" : ""}`}
                        autoFocus
                        value={varEditValue}
                        onChange={(e) => { setVarEditValue(e.target.value); setVarEditInvalid(false); }}
                        onKeyDown={(e) => {
                          e.stopPropagation();
                          if (e.key === "Enter") { e.preventDefault(); commitVarEdit(v.name, varRadix); }
                          else if (e.key === "Escape") { e.preventDefault(); cancelVarEdit(); }
                        }}
                        onBlur={cancelVarEdit}
                      />
                    ) : (
                      <span
                        className={`wp-vars-value${canEdit ? " wp-vars-value-editable" : ""}`}
                        onDoubleClick={() => beginVarEdit(v.name, formatDataRadix(v.value, varRadix))}
                        title={canEdit ? "Double-click to edit" : undefined}
                      >
                        {formatDataRadix(v.value, varRadix)}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            )
          )}
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

      {editDialog && (
        <div
          className="wp-add-backdrop"
          onClick={() => setEditDialog(null)}
        >
          <div className="wp-add-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="wp-add-title">Edit Watchpoint</div>

            <div className="wp-add-field">
              <input
                className={`wp-add-input${editDialog.error ? " invalid" : ""}`}
                autoFocus
                spellCheck={false}
                value={editDialog.value}
                onChange={(e) =>
                  setEditDialog((d) => d && { ...d, value: e.target.value, error: "" })
                }
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitEditWatchpoint(); }
                  if (e.key === "Escape") { e.preventDefault(); setEditDialog(null); }
                }}
              />
            </div>

            {editDialog.error && (
              <div className="wp-add-error">{editDialog.error}</div>
            )}

            <div className="wp-add-buttons">
              <button
                className="wp-add-btn-action wp-add-btn-cancel"
                onClick={() => setEditDialog(null)}
              >
                Cancel
              </button>
              <button
                className="wp-add-btn-action wp-add-btn-ok"
                onClick={commitEditWatchpoint}
                disabled={!!editDialog.error}
                title={editDialog.error ? "Fix the error before saving" : "Save changes"}
              >
                Save
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
