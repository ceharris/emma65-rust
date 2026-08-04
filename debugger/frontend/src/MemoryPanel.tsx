import {useCallback, useEffect, useRef, useState} from "react";
import {listen} from "@tauri-apps/api/event";
import {invoke} from "@tauri-apps/api/core";
import {open as openFileDialog} from "@tauri-apps/plugin-dialog";
import {ExecState} from "./DisassemblyPanel";
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

/** State for the edit-memory dialog; null means closed. */
interface EditDialogState {
  /**
   * Target address for the first byte of the write.
   * Non-null when opened by double-click (pre-set); null when opened by keyboard shortcut or the Edit button.
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
  /** "hex" for hexadecimal byte input, "utf8" for Unicode character input; switchable via the dialog's radio group. */
  mode: "hex" | "utf8";
  /** When true, the write uses Bus::patch to bypass ROM write protection. */
  allowRomOverwrite: boolean;
}

/** State for the load-memory dialog; null means closed. */
interface LoadDialogState {
  /** Path to the file to load. */
  path: string;
  /** Validation error for the path field; empty string means no error. */
  pathError: string;
  /** Selected load format; null when no recognizable extension and no manual selection. */
  format: "image" | "intel_hex" | "motorola_srec" | null;
  /** Validation error for the format field; empty string means no error. */
  formatError: string;
  /** Controlled value of the load address input (hex, meaningful only for binary image). */
  loadAddress: string;
  /** Validation error for the load address field; empty string means no error. */
  loadAddressError: string;
  /** Optional path to a VICE labels symbol file; empty string means none. */
  symbolPath: string;
}

/** Deduces the load format from a file path's extension. Returns null if unrecognized. */
function formatFromPath(path: string): LoadDialogState["format"] {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "bin" || ext === "rom") return "image";
  if (ext === "hex" || ext === "ihx" || ext === "ihex") return "intel_hex";
  if (ext === "s19" || ext === "srec") return "motorola_srec";
  return null;
}

/** Human-readable label for each load format. */
const FORMAT_LABELS: Record<NonNullable<LoadDialogState["format"]>, string> = {
  image: "Binary Image",
  intel_hex: "Intel Hex",
  motorola_srec: "Motorola S-Record",
};

/** State for the fill-memory dialog; null means closed. */
interface FillDialogState {
  /** Controlled value of the start address input. */
  startInput: string;
  /** Validation error for the start address field; empty string means no error. */
  startError: string;
  /** Controlled value of the end address input. */
  endInput: string;
  /** Validation error for the end address field; empty string means no error. */
  endError: string;
  /** Controlled value of the fill byte input (1–2 hex digits). */
  fillValue: string;
  /** Validation error for the fill value field; empty string means no error. */
  fillError: string;
  /** When true, uses Bus::patch to bypass ROM write protection. */
  allowRomOverwrite: boolean;
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
  /** Edit-memory dialog state; null when closed. */
  const [editDialog, setEditDialog] = useState<EditDialogState | null>(null);
  /** Load-file dialog state; null when closed. */
  const [loadDialog, setLoadDialog] = useState<LoadDialogState | null>(null);
  /** Error message from a failed load operation; null when no error dialog is open. */
  const [loadErrorDialog, setLoadErrorDialog] = useState<string | null>(null);
  /** Fill-memory dialog state; null when closed. */
  const [fillDialog, setFillDialog] = useState<FillDialogState | null>(null);
  /** Symbol names indexed by page offset (0–255); empty array means no symbols at that address. */
  const [pageSymbols, setPageSymbols] = useState<string[][]>([]);

