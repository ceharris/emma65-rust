import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ExecState } from "./DisassemblyPanel";
import "./styles/memory.scss";

/** Number of bytes per display row. */
const BYTES_PER_ROW = 16;

/** Number of rows in a page (256 bytes). */
const ROWS_PER_PAGE = 16;

/** Address mask for paragraph alignment */
const PARAGRAPH_MASK = 0xfff0;

/** Total size of the memory address space */
const MEMORY_SIZE = 0x10000;

/** Parse a hex address string (optional $/ 0x prefix; bare digits treated as hex). Returns NaN on failure. */
function parseAddress(input: string): number {
  const trimmed = input.trim();
  if (/^\$[0-9a-fA-F]+$/.test(trimmed)) {
    return parseInt(trimmed.slice(1), 16);
  }
  if (/^0x[0-9a-fA-F]+$/i.test(trimmed)) {
    return parseInt(trimmed, 16);
  }
  if (/^[0-9a-fA-F]+$/.test(trimmed)) {
    return parseInt(trimmed, 16);
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

/**
 * Parses a sequence of hex byte tokens separated by whitespace and/or single commas.
 * Each token must be one or two hex digits. Returns the parsed byte array,
 * or null if any token is invalid or the input is empty.
 */
function parseHexBytes(raw: string): number[] | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const tokens = trimmed.split(/\s*[\s,]\s*/).filter(Boolean);
  const bytes: number[] = [];
  for (const token of tokens) {
    if (!/^[0-9a-fA-F]{1,2}$/.test(token)) return null;
    bytes.push(parseInt(token, 16));
  }
  return bytes.length > 0 ? bytes : null;
}

/** State for the write-memory dialog; null means closed. */
interface WriteDialogState {
  /**
   * Target address for the first byte of the write.
   * Non-null when opened by double-click (pre-set); null when opened by keyboard shortcut.
   */
  addr: number | null;
  /** Controlled value of the editable address input (used only when addr is null). */
  addrInput: string;
  /** Validation error for the address field; empty string means no error. */
  addrError: string;
  /** Controlled value of the data input field. */
  inputValue: string;
  /** Validation or backend error message; empty string means no error. */
  errorMsg: string;
  /** "hex" when opened from the hex column or Alt+Shift+H; "utf8" from ASCII column or Alt+Shift+A. */
  mode: "hex" | "utf8";
}

interface Props {
  /** Current CPU execution state; used to guard double-click and key shortcuts when running. */
  execState: ExecState;
}

export default function MemoryPanel({ execState }: Props) {
  /** Paragraph-aligned start address of the currently displayed 256-byte page. */
  const [pageAddr, setPageAddr] = useState<number>(0x0000);
  /** Ref mirrors pageAddr so event listeners always see the current value. */
  const pageAddrRef = useRef<number>(0x0000);
  /** 256-byte buffer for the current page. */
  const [bytes, setBytes] = useState<Uint8Array>(new Uint8Array(256));
  /** Controlled value of the address input field. */
  const [inputValue, setInputValue] = useState<string>("0000");
  const [ready, setReady] = useState(false);
  /** Write-memory dialog state; null when closed. */
  const [writeDialog, setWriteDialog] = useState<WriteDialogState | null>(null);

  /** Fetch the 256-byte page starting at `addr` (must be paragraph-aligned). */
  const fetchPage = useCallback(async (addr: number) => {
    try {
      const result = await invoke<number[]>("get_memory", { addr });
      setBytes(new Uint8Array(result));
      pageAddrRef.current = addr;
      setPageAddr(addr);
      setInputValue(fmtAddr(addr));
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

  /** Alt+Shift+H / Alt+Shift+A: open write dialog at an arbitrary address. */
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (document.activeElement instanceof HTMLInputElement) return;
      if (execState !== "stopped" || writeDialog) return;
      if (e.altKey && e.shiftKey && e.code === "KeyH") {
        e.preventDefault();
        setWriteDialog({ addr: null, addrInput: "", addrError: "", inputValue: "", errorMsg: "", mode: "hex" });
      } else if (e.altKey && e.shiftKey && e.code === "KeyA") {
        e.preventDefault();
        setWriteDialog({ addr: null, addrInput: "", addrError: "", inputValue: "", errorMsg: "", mode: "utf8" });
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [execState, writeDialog]);

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

  /** Opens the hex write dialog for the byte at `addr`. */
  const handleByteDoubleClick = useCallback((addr: number) => {
    setWriteDialog({ addr, addrInput: "", addrError: "", inputValue: "", errorMsg: "", mode: "hex" });
  }, []);

  /** Opens the UTF-8 text write dialog for the byte at `addr`. */
  const handleAsciiCharDoubleClick = useCallback((addr: number) => {
    setWriteDialog({ addr, addrInput: "", addrError: "", inputValue: "", errorMsg: "", mode: "utf8" });
  }, []);

  /** Validates address and data, invokes write_memory, refreshes on success, shows errors on failure. */
  const commitWriteMemory = useCallback(async () => {
    if (!writeDialog) return;

    // Resolve address: pre-set from double-click, or parse from the editable input.
    let resolvedAddr: number;
    if (writeDialog.addr !== null) {
      resolvedAddr = writeDialog.addr;
    } else {
      const parsed = parseAddress(writeDialog.addrInput);
      if (isNaN(parsed) || parsed < 0 || parsed > 0xffff) {
        setWriteDialog((d) => d && { ...d, addrError: "Enter a valid hex address (0–FFFF)" });
        return;
      }
      resolvedAddr = parsed;
    }

    // Validate data.
    let data: number[];
    if (writeDialog.mode === "hex") {
      const parsed = parseHexBytes(writeDialog.inputValue);
      if (parsed === null) {
        setWriteDialog((d) => d && {
          ...d,
          errorMsg: "Enter one or more hex bytes (1–2 digits each), separated by spaces or commas",
        });
        return;
      }
      data = parsed;
    } else {
      if (!writeDialog.inputValue) {
        setWriteDialog((d) => d && { ...d, errorMsg: "Enter at least one character" });
        return;
      }
      // No trimming — spaces and all characters are written verbatim.
      data = Array.from(new TextEncoder().encode(writeDialog.inputValue));
    }

    try {
      await invoke("write_memory", { addr: resolvedAddr, data });
      setWriteDialog(null);
      fetchPage(pageAddrRef.current);
    } catch (e) {
      setWriteDialog((d) => d && { ...d, errorMsg: String(e) });
    }
  }, [writeDialog, fetchPage]);

  /** Dismiss the write dialog on Escape while it is open. */
  useEffect(() => {
    if (!writeDialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setWriteDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [writeDialog]);

  /** Builds per-character ASCII spans for one 8-byte half-row. */
  const makeAsciiSpans = useCallback(
    (halfSlice: Uint8Array, baseAddr: number) =>
      Array.from(halfSlice).map((b, i) => {
        const byteAddr = (baseAddr + i) & (MEMORY_SIZE - 1);
        return (
          <span
            key={i}
            className={`mem-ascii-char${execState !== "stopped" ? " locked" : ""}`}
            onDoubleClick={
              execState === "stopped"
                ? () => handleAsciiCharDoubleClick(byteAddr)
                : undefined
            }
          >
            {toAsciiChar(b)}
          </span>
        );
      }),
    [execState, handleAsciiCharDoubleClick],
  );

  /** Builds per-byte hex spans for one 8-byte half-row. */
  const makeHexSpans = useCallback(
    (halfSlice: Uint8Array, baseAddr: number) =>
      Array.from(halfSlice).map((b, i) => {
        const byteAddr = (baseAddr + i) & (MEMORY_SIZE - 1);
        return (
          <span
            key={i}
            className={`mem-hex-byte${execState !== "stopped" ? " locked" : ""}`}
            onDoubleClick={
              execState === "stopped"
                ? () => handleByteDoubleClick(byteAddr)
                : undefined
            }
          >
            {b.toString(16).toUpperCase().padStart(2, "0")}
          </span>
        );
      }),
    [execState, handleByteDoubleClick],
  );

  const rows: React.ReactNode[] = [];
  for (let row = 0; row < ROWS_PER_PAGE; row++) {
    const rowAddr = (pageAddr + row * BYTES_PER_ROW) & (MEMORY_SIZE - 1);
    const slice = bytes.slice(row * BYTES_PER_ROW, (row + 1) * BYTES_PER_ROW);
    rows.push(
      <div key={rowAddr} className="mem-row">
        <span className="mem-addr">{fmtAddr(rowAddr)}:</span>
        <span className="mem-hex-group">
          {makeHexSpans(slice.slice(0, 8), rowAddr)}
        </span>
        <span className="mem-hex-group">
          {makeHexSpans(slice.slice(8, 16), (rowAddr + 8) & (MEMORY_SIZE - 1))}
        </span>
        <span className="mem-ascii-group">
          {makeAsciiSpans(slice.slice(0, 8), rowAddr)}
        </span>
        <span className="mem-ascii-group">
          {makeAsciiSpans(slice.slice(8, 16), (rowAddr + 8) & (MEMORY_SIZE - 1))}
        </span>
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
          placeholder="0000"
          title="Enter hex address and press Enter"
        />
      </div>
      <div className="memory-body">
        {!ready ? (
          <span className="memory-empty">Waiting for session…</span>
        ) : (
          rows
        )}
      </div>
      {writeDialog && (
        <div
          className="mem-write-backdrop"
          onClick={() => setWriteDialog(null)}
          onWheel={(e) => e.stopPropagation()}
        >
          <div className="mem-write-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="mem-write-title">Write Memory</div>

            <div className="mem-write-field">
              <label className="mem-write-label">Address</label>
              {writeDialog.addr !== null ? (
                <input
                  className="mem-write-addr-display"
                  value={fmtAddr(writeDialog.addr)}
                  readOnly
                  disabled
                  tabIndex={-1}
                />
              ) : (
                <input
                  className={`mem-write-addr-input${writeDialog.addrError ? " invalid" : ""}`}
                  autoFocus
                  spellCheck={false}
                  placeholder="0000"
                  value={writeDialog.addrInput}
                  onChange={(e) =>
                    setWriteDialog((d) => d && { ...d, addrInput: e.target.value, addrError: "" })
                  }
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === "Enter") { e.preventDefault(); commitWriteMemory(); }
                    if (e.key === "Escape") { e.preventDefault(); setWriteDialog(null); }
                  }}
                />
              )}
            </div>

            {writeDialog.addrError && (
              <div className="mem-write-error">{writeDialog.addrError}</div>
            )}

            <div className="mem-write-field">
              <label className="mem-write-label">
                {writeDialog.mode === "hex" ? "Bytes" : "Text"}
              </label>
              <input
                className={`mem-write-data-input${writeDialog.errorMsg ? " invalid" : ""}`}
                autoFocus={writeDialog.addr !== null}
                spellCheck={writeDialog.mode === "utf8"}
                placeholder={writeDialog.mode === "hex" ? "e.g. 4C 00 06" : "Enter Unicode text"}
                value={writeDialog.inputValue}
                onChange={(e) =>
                  setWriteDialog((d) => d && { ...d, inputValue: e.target.value, errorMsg: "" })
                }
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitWriteMemory(); }
                  if (e.key === "Escape") { e.preventDefault(); setWriteDialog(null); }
                }}
              />
            </div>

            {writeDialog.errorMsg && (
              <div className="mem-write-error">{writeDialog.errorMsg}</div>
            )}

            <div className="mem-write-buttons">
              <button
                className="mem-write-btn mem-write-btn-cancel"
                onClick={() => setWriteDialog(null)}
              >
                Cancel
              </button>
              <button
                className="mem-write-btn mem-write-btn-ok"
                onClick={commitWriteMemory}
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
