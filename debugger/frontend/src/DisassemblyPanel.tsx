import {useCallback, useEffect, useRef, useState} from "react";
import {listen} from "@tauri-apps/api/event";
import {invoke} from "@tauri-apps/api/core";
import {useExecutionContext} from "./ExecutionContext";
import {useRunControlsContext} from "./RunControlsContext";
import "./styles/disassembly.scss";

interface DisassembledRow {
  addr: number;
  bytes: string[];
  labels: string[];
  mnemonic: string;
  operand: string;
  comment: string;
  is_valid: boolean;
}

interface BreakpointInfo {
  addr: number;
  enabled: boolean;
}

/** Position and target address of an open row context menu. */
interface ContextMenuState {
  addr: number;
  x: number;
  y: number;
}

const HEX_DIGITS = /^[0-9a-fA-F]+$/;

/**
 * Parses a "Set breakpoint at address…" input into a 16-bit address.
 *
 * Accepts an optional `$` or `0x` hex prefix; unprefixed text is also parsed as hex,
 * matching how addresses are always displayed in this panel (see `formatAddr`).
 * Returns null if the text doesn't parse as a value in 0..0xFFFF.
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

interface RegisterSnapshot {
  a: number; x: number; y: number; s: number;
  pc: number; p: number; changed_flags: number;
  cpu_stopped: boolean;
  cpu_waiting: boolean;
  breakpoint_hit: boolean;
}

/** Rows to pre-fetch beyond the visible window so scrolling has buffer. */
const FETCH_ROWS = 48;
/** When PC index reaches within this many rows of the bottom, extend the list. */
const SCROLL_EDGE = 6;