  /** Fetch the 256-byte page starting at `addr` (must be paragraph-aligned). */
  const fetchPage = useCallback(async (addr: number) => {
    try {
      const [result, symbols] = await Promise.all([
        invoke<number[]>("get_memory", { addr }),
        invoke<string[][]>("get_symbols_for_range", { start: addr, count: 256 })
          .catch(() => [] as string[][]),
      ]);
      setBytes(new Uint8Array(result));
      setPageSymbols(symbols);
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
    async (e: React.KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") {
        let addr: number | null = null;
        try {
          addr = await invoke<number | null>("resolve_symbol", { name: inputValue.trim() });
        } catch {
          // symbol resolution unavailable; fall through to hex parse
        }
        if (addr === null) {
          const parsed = parseAddress(inputValue);
          if (!isNaN(parsed) && parsed >= 0 && parsed <= (MEMORY_SIZE - 1)) addr = parsed;
        }
        if (addr !== null) navigateTo(addr);
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

  /** Alt+Shift+H / Alt+Shift+A: open edit dialog at an arbitrary address. */
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (document.activeElement instanceof HTMLInputElement) return;
      if (execState !== "stopped" || editDialog) return;
      if (e.altKey && e.shiftKey && e.code === "KeyH") {
        e.preventDefault();
        setEditDialog({ addr: null, addrInput: "", addrError: "", inputValue: "", errorMsg: "", mode: "hex", allowRomOverwrite: false });
      } else if (e.altKey && e.shiftKey && e.code === "KeyA") {
        e.preventDefault();
        setEditDialog({ addr: null, addrInput: "", addrError: "", inputValue: "", errorMsg: "", mode: "utf8", allowRomOverwrite: false });
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [execState, editDialog]);

  /** Alt+F: open load-file dialog. */
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (document.activeElement instanceof HTMLInputElement) return;
      if (execState !== "stopped" || loadDialog || editDialog) return;
      if (e.altKey && !e.shiftKey && e.code === "KeyF") {
        e.preventDefault();
        setLoadDialog({ path: "", pathError: "", format: null, formatError: "", loadAddress: "0000", loadAddressError: "", symbolPath: "" });
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [execState, loadDialog, editDialog]);

  /** Alt+Shift+F: open fill-memory dialog. */
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (document.activeElement instanceof HTMLInputElement) return;
      if (execState !== "stopped" || fillDialog || loadDialog || editDialog) return;
      if (e.altKey && e.shiftKey && e.code === "KeyF") {
        e.preventDefault();
        const endAddr = (pageAddrRef.current + 0xff) & 0xffff;
        setFillDialog({ startInput: fmtAddr(pageAddrRef.current), startError: "", endInput: fmtAddr(endAddr), endError: "", fillValue: "00", fillError: "", allowRomOverwrite: false });
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [execState, fillDialog, loadDialog, editDialog]);

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

  /** Opens the edit dialog in hex mode for the byte at `addr`. */
  const handleByteDoubleClick = useCallback((addr: number) => {
    setEditDialog({ addr, addrInput: "", addrError: "", inputValue: "", errorMsg: "", mode: "hex", allowRomOverwrite: false });
  }, []);

  /** Opens the edit dialog in Unicode mode for the byte at `addr`. */
  const handleAsciiCharDoubleClick = useCallback((addr: number) => {
    setEditDialog({ addr, addrInput: "", addrError: "", inputValue: "", errorMsg: "", mode: "utf8", allowRomOverwrite: false });
  }, []);

  /** Validates address and data, invokes write_memory, refreshes on success, shows errors on failure. */
  const commitEditMemory = useCallback(async () => {
    if (!editDialog) return;

    // Resolve address: pre-set from double-click, or parse from the editable input.
    let resolvedAddr: number;
    if (editDialog.addr !== null) {
      resolvedAddr = editDialog.addr;
    } else {
      const parsed = parseAddress(editDialog.addrInput);
      if (isNaN(parsed) || parsed < 0 || parsed > 0xffff) {
        setEditDialog((d) => d && { ...d, addrError: "Enter a valid hex address (0–FFFF)" });
        return;
      }
      resolvedAddr = parsed;
    }

    // Validate data.
    let data: number[];
    if (editDialog.mode === "hex") {
      const parsed = parseHexBytes(editDialog.inputValue);
      if (parsed === null) {
        setEditDialog((d) => d && {
          ...d,
          errorMsg: "Enter one or more hex bytes (1–2 digits each), separated by spaces or commas",
        });
        return;
      }
      data = parsed;
    } else {
      if (!editDialog.inputValue) {
        setEditDialog((d) => d && { ...d, errorMsg: "Enter at least one character" });
        return;
      }
      // No trimming — spaces and all characters are written verbatim.
      data = Array.from(new TextEncoder().encode(editDialog.inputValue));
    }

    try {
      await invoke("write_memory", { addr: resolvedAddr, data, patch: editDialog.allowRomOverwrite });
      setEditDialog(null);
      fetchPage(pageAddrRef.current);
    } catch (e) {
      setEditDialog((d) => d && { ...d, errorMsg: String(e) });
    }
  }, [editDialog, fetchPage]);

  /** Dismiss the edit dialog on Escape while it is open. */
  useEffect(() => {
    if (!editDialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setEditDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [editDialog]);

  /** Opens the native file chooser and fills the path + auto-selects format. */
  const handleChooseFile = useCallback(async () => {
    const selected = await openFileDialog({
      multiple: false,
      filters: [{ name: "Memory Files", extensions: ["bin", "rom", "hex", "ihx", "ihex", "s19", "srec"] }],
    });
    if (typeof selected === "string") {
      setLoadDialog((d) => d && {
        ...d,
        path: selected,
        pathError: "",
        format: formatFromPath(selected),
        formatError: "",
      });
    }
  }, []);

  /** Opens the native file chooser for a symbol file; defaults to the directory of the load file. */
  const handleChooseSymbolFile = useCallback(async () => {
    const dir = loadDialog?.path.trim().replace(/\/[^/]*$/, "") || undefined;
    const selected = await openFileDialog({
      multiple: false,
      filters: [{ name: "Symbol Files", extensions: ["lbl", "sym", "map", "txt"] }],
      defaultPath: dir,
    });
    if (typeof selected === "string") {
      setLoadDialog((d) => d && { ...d, symbolPath: selected });
    }
  }, [loadDialog]);

  /** Validates inputs, invokes load_memory, refreshes the memory view, shows an error dialog on failure. */
  const commitLoadMemory = useCallback(async () => {
    if (!loadDialog) return;

    if (!loadDialog.path.trim()) {
      setLoadDialog((d) => d && { ...d, pathError: "Enter a file path" });
      return;
    }

    if (!loadDialog.format) {
      setLoadDialog((d) => d && { ...d, formatError: "Select a load format" });
      return;
    }

    let bias = 0;
    if (loadDialog.format === "image") {
      const parsed = parseAddress(loadDialog.loadAddress);
      if (isNaN(parsed) || parsed < 0 || parsed > 0xffff) {
        setLoadDialog((d) => d && { ...d, loadAddressError: "Enter a valid hex address (0–FFFF)" });
        return;
      }
      bias = parsed;
    }

    setLoadDialog(null);
    try {
      await invoke("load_memory", {
        path: loadDialog.path,
        format: loadDialog.format,
        bias,
        symbolPath: loadDialog.symbolPath.trim() || null,
      });
      fetchPage(pageAddrRef.current);
    } catch (e) {
      setLoadErrorDialog(String(e));
    }
  }, [loadDialog, fetchPage]);

  /** Dismiss the load dialog on Escape while it is open. */
  useEffect(() => {
    if (!loadDialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setLoadDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [loadDialog]);

  /** Dismiss the load error dialog on Escape while it is open. */
  useEffect(() => {
    if (!loadErrorDialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setLoadErrorDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [loadErrorDialog]);

  /** Validates inputs and invokes fill_memory; refreshes the view on success. */
  const commitFillMemory = useCallback(async () => {
    if (!fillDialog) return;

    const start = parseAddress(fillDialog.startInput);
    if (isNaN(start) || start < 0 || start > 0xffff) {
      setFillDialog((d) => d && { ...d, startError: "Enter a valid hex address (0–FFFF)" });
      return;
    }

    const end = parseAddress(fillDialog.endInput);
    if (isNaN(end) || end < 0 || end > 0xffff) {
      setFillDialog((d) => d && { ...d, endError: "Enter a valid hex address (0–FFFF)" });
      return;
    }
    if (end < start) {
      setFillDialog((d) => d && { ...d, endError: "End address must be ≥ start address" });
      return;
    }

    const fillTrimmed = fillDialog.fillValue.trim();
    if (!/^[0-9a-fA-F]{1,2}$/.test(fillTrimmed)) {
      setFillDialog((d) => d && { ...d, fillError: "Enter a hex byte value (00–FF)" });
      return;
    }
    const value = parseInt(fillTrimmed, 16);

    setFillDialog(null);
    try {
      await invoke("fill_memory", { start, end, value, patch: fillDialog.allowRomOverwrite });
      fetchPage(pageAddrRef.current);
    } catch (e) {
      setLoadErrorDialog(String(e));
    }
  }, [fillDialog, fetchPage]);

  /** Dismiss the fill dialog on Escape while it is open. */
  useEffect(() => {
    if (!fillDialog) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") setFillDialog(null);
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [fillDialog]);

  /** Builds per-character ASCII spans for one 8-byte half-row. */
  const makeAsciiSpans = useCallback(
    (halfSlice: Uint8Array, baseAddr: number) =>
      Array.from(halfSlice).map((b, i) => {
        const byteAddr = (baseAddr + i) & (MEMORY_SIZE - 1);
        const offset = (byteAddr - pageAddr + MEMORY_SIZE) % MEMORY_SIZE;
        const names = pageSymbols[offset];
        const title = names?.length ? names.join("\n") : undefined;
        return (
          <span
            key={i}
            className={`mem-ascii-char${execState !== "stopped" ? " locked" : ""}`}
            title={title}
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
    [execState, handleAsciiCharDoubleClick, pageAddr, pageSymbols],
  );

  /** Builds per-byte hex spans for one 8-byte half-row. */
  const makeHexSpans = useCallback(
    (halfSlice: Uint8Array, baseAddr: number) =>
      Array.from(halfSlice).map((b, i) => {
        const byteAddr = (baseAddr + i) & (MEMORY_SIZE - 1);
        const offset = (byteAddr - pageAddr + MEMORY_SIZE) % MEMORY_SIZE;
        const names = pageSymbols[offset];
        const title = names?.length ? names.join("\n") : undefined;
        return (
          <span
            key={i}
            className={`mem-hex-byte${execState !== "stopped" ? " locked" : ""}`}
            title={title}
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
    [execState, handleByteDoubleClick, pageAddr, pageSymbols],
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
        <div className="mem-header-left">
          <span className="panel-title">Memory</span>
          <button
            className="mem-load-btn"
            onClick={() => setLoadDialog({ path: "", pathError: "", format: null, formatError: "", loadAddress: "0000", loadAddressError: "", symbolPath: "" })}
            disabled={execState !== "stopped"}
            title="Load file into memory (Alt+F)"
          >
            Load
          </button>
          <button
            className="mem-fill-btn"
            onClick={() => {
              const endAddr = (pageAddrRef.current + 0xff) & 0xffff;
              setFillDialog({ startInput: fmtAddr(pageAddr), startError: "", endInput: fmtAddr(endAddr), endError: "", fillValue: "00", fillError: "", allowRomOverwrite: false });
            }}
            disabled={execState !== "stopped"}
            title="Fill memory range (Alt+Shift+F)"
          >
            Fill
          </button>
          <button
            className="mem-edit-btn"
            onClick={() => setEditDialog({ addr: null, addrInput: "", addrError: "", inputValue: "", errorMsg: "", mode: "hex", allowRomOverwrite: false })}
            disabled={execState !== "stopped"}
            title="Edit memory (Alt+Shift+H)"
          >
            Edit
          </button>
        </div>
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
      {fillDialog && (
        <div
          className="mem-fill-backdrop"
          onClick={() => setFillDialog(null)}
          onWheel={(e) => e.stopPropagation()}
        >
          <div className="mem-fill-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="mem-fill-title">Fill Memory</div>

            <div className="mem-fill-field">
              <label className="mem-fill-label">Start Address</label>
              <input
                className={`mem-fill-input${fillDialog.startError ? " invalid" : ""}`}
                autoFocus
                spellCheck={false}
                placeholder="0000"
                value={fillDialog.startInput}
                onChange={(e) =>
                  setFillDialog((d) => d && { ...d, startInput: e.target.value, startError: "" })
                }
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitFillMemory(); }
                  if (e.key === "Escape") { e.preventDefault(); setFillDialog(null); }
                }}
              />
            </div>

            {fillDialog.startError && (
              <div className="mem-fill-error">{fillDialog.startError}</div>
            )}

            <div className="mem-fill-field">
              <label className="mem-fill-label">End Address</label>
              <input
                className={`mem-fill-input${fillDialog.endError ? " invalid" : ""}`}
                spellCheck={false}
                placeholder="00FF"
                value={fillDialog.endInput}
                onChange={(e) =>
                  setFillDialog((d) => d && { ...d, endInput: e.target.value, endError: "" })
                }
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitFillMemory(); }
                  if (e.key === "Escape") { e.preventDefault(); setFillDialog(null); }
                }}
              />
            </div>

            {fillDialog.endError && (
              <div className="mem-fill-error">{fillDialog.endError}</div>
            )}

            <div className="mem-fill-field">
              <label className="mem-fill-label">Fill Value</label>
              <input
                className={`mem-fill-input${fillDialog.fillError ? " invalid" : ""}`}
                spellCheck={false}
                placeholder="00"
                value={fillDialog.fillValue}
                onChange={(e) =>
                  setFillDialog((d) => d && { ...d, fillValue: e.target.value, fillError: "" })
                }
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitFillMemory(); }
                  if (e.key === "Escape") { e.preventDefault(); setFillDialog(null); }
                }}
              />
            </div>

            {fillDialog.fillError && (
              <div className="mem-fill-error">{fillDialog.fillError}</div>
            )}

            <label className="mem-fill-rom-overwrite">
              <input
                type="checkbox"
                checked={fillDialog.allowRomOverwrite}
                onChange={(e) =>
                  setFillDialog((d) => d && { ...d, allowRomOverwrite: e.target.checked })
                }
              />
              Allow ROM Overwrite
            </label>

            <div className="mem-fill-buttons">
              <button
                className="mem-fill-btn-action mem-fill-btn-cancel"
                onClick={() => setFillDialog(null)}
              >
                Cancel
              </button>
              <button
                className="mem-fill-btn-action mem-fill-btn-ok"
                onClick={commitFillMemory}
              >
                OK
              </button>
            </div>
          </div>
        </div>
      )}

      {loadDialog && (
        <div
          className="mem-load-backdrop"
          onClick={() => setLoadDialog(null)}
          onWheel={(e) => e.stopPropagation()}
        >
          <div className="mem-load-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="mem-load-title">Load File</div>

            <div className="mem-load-path-row">
              <input
                className={`mem-load-path-input${loadDialog.pathError ? " invalid" : ""}`}
                autoFocus
                spellCheck={false}
                placeholder="Path to file"
                value={loadDialog.path}
                onChange={(e) =>
                  setLoadDialog((d) => d && {
                    ...d,
                    path: e.target.value,
                    pathError: "",
                    format: formatFromPath(e.target.value),
                    formatError: "",
                  })
                }
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitLoadMemory(); }
                  if (e.key === "Escape") { e.preventDefault(); setLoadDialog(null); }
                }}
              />
              <button
                className="mem-load-folder-btn"
                onClick={handleChooseFile}
                title="Browse for file"
              >
                📁
              </button>
            </div>

            {loadDialog.pathError && (
              <div className="mem-load-error">{loadDialog.pathError}</div>
            )}

            <div className="mem-load-format-group">
              {(["image", "intel_hex", "motorola_srec"] as const).map((fmt) => (
                <label key={fmt} className="mem-load-format-option">
                  <input
                    type="radio"
                    name="load-format"
                    checked={loadDialog.format === fmt}
                    onChange={() =>
                      setLoadDialog((d) => d && { ...d, format: fmt, formatError: "" })
                    }
                  />
                  {FORMAT_LABELS[fmt]}
                </label>
              ))}
            </div>

            {loadDialog.formatError && (
              <div className="mem-load-error">{loadDialog.formatError}</div>
            )}

            <div className="mem-load-field">
              <label className="mem-load-label">Load Address</label>
              <input
                className={`mem-load-addr-input${loadDialog.loadAddressError ? " invalid" : ""}`}
                spellCheck={false}
                placeholder="0000"
                value={loadDialog.format === "image" ? loadDialog.loadAddress : "0000"}
                disabled={loadDialog.format !== "image"}
                onChange={(e) =>
                  setLoadDialog((d) => d && { ...d, loadAddress: e.target.value, loadAddressError: "" })
                }
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitLoadMemory(); }
                  if (e.key === "Escape") { e.preventDefault(); setLoadDialog(null); }
                }}
              />
            </div>

            {loadDialog.loadAddressError && (
              <div className="mem-load-error">{loadDialog.loadAddressError}</div>
            )}

            <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
              <label style={{ color: "var(--color-muted)", fontSize: "var(--font-size-btn)" }}>Symbol File</label>
              <div className="mem-load-path-row">
                <input
                  className="mem-load-path-input"
                  spellCheck={false}
                  placeholder="Path to symbol file (optional)"
                  value={loadDialog.symbolPath}
                  onChange={(e) =>
                    setLoadDialog((d) => d && { ...d, symbolPath: e.target.value })
                  }
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === "Enter") { e.preventDefault(); commitLoadMemory(); }
                    if (e.key === "Escape") { e.preventDefault(); setLoadDialog(null); }
                  }}
                />
                <button
                  className="mem-load-folder-btn"
                  onClick={handleChooseSymbolFile}
                  title="Browse for symbol file"
                >
                  📁
                </button>
              </div>
            </div>

            <div className="mem-load-rom-note">
              The load operation will bypass read-only restrictions for any ROM region targeted by the load.
            </div>

            <div className="mem-load-buttons">
              <button
                className="mem-load-btn-action mem-load-btn-cancel"
                onClick={() => setLoadDialog(null)}
              >
                Cancel
              </button>
              <button
                className="mem-load-btn-action mem-load-btn-ok"
                onClick={commitLoadMemory}
              >
                OK
              </button>
            </div>
          </div>
        </div>
      )}

