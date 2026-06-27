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

// --- radix cycling ---

type DataRadix = "hex" | "udec" | "sdec" | "oct";

const DATA_RADIX_CYCLE: DataRadix[] = ["hex", "udec", "sdec", "oct"];

const DATA_RADIX_LABEL: Record<DataRadix, string> = {
  hex:  "HEX",
  udec: "DEC",
  sdec: "±DEC",
  oct:  "OCT",
};


function formatData(value: number | null, radix: DataRadix): string {
  if (value !== null) {
    switch (radix) {
      case "hex":  return value.toString(16).toUpperCase().padStart(2, "0");
      case "udec": return value.toString(10);
      case "sdec": return ((value << 24) >> 24).toString(10);
      case "oct":  return value.toString(8).padStart(3, "0");
    }
  }
  else {
    switch (radix) {
      case "hex": return "--";
      case "udec": return "---";
      case "sdec": return "----";
      case "oct": return "---";
    }
  }
}

function radixDataWidth(radix: DataRadix): number {
  switch (radix) {
    case "hex": return 50;
    case "udec": return 60;
    case "sdec": return 65;
    case "oct": return 60;
  }
}

/** Returns a 4-digit uppercase hex address string (page-1 address). */
function formatAddr(pageOffset: number): string {
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
  const [dataRadix, setDataRadix] = useState<DataRadix>("hex");
  const [alignOdd, setAlignOdd] = useState(false);
  const [dataWidth, setDataWidth] = useState(50);

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

  const cycleDataRadix = useCallback(() => {
    setDataRadix((prevRadix) => {
      const i = DATA_RADIX_CYCLE.indexOf(prevRadix);
      const nextRadix = DATA_RADIX_CYCLE[(i + 1) % DATA_RADIX_CYCLE.length];
      setDataWidth(radixDataWidth(nextRadix));
      return nextRadix;
    });
  }, []);

  const toggleAlign = useCallback(() => {
    setAlignOdd((v) => !v);
  }, []);

  return (
    <div className="stack-panel">
      <div className="stack-header">
        <span className="panel-title">Stack</span>
        <button className="radix-btn" onClick={cycleDataRadix} title="Cycle radix">
          {DATA_RADIX_LABEL[dataRadix]}
        </button>
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
        <div className="stack-body" style={{width: `${dataWidth}%`}}>
          {buildRows(snap.s, snap.page, alignOdd).map((row) => (
            <div key={row.offset} className={`stack-row${row.isActive ? " stack-row-active" : ""}`}>
              <span className="stack-chevron">{row.isActive ? ">" : " "}</span>
              <span className="stack-addr">{formatAddr(row.offset)}</span>
              <span className={`stack-lo${row.lo === null ? " stack-placeholder" : ""}`}>{formatData(row.lo, dataRadix)}</span>
              <span className={`stack-hi${row.hi === null ? " stack-placeholder" : ""}`}>{formatData(row.hi, dataRadix)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
