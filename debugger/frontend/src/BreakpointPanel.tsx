import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useRunControlsContext } from "./RunControlsContext";
import { usePanelHeaderAction } from "./layout/panelHeaderActions";
import "./styles/breakpoints.scss";

interface BreakpointInfo {
  addr: number;
  enabled: boolean;
  label: string | null;
}

/** State for the add-breakpoint popover; null means closed. */
interface AddDialogState {
  /** Controlled value of the address/symbol input. */
  value: string;
  /** Validation error; empty string means no error. */
  error: string;
}

const HEX_DIGITS = /^[0-9a-fA-F]+$/;

/**
 * Parses an address input into a 16-bit address, accepting an optional `$`
 * or `0x` hex prefix (unprefixed text is also parsed as hex, matching how
 * addresses are displayed everywhere in this app). Returns null if the text
 * doesn't parse as a value in 0..0xFFFF.
 */
function parseAddressInput(raw: string): number | null {
  const s = raw.trim();
  const body = s.startsWith("$") ? s.slice(1)
    : /^0x/i.test(s) ? s.slice(2)
    : s;
  if (!HEX_DIGITS.test(body)) return null;
  const n = parseInt(body, 16);
  return n >= 0 && n <= 0xffff ? n : null;
}

function formatAddr(addr: number): string {
  return addr.toString(16).toUpperCase().padStart(4, "0");
}

export default function BreakpointPanel() {
  const { isStopped } = useRunControlsContext();
  const [rows, setRows] = useState<BreakpointInfo[] | null>(null);
  const [addDialog, setAddDialog] = useState<AddDialogState | null>(null);

  const canEdit = isStopped;

  useEffect(() => {
    invoke<BreakpointInfo[]>("get_breakpoints").then(setRows).catch((e) => console.error("get_breakpoints failed:", e));
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<BreakpointInfo[]>("breakpoints-changed", (event) => {
      setRows(event.payload);
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  const toggleBreakpoint = useCallback(async (addr: number, enabled: boolean) => {
    if (!canEdit) return;
    try {
      await invoke<BreakpointInfo[]>(enabled ? "disable_breakpoint" : "enable_breakpoint", { addr });
    } catch (e) {
      console.error("toggle breakpoint failed:", e);
    }
  }, [canEdit]);

  const removeBreakpoint = useCallback(async (addr: number) => {
    if (!canEdit) return;
    try {
      await invoke<BreakpointInfo[]>("remove_breakpoint", { addr });
    } catch (e) {
      console.error("remove_breakpoint failed:", e);
    }
  }, [canEdit]);

  /** Resolves `input` as a symbol first, falling back to a hex address parse. */
  const resolveAddress = useCallback(async (input: string): Promise<number | null> => {
    const trimmed = input.trim();
    try {
      const resolved = await invoke<number | null>("resolve_symbol", { name: trimmed });
      if (resolved !== null) return resolved;
    } catch {
      // symbol resolution unavailable; fall through to hex parse
    }
    return parseAddressInput(trimmed);
  }, []);

  const commitAddBreakpoint = useCallback(async () => {
    if (!addDialog) return;
    const input = addDialog.value.trim();
    if (!input) {
      setAddDialog((d) => d && { ...d, error: "Enter an address or symbol" });
      return;
    }
    const addr = await resolveAddress(input);
    if (addr === null) {
      setAddDialog((d) => d && { ...d, error: "Unrecognized address or symbol" });
      return;
    }
    try {
      await invoke<BreakpointInfo[]>("set_breakpoint", { addr });
      setAddDialog(null);
    } catch (e) {
      setAddDialog((d) => d && { ...d, error: String(e) });
    }
  }, [addDialog, resolveAddress]);

  /** Dismiss the add popover on Escape while it is open. */
  useEffect(() => {
    if (!addDialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setAddDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [addDialog]);

  usePanelHeaderAction("breakpoints", {
    title: "Add breakpoint",
    onClick: () => setAddDialog({ value: "", error: "" }),
    disabled: !canEdit,
    disabledTitle: "Stop the CPU to edit breakpoints",
  });

  return (
    <div className="breakpoint-panel">
      {rows === null ? (
        <span className="breakpoint-empty">Waiting…</span>
      ) : rows.length === 0 ? (
        <span className="breakpoint-empty">No breakpoints</span>
      ) : (
        <div className="breakpoint-body">
          {rows.map((row) => (
            <div key={row.addr} className={`breakpoint-row${row.enabled ? "" : " disabled"}`}>
              <span
                className={`indicator ${row.enabled ? "bp-enabled" : "bp-disabled"}${canEdit ? "" : " readonly"}`}
                onClick={() => toggleBreakpoint(row.addr, row.enabled)}
                title={canEdit ? (row.enabled ? "Disable breakpoint" : "Enable breakpoint") : "Stop the CPU to edit breakpoints"}
              >
                {row.enabled ? "●" : "⊘"}
              </span>
              <span className="breakpoint-addr">{formatAddr(row.addr)}</span>
              {row.label !== null && (
                <span className="breakpoint-label" title={row.label}>{row.label}</span>
              )}
              <button
                className="breakpoint-remove-btn"
                onClick={() => removeBreakpoint(row.addr)}
                disabled={!canEdit}
                title={canEdit ? "Remove breakpoint" : "Stop the CPU to edit breakpoints"}
              >
                ×
              </button>
            </div>
          ))}
        </div>
      )}

      {addDialog && (
        <div className="bp-add-backdrop" onClick={() => setAddDialog(null)}>
          <div className="bp-add-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="bp-add-title">Add Breakpoint</div>

            <div className="bp-add-field">
              <input
                className={`bp-add-input${addDialog.error ? " invalid" : ""}`}
                autoFocus
                spellCheck={false}
                placeholder="e.g. 55AA or a symbol"
                value={addDialog.value}
                onChange={(e) => setAddDialog((d) => d && { ...d, value: e.target.value, error: "" })}
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitAddBreakpoint(); }
                  if (e.key === "Escape") { e.preventDefault(); setAddDialog(null); }
                }}
              />
            </div>

            {addDialog.error && <div className="bp-add-error">{addDialog.error}</div>}

            <div className="bp-add-buttons">
              <button className="bp-add-btn-action bp-add-btn-cancel" onClick={() => setAddDialog(null)}>
                Cancel
              </button>
              <button className="bp-add-btn-action bp-add-btn-ok" onClick={commitAddBreakpoint}>
                OK
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