      {loadErrorDialog !== null && (
        <div
          className="mem-load-backdrop"
          onClick={() => setLoadErrorDialog(null)}
          onWheel={(e) => e.stopPropagation()}
        >
          <div className="mem-load-error-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="mem-load-title">Load Error</div>
            <div className="mem-load-error-message">{loadErrorDialog}</div>
            <div className="mem-load-buttons">
              <button
                className="mem-load-btn-action mem-load-btn-ok"
                onClick={() => setLoadErrorDialog(null)}
              >
                Dismiss
              </button>
            </div>
          </div>
        </div>
      )}

      {editDialog && (
        <div
          className="mem-edit-backdrop"
          onClick={() => setEditDialog(null)}
          onWheel={(e) => e.stopPropagation()}
        >
          <div className="mem-edit-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="mem-edit-title">Edit Memory</div>

            <div className="mem-edit-field">
              <label className="mem-edit-label">Address</label>
              {editDialog.addr !== null ? (
                <input
                  className="mem-edit-addr-display"
                  value={fmtAddr(editDialog.addr)}
                  readOnly
                  disabled
                  tabIndex={-1}
                />
              ) : (
                <input
                  className={`mem-edit-addr-input${editDialog.addrError ? " invalid" : ""}`}
                  autoFocus
                  spellCheck={false}
                  placeholder="0000"
                  value={editDialog.addrInput}
                  onChange={(e) =>
                    setEditDialog((d) => d && { ...d, addrInput: e.target.value, addrError: "" })
                  }
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === "Enter") { e.preventDefault(); commitEditMemory(); }
                    if (e.key === "Escape") { e.preventDefault(); setEditDialog(null); }
                  }}
                />
              )}
            </div>

            {editDialog.addrError && (
              <div className="mem-edit-error">{editDialog.addrError}</div>
            )}

            <div className="mem-edit-mode-group">
              <label className="mem-edit-mode-option">
                <input
                  type="radio"
                  name="edit-mode"
                  checked={editDialog.mode === "hex"}
                  onChange={() =>
                    setEditDialog((d) => d && { ...d, mode: "hex", inputValue: "", errorMsg: "" })
                  }
                />
                Hexadecimal
              </label>
              <label className="mem-edit-mode-option">
                <input
                  type="radio"
                  name="edit-mode"
                  checked={editDialog.mode === "utf8"}
                  onChange={() =>
                    setEditDialog((d) => d && { ...d, mode: "utf8", inputValue: "", errorMsg: "" })
                  }
                />
                ASCII/Unicode Text
              </label>
            </div>

            <label className="mem-edit-rom-overwrite">
              <input
                type="checkbox"
                checked={editDialog.allowRomOverwrite}
                onChange={(e) =>
                  setEditDialog((d) => d && { ...d, allowRomOverwrite: e.target.checked })
                }
              />
              Allow ROM Overwrite
            </label>

            <div className="mem-edit-field">
              <label className="mem-edit-label">
                {editDialog.mode === "hex" ? "Bytes" : "Text"}
              </label>
              <input
                className={`mem-edit-data-input${editDialog.errorMsg ? " invalid" : ""}`}
                autoFocus={editDialog.addr !== null}
                spellCheck={editDialog.mode === "utf8"}
                placeholder={editDialog.mode === "hex" ? "e.g. 4C 00 06" : "ASCII or Unicode text"}
                value={editDialog.inputValue}
                onChange={(e) =>
                  setEditDialog((d) => d && { ...d, inputValue: e.target.value, errorMsg: "" })
                }
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") { e.preventDefault(); commitEditMemory(); }
                  if (e.key === "Escape") { e.preventDefault(); setEditDialog(null); }
                }}
              />
            </div>

            {editDialog.errorMsg && (
              <div className="mem-edit-error">{editDialog.errorMsg}</div>
            )}

            <div className="mem-edit-buttons">
              <button
                className="mem-edit-btn-action mem-edit-btn-cancel"
                onClick={() => setEditDialog(null)}
              >
                Cancel
              </button>
              <button
                className="mem-edit-btn-action mem-edit-btn-ok"
                onClick={commitEditMemory}
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
