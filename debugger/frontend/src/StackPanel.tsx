import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/stack.scss";

interface StackSnapshot {
  s: number;
  page: number[];
}

/** Number of word-pair rows visible in the panel. */
const VISIBLE_PAIRS = 8;

/** Returns a two-digit uppercase hex string or "--" for the placeholder. */
function fmtByte(value: number | null): string {
  return value === null ? "--" : value.toString(16).toUpperCase().padStart(2, "0");
}

/** Returns a 4-digit uppercase hex address string (page-1 address). */
function fmtAddr(pageOffset: number): string {
  return (0x0100 + pageOffset).toString(16).toUpperCase().padStart(4, "0");
}

interface StackRow {
  /** Page offset of the lo byte of this pair. */
  offset: number;
  /** Lo byte value, or null when S points to this slot (next write position). */
  lo: number | null;
  /** Hi byte value, or null when S points to this slot (next write position). */
  hi: number | null;
  /** True for the first row (the pair containing the most recently pushed byte). */
  isActive: boolean;
}

/**
 * Builds exactly VISIBLE_PAIRS word-pair rows, wrapping at the page boundary.
 *
 * The 65C02 stack pointer S points to the next free slot (pre-decrement push),
 * so the most recently pushed byte is at (S + 1) mod 256.  The top row always
 * contains that byte; subsequent rows proceed toward higher addresses with
 * wraparound.  The placeholder `--` marks whichever slot S occupies.
 *
 * `alignOdd` shifts pair boundaries by one byte:
 * Even pairs: (0x00,0x01), (0x02,0x03), ..., (0xFE,0xFF) — lo byte is even
 * Odd pairs:  (0x01,0x02), (0x03,0x04), ..., (0xFF,0x00) — lo byte is odd, wraps
 *
 * Both alignments use the same formula: (activePairOffset + i*2) % 256.
 */
function buildRows(s: number, page: number[], alignOdd: boolean): StackRow[] {
  const lastPushed = (s + 1) & 0xff;

  // Find the pair that contains lastPushed.
  // Even: lo bytes are even — round lastPushed down to even.
  // Odd:  lo bytes are odd  — if lastPushed is odd it is the lo byte;
  //       if even it is the hi byte and the lo byte is one below (wrapping).
  let activePairOffset: number;
  if (!alignOdd) {
    activePairOffset = lastPushed & 0xfe;
  } else {
    activePairOffset = lastPushed % 2 === 1
      ? lastPushed
      : (lastPushed - 1 + 256) % 256;
  }

  const rows: StackRow[] = [];
  for (let i = 0; i < VISIBLE_PAIRS; i++) {
    const offset = (activePairOffset + i * 2) % 256;
    const hiOffset = (offset + 1) % 256;
    rows.push({
      offset,
      lo: offset === s ? null : page[offset],
      hi: hiOffset === s ? null : page[hiOffset],
      isActive: i === 0,
    });
  }
  return rows;
}

export default function StackPanel() {
  const [snap, setSnap] = useState<StackSnapshot | null>(null);
  const [alignOdd, setAlignOdd] = useState(false);

  const fetchStack = useCallback(async () => {
    try {
      const result = await invoke<StackSnapshot>("get_stack");
      setSnap(result);
    } catch (e) {
      console.error("get_stack failed:", e);
    }
  }, []);

  useEffect(() => {
    fetchStack();
  }, [fetchStack]);

  useEffect(() => {
    const unlistenPromise = listen<number>("debugger-halted", () => {
      fetchStack();
    });
    return () => { unlistenPromise.then((f) => f()); };
  }, [fetchStack]);

  const toggleAlign = useCallback(() => {
    setAlignOdd((v) => !v);
  }, []);

  return (
    <div className="stack-panel">
      <div className="stack-header">
        <span className="panel-title">Stack</span>
        <button
          className="align-btn"
          onClick={toggleAlign}
          title="Toggle even/odd word alignment"
        >
          {alignOdd ? "ODD" : "EVEN"}
        </button>
      </div>
      {snap === null ? (
        <span className="stack-empty">Waiting…</span>
      ) : (
        <div className="stack-body">
          {buildRows(snap.s, snap.page, alignOdd).map((row) => (
            <div key={row.offset} className={`stack-row${row.isActive ? " stack-row-active" : ""}`}>
              <span className="stack-chevron">{row.isActive ? ">" : " "}</span>
              <span className="stack-addr">{fmtAddr(row.offset)}</span>
              <span className={`stack-lo${row.lo === null ? " stack-placeholder" : ""}`}>{fmtByte(row.lo)}</span>
              <span className={`stack-hi${row.hi === null ? " stack-placeholder" : ""}`}>{fmtByte(row.hi)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
