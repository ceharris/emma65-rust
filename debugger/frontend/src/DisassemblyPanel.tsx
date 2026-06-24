import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/disassembly.scss";

interface DisassembledRow {
  addr: number;
  bytes: string[];
  mnemonic: string;
  operand: string;
  is_valid: boolean;
}

interface RegisterSnapshot {
  a: number; x: number; y: number; s: number;
  pc: number; p: number; changed_flags: number;
}

interface Props {
  /** Called with the post-step register snapshot so RegisterPanel can update immediately. */
  onStep: (snap: RegisterSnapshot) => void;
}

/** Rows to pre-fetch beyond the visible window so scrolling has buffer. */
const FETCH_ROWS = 48;
/** When PC index reaches within this many rows of the bottom, extend the list. */
const SCROLL_EDGE = 6;

export default function DisassemblyPanel({ onStep }: Props) {
  const [rows, setRows] = useState<DisassembledRow[]>([]);
  const [currentPc, setCurrentPc] = useState<number | null>(null);
  const [stepping, setStepping] = useState(false);
  // Keep a ref so callbacks see the latest rows without needing to be re-created.
  const rowsRef = useRef<DisassembledRow[]>([]);
  const pcRef = useRef<number | null>(null);

  rowsRef.current = rows;
  pcRef.current = currentPc;

  /** Fetch `FETCH_ROWS` instructions starting at `addr` and replace the row list. */
  const fetchFrom = useCallback(async (addr: number) => {
    try {
      const result = await invoke<DisassembledRow[]>("get_disassembly", {
        addr,
        count: FETCH_ROWS,
      });
      setRows(result);
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
    const unlistenPromise = listen<number>("debugger-halted", (event) => {
      handleHalted(event.payload);
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, [handleHalted]);

  // Scroll the current-PC row into view whenever it changes.
  const pcRowRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    pcRowRef.current?.scrollIntoView({ block: "nearest" });
  }, [currentPc]);

  const stepInto = useCallback(async () => {
    if (stepping) return;
    setStepping(true);
    try {
      const snap = await invoke<RegisterSnapshot>("step_into");
      onStep(snap);
    } catch (e) {
      console.error("step_into failed:", e);
    } finally {
      setStepping(false);
    }
  }, [stepping, onStep]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "F11" && !e.shiftKey) {
        e.preventDefault();
        stepInto();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [stepInto]);

  const formatAddr = (addr: number) =>
    addr.toString(16).toUpperCase().padStart(4, "0");

  const formatBytes = (bytes: string[]) =>
    bytes.map((b) => b.padStart(2, "0")).join(" ").padEnd(8, " ");

  return (
    <div className="disassembly-panel">
      <div className="disassembly-header">
        <span className="panel-title">Disassembly</span>
        <div className="exec-controls">
          <button
            className="exec-btn step-into-btn"
            onClick={stepInto}
            disabled={stepping}
            title="Step Into (F11)"
          >
            Step Into
          </button>
        </div>
      </div>
      <div className="disassembly-body">
        {rows.length === 0 ? (
          <span className="disassembly-empty">Waiting for session…</span>
        ) : (
          rows.map((row) => {
            const isCurrent = row.addr === currentPc;
            return (
              <div
                key={row.addr}
                ref={isCurrent ? pcRowRef : null}
                className={[
                  "disasm-row",
                  isCurrent ? "current-pc" : "",
                  row.is_valid ? "" : "invalid-op",
                ]
                  .filter(Boolean)
                  .join(" ")}
              >
                <span className="disasm-gutter" />
                <span className="disasm-addr">{formatAddr(row.addr)}</span>
                <span className="disasm-bytes">{formatBytes(row.bytes)}</span>
                <span className="disasm-mnemonic">{row.mnemonic}</span>
                {row.operand && (
                  <span className="disasm-operand">{row.operand}</span>
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
