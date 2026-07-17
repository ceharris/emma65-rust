import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/memory.scss";

/** Number of bytes per display row. */
const BYTES_PER_ROW = 16;

/** Number of rows in a page (256 bytes). */
const ROWS_PER_PAGE = 16;

/** Address mask for paragraph alignment */
const PARAGRAPH_MASK = 0xfff0;

/** Total size of the memory address space */
const MEMORY_SIZE = 0x10000;

/** Parse a hex (0x/$ prefix) or decimal address string. Returns NaN on failure. */
function parseAddress(input: string): number {
  const trimmed = input.trim();
  if (/^\$[0-9a-fA-F]+$/.test(trimmed)) {
    return parseInt(trimmed.slice(1), 16);
  }
  if (/^0x[0-9a-fA-F]+$/i.test(trimmed)) {
    return parseInt(trimmed, 16);
  }
  if (/^\d+$/.test(trimmed)) {
    return parseInt(trimmed, 10);
  }
  return NaN;
}

/** Returns the 4-digit uppercase hex string for an address. */
function fmtAddr(addr: number): string {
  return addr.toString(16).toUpperCase().padStart(4, "0");
}

/** Renders a single printable ASCII character or `.` for non-printable bytes. */
function toAsciiChar(byte: number): string {
  return byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : ".";
}

export default function MemoryPanel() {
  /** Paragraph-aligned start address of the currently displayed 256-byte page. */
  const [pageAddr, setPageAddr] = useState<number>(0x0000);
  /** Ref mirrors pageAddr so event listeners always see the current value. */
  const pageAddrRef = useRef<number>(0x0000);
  /** 256-byte buffer for the current page. */
  const [bytes, setBytes] = useState<Uint8Array>(new Uint8Array(256));
  /** Controlled value of the address input field. */
  const [inputValue, setInputValue] = useState<string>("$0000");
  const [ready, setReady] = useState(false);

  /** Fetch the 256-byte page starting at `addr` (must be paragraph-aligned). */
  const fetchPage = useCallback(async (addr: number) => {
    try {
      const result = await invoke<number[]>("get_memory", { addr });
      setBytes(new Uint8Array(result));
      pageAddrRef.current = addr;
      setPageAddr(addr);
      setInputValue(`$${fmtAddr(addr)}`);
    } catch (e) {
      console.error("get_memory failed:", e);
    }
  }, []);

  /** Navigate to the page containing `rawAddr`, keeping paragraph alignment. */
  const navigateTo = useCallback(
    (rawAddr: number) => {
      const aligned = (rawAddr & PARAGRAPH_MASK) >>> 0;
      fetchPage(aligned);
    },
    [fetchPage],
  );

  // Initial load and refresh on each halt or running tick.
  useEffect(() => {
    fetchPage(0x0000).then(() => setReady(true));

    const unlistenHalted = listen("debugger-halted", () => {
      fetchPage(pageAddrRef.current);
    });
    const unlistenTick = listen("debugger-running-tick", () => {
      fetchPage(pageAddrRef.current);
    });

    return () => {
      unlistenHalted.then((f) => f());
      unlistenTick.then((f) => f());
    };
  }, [fetchPage]);

  /** Navigate on Enter in the address input. */
  const handleInputKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") {
        const addr = parseAddress(inputValue);
        if (!isNaN(addr) && addr >= 0 && addr <= (MEMORY_SIZE - 1)) {
          navigateTo(addr);
        }
      }
    },
    [inputValue, navigateTo],
  );

  /** Keyboard scrolling: arrow keys (1 row) and Page Up/Down (1 page). */
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (document.activeElement instanceof HTMLInputElement) return;
      const PAGE = BYTES_PER_ROW * ROWS_PER_PAGE;
      let delta = 0;
      if (e.key === "ArrowDown") delta = BYTES_PER_ROW;
      else if (e.key === "ArrowUp") delta = -BYTES_PER_ROW;
      else if (e.key === "PageDown") delta = PAGE;
      else if (e.key === "PageUp") delta = -PAGE;
      else return;
      e.preventDefault();
      const next = (pageAddrRef.current + delta + MEMORY_SIZE) & PARAGRAPH_MASK;
      fetchPage(next);
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [fetchPage]);

  /** Wheel scrolling: one row per tick. */
  const handleWheel = useCallback(
    (e: React.WheelEvent) => {
      e.preventDefault();
      const delta = e.deltaY > 0 ? BYTES_PER_ROW : -BYTES_PER_ROW;
      const next = (pageAddrRef.current + delta + MEMORY_SIZE) & PARAGRAPH_MASK;
      fetchPage(next);
    },
    [fetchPage],
  );

  const rows: React.ReactNode[] = [];
  for (let row = 0; row < ROWS_PER_PAGE; row++) {
    const rowAddr = (pageAddr + row * BYTES_PER_ROW) & (MEMORY_SIZE - 1);
    const slice = bytes.slice(row * BYTES_PER_ROW, (row + 1) * BYTES_PER_ROW);

    const hexLow = Array.from(slice.slice(0, 8))
      .map((b) => b.toString(16).toUpperCase().padStart(2, "0"))
      .join(" ");
    const hexHigh = Array.from(slice.slice(8, 16))
      .map((b) => b.toString(16).toUpperCase().padStart(2, "0"))
      .join(" ");
    const asciiLow = Array.from(slice.slice(0, 8)).map(toAsciiChar).join("");
    const asciiHigh = Array.from(slice.slice(8, 16)).map(toAsciiChar).join("");

    rows.push(
      <div key={rowAddr} className="mem-row">
        <span className="mem-addr">{fmtAddr(rowAddr)}:</span>
        <span className="mem-hex-group">{hexLow}</span>
        <span className="mem-hex-group">{hexHigh}</span>
        <span className="mem-ascii-group">{asciiLow}</span>
        <span className="mem-ascii-group">{asciiHigh}</span>
      </div>,
    );
  }

  return (
    <div className="memory-panel" onWheel={handleWheel}>
      <div className="memory-header">
        <span className="panel-title">Memory</span>
        <input
          className="mem-addr-input"
          value={inputValue}
          onChange={(e) => setInputValue(e.target.value)}
          onKeyDown={handleInputKeyDown}
          spellCheck={false}
          placeholder="$0000"
          title="Enter address and press Enter"
        />
      </div>
      <div className="memory-body">
        {!ready ? (
          <span className="memory-empty">Waiting for session…</span>
        ) : (
          rows
        )}
      </div>
    </div>
  );
}