export default function DisassemblyPanel() {
  const { onStep } = useExecutionContext();
  const { isStopped } = useRunControlsContext();
  const [rows, setRows] = useState<DisassembledRow[]>([]);
  const [currentPc, setCurrentPc] = useState<number | null>(null);
  const [addrInputValue, setAddrInputValue] = useState<string>("");
  const [breakpoints, setBreakpoints] = useState<Map<number, boolean>>(new Map());
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);
  const [addressInputOpen, setAddressInputOpen] = useState(false);
  const [addressInputValue, setAddressInputValue] = useState("");
  const [addressInputInvalid, setAddressInputInvalid] = useState(false);
  const contextMenuRef = useRef<HTMLDivElement | null>(null);

  // Keep refs so callbacks see the latest values without being re-created.
  const rowsRef = useRef<DisassembledRow[]>([]);
  const pcRef = useRef<number | null>(null);

  rowsRef.current = rows;
  pcRef.current = currentPc;

  /** Converts a sorted breakpoint list from the backend into the addr -> enabled map used for display. */
  const applyBreakpointList = useCallback((list: BreakpointInfo[]) => {
    setBreakpoints(new Map(list.map((b) => [b.addr, b.enabled])));
  }, []);

  const handleToggleBreakpoint = useCallback(async (addr: number) => {
    if (!isStopped) return;
    try {
      const updated = await invoke<BreakpointInfo[]>("toggle_breakpoint", { addr });
      applyBreakpointList(updated);
    } catch (e) {
      console.error("toggle_breakpoint failed:", e);
    }
  }, [applyBreakpointList, isStopped]);

  /** Runs a breakpoint command (set/remove/disable/enable) and applies the returned list. */
  const runBreakpointCommand = useCallback(async (command: string, addr: number) => {
    if (!isStopped) return;
    try {
      const updated = await invoke<BreakpointInfo[]>(command, { addr });
      applyBreakpointList(updated);
    } catch (e) {
      console.error(`${command} failed:`, e);
    }
  }, [applyBreakpointList, isStopped]);

  const closeContextMenu = useCallback(() => {
    setContextMenu(null);
    setAddressInputOpen(false);
    setAddressInputInvalid(false);
  }, []);

  const handleRowContextMenu = useCallback((e: React.MouseEvent, addr: number) => {
    e.preventDefault();
    setAddressInputOpen(false);
    setAddressInputInvalid(false);
    setAddressInputValue("");
    setContextMenu({ addr, x: e.clientX, y: e.clientY });
  }, []);

  const openAddressInput = useCallback(() => {
    if (!isStopped) return;
    setAddressInputValue("");
    setAddressInputInvalid(false);
    setAddressInputOpen(true);
  }, [isStopped]);

  const commitAddressInput = useCallback(async () => {
    if (!isStopped) return;
    const addr = parseAddressInput(addressInputValue);
    if (addr === null) {
      setAddressInputInvalid(true);
      return;
    }
    await runBreakpointCommand("set_breakpoint", addr);
    closeContextMenu();
  }, [addressInputValue, runBreakpointCommand, closeContextMenu, isStopped]);

  // Close the context menu on outside click or Escape.
  useEffect(() => {
    if (contextMenu === null) return;
    const handlePointerDown = (e: MouseEvent) => {
      if (contextMenuRef.current && !contextMenuRef.current.contains(e.target as Node)) {
        closeContextMenu();
      }
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeContextMenu();
    };
    document.addEventListener("mousedown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [contextMenu, closeContextMenu]);

  /** Fetch `FETCH_ROWS` instructions starting at `addr` and replace the row list. */
  const fetchFrom = useCallback(async (addr: number) => {
    try {
      const result = await invoke<DisassembledRow[]>("get_disassembly", {
        addr,
        count: FETCH_ROWS,
      });
      setRows(result);
      setAddrInputValue(formatAddr(addr));
    } catch (e) {
      console.error("get_disassembly failed:", e);
    }
  }, []);

  /** Append enough new rows so at least `FETCH_ROWS` exist past the current PC index. */
  const extendFrom = useCallback(async (fromAddr: number) => {
    try {
      const extra = await invoke<DisassembledRow[]>("get_disassembly", {
        addr: fromAddr,
        count: FETCH_ROWS,
      });
      setRows((prev) => {
        // Deduplicate by addr: keep all prev rows, then append rows whose addr isn't already present.
        const seen = new Set(prev.map((r) => r.addr));
        const newRows = extra.filter((r) => !seen.has(r.addr));
        return [...prev, ...newRows];
      });
    } catch (e) {
      console.error("get_disassembly (extend) failed:", e);
    }
  }, []);

  const handleHalted = useCallback(async (newPc: number) => {
    const current = rowsRef.current;

    // Find where the new PC sits in the current row list.
    const pcIndex = current.findIndex((r) => r.addr === newPc);

    if (pcIndex === -1) {
      // PC is not in the current row list (jump to a new location) — fetch fresh.
      await fetchFrom(newPc);
      setCurrentPc(newPc);
      return;
    }

    // PC is in the list. If it's approaching the bottom edge, extend the list.
    if (pcIndex >= current.length - SCROLL_EDGE) {
      const lastRow = current[current.length - 1];
      // Start fetching from just after the last known row.
      const nextAddr = lastRow.addr + lastRow.bytes.length;
      extendFrom(nextAddr);
    }

    setCurrentPc(newPc);
  }, [fetchFrom, extendFrom]);

  useEffect(() => {
    const unlistenHaltedPromise = listen<number>("debugger-halted", (event) => {
      handleHalted(event.payload);
    });

    // RunControlsContext also listens for this event (to reset its own
    // free-run/stepping state) — deliberate dual subscription, not
    // duplication to clean up later.
    const unlistenRunStoppedPromise = listen<RegisterSnapshot>("debugger-run-stopped", (event) => {
      onStep(event.payload);
    });

    // Re-fetch disassembly from the current PC when memory contents change (e.g. after a load).
    const unlistenMemoryModifiedPromise = listen("memory-modified", () => {
      const pc = pcRef.current;
      if (pc !== null) fetchFrom(pc);
    });

    // Keep the gutter in sync with edits made from the standalone Breakpoints panel.
    const unlistenBreakpointsChangedPromise = listen<BreakpointInfo[]>("breakpoints-changed", (event) => {
      applyBreakpointList(event.payload);
    });

    // Proactively fetch on mount: the initial `debugger-halted` event can fire
    // before our listener is registered (listen() is async), leaving rows empty.
    invoke<RegisterSnapshot>("get_registers")
      .then((snap) => { if (rowsRef.current.length === 0) handleHalted(snap.pc); })
      .catch(() => {});

    invoke<BreakpointInfo[]>("get_breakpoints")
      .then(applyBreakpointList)
      .catch(() => {});

    return () => {
      unlistenHaltedPromise.then((f) => f());
      unlistenRunStoppedPromise.then((f) => f());
      unlistenMemoryModifiedPromise.then((f) => f());
      unlistenBreakpointsChangedPromise.then((f) => f());
    };
  }, [handleHalted, onStep, applyBreakpointList, fetchFrom]);

  // Scroll the current-PC row into view whenever it changes.
  const pcRowRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    pcRowRef.current?.scrollIntoView({ block: "nearest" });
  }, [currentPc]);

  const handleAddrInputKeyDown = useCallback(async (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      let addr: number | null = null;
      try {
        addr = await invoke<number | null>("resolve_symbol", { name: addrInputValue.trim() });
      } catch {
        // symbol resolution unavailable; fall through to hex parse
      }
      if (addr === null) addr = parseAddressInput(addrInputValue);
      if (addr !== null) fetchFrom(addr);
    }
  }, [addrInputValue, fetchFrom]);

  const formatAddr = (addr: number) =>
    addr.toString(16).toUpperCase().padStart(4, "0");

  const formatBytes = (bytes: string[]) =>
    bytes.map((b) => b.padStart(2, "0")).join(" ").padEnd(8, " ");

  return (
    <div className="disassembly-panel">
      <div className="disassembly-header">
        <div className="disassembly-toolbar">
          <input
            className="disasm-addr-input"
            value={addrInputValue}
            onChange={(e) => setAddrInputValue(e.target.value)}
            onKeyDown={handleAddrInputKeyDown}
            spellCheck={false}
            placeholder="0000"
            title="Enter hex address and press Enter"
          />
        </div>
      </div>
      <div className="disassembly-body">
        {rows.length === 0 ? (
          <span className="disassembly-empty">Waiting for session…</span>
        ) : (
          rows.flatMap((row) => {
            const isCurrent = row.addr === currentPc;
            const bpEnabled = breakpoints.get(row.addr);
            const gutterClasses = [
              "disasm-gutter",
              bpEnabled === true ? "breakpoint" : bpEnabled === false ? "breakpoint-disabled" : "",
              isStopped ? "" : "locked",
            ]
              .filter(Boolean)
              .join(" ");
            const gutterTitle = isStopped
              ? (bpEnabled !== undefined ? "Remove breakpoint" : "Set breakpoint")
              : "Stop the CPU to edit breakpoints";
            const gutterDot = bpEnabled === false ? "○" : "●";

            const labelGutterClasses = [
              "disasm-gutter",
              isStopped ? "" : "locked",
            ]
              .filter(Boolean)
              .join(" ");

            const labelRows = row.labels.map((label, index) => (
              <div
                key={`${row.addr}-label-${label}`}
                ref={isCurrent && index === 0 ? pcRowRef : null}
                className="disasm-row disasm-label-row"
                onContextMenu={(e) => handleRowContextMenu(e, row.addr)}
              >
                <span
                  className={labelGutterClasses}
                  onClick={() => handleToggleBreakpoint(row.addr)}
                  title={gutterTitle}
                >
                  ●
                </span>
                <span className="disasm-label">{label}:</span>
              </div>
            ));

            const instrRow = (
              <div
                key={row.addr}
                ref={isCurrent && row.labels.length === 0 ? pcRowRef : null}
                className={[
                  "disasm-row",
                  isCurrent ? "current-pc" : "",
                  row.is_valid ? "" : "invalid-op",
                ]
                  .filter(Boolean)
                  .join(" ")}
                onContextMenu={(e) => handleRowContextMenu(e, row.addr)}
              >
                <span
                  className={gutterClasses}
                  onClick={() => handleToggleBreakpoint(row.addr)}
                  title={gutterTitle}
                >
                  {gutterDot}
                </span>
                <span className="disasm-addr">{formatAddr(row.addr)}</span>
                <span className="disasm-bytes">{formatBytes(row.bytes)}</span>
                <span className="disasm-mnemonic">{row.mnemonic}</span>
                {row.operand && (
                  <span className="disasm-operand">{row.operand}</span>
                )}
                {row.comment && (
                  <span className="disasm-comment">{row.comment}</span>
                )}
              </div>
            );

            return [...labelRows, instrRow];
          })
        )}
      </div>
      {contextMenu && (
        <div
          ref={contextMenuRef}
          className="disasm-context-menu"
          style={{ top: contextMenu.y, left: contextMenu.x }}
        >
          {addressInputOpen ? (
            <input
              className={`disasm-context-menu-input${addressInputInvalid ? " invalid" : ""}`}
              type="text"
              autoFocus
              placeholder="$XXXX"
              value={addressInputValue}
              onChange={(e) => { setAddressInputValue(e.target.value); setAddressInputInvalid(false); }}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitAddressInput();
                if (e.key === "Escape") closeContextMenu();
              }}
              onBlur={closeContextMenu}
            />
          ) : (
            <>
              {breakpoints.get(contextMenu.addr) === undefined && (
                <div
                  className={`disasm-context-menu-item${isStopped ? "" : " disabled"}`}
                  title={isStopped ? undefined : "Stop the CPU to edit breakpoints"}
                  onClick={isStopped ? () => { runBreakpointCommand("set_breakpoint", contextMenu.addr); closeContextMenu(); } : undefined}
                >
                  Set Breakpoint
                </div>
              )}
              {breakpoints.get(contextMenu.addr) === true && (
                <div
                  className={`disasm-context-menu-item${isStopped ? "" : " disabled"}`}
                  title={isStopped ? undefined : "Stop the CPU to edit breakpoints"}
                  onClick={isStopped ? () => { runBreakpointCommand("disable_breakpoint", contextMenu.addr); closeContextMenu(); } : undefined}
                >
                  Disable Breakpoint
                </div>
              )}
              {breakpoints.get(contextMenu.addr) === false && (
                <div
                  className={`disasm-context-menu-item${isStopped ? "" : " disabled"}`}
                  title={isStopped ? undefined : "Stop the CPU to edit breakpoints"}
                  onClick={isStopped ? () => { runBreakpointCommand("enable_breakpoint", contextMenu.addr); closeContextMenu(); } : undefined}
                >
                  Enable Breakpoint
                </div>
              )}
              {breakpoints.get(contextMenu.addr) !== undefined && (
                <div
                  className={`disasm-context-menu-item${isStopped ? "" : " disabled"}`}
                  title={isStopped ? undefined : "Stop the CPU to edit breakpoints"}
                  onClick={isStopped ? () => { runBreakpointCommand("remove_breakpoint", contextMenu.addr); closeContextMenu(); } : undefined}
                >
                  Remove Breakpoint
                </div>
              )}
              <div
                className={`disasm-context-menu-item${isStopped ? "" : " disabled"}`}
                title={isStopped ? undefined : "Stop the CPU to edit breakpoints"}
                onClick={isStopped ? openAddressInput : undefined}
              >
                Set Breakpoint at Address…
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}
