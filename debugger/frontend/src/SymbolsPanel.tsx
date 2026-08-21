import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import "./styles/symbols.scss";

interface SymbolRow {
  name: string;
  address: number;
  source: string;
  source_path: string | null;
  aliases: string[];
}

type SortColumn = "name" | "address" | "source";
type SortDirection = "asc" | "desc";

/** User-resizable column widths, in CSS pixels — Aliases always fills whatever space remains. */
type ResizableColumn = "name" | "address" | "source";
type ColumnWidths = Record<ResizableColumn, number>;

// Mirrors preferences.rs's SymbolsColumnWidths defaults, so the panel's
// first paint (before the async fetch resolves) already matches what a
// fresh ui.toml would report.
const DEFAULT_COLUMN_WIDTHS: ColumnWidths = { name: 140, address: 68, source: 160 };

const MIN_COLUMN_WIDTH = 40;

function formatAddr(addr: number): string {
  return addr.toString(16).toUpperCase().padStart(4, "0");
}

/**
 * Compares two rows by `column`, breaking ties by name ascending unless
 * `column` already is "name" — matches the panel spec: clicking Address or
 * Source sorts primarily by that column with Name as the secondary key,
 * while clicking Name sorts by Name alone.
 */
function compareRows(a: SymbolRow, b: SymbolRow, column: SortColumn): number {
  switch (column) {
    case "name":
      return a.name.localeCompare(b.name);
    case "address":
      return a.address - b.address || a.name.localeCompare(b.name);
    case "source":
      return a.source.localeCompare(b.source) || a.name.localeCompare(b.name);
  }
}

function matchesFilter(row: SymbolRow, needle: string): boolean {
  if (!needle) return true;
  return (
    row.name.toLowerCase().includes(needle) ||
    formatAddr(row.address).toLowerCase().includes(needle) ||
    row.source.toLowerCase().includes(needle) ||
    row.aliases.join(", ").toLowerCase().includes(needle)
  );
}

export default function SymbolsPanel() {
  const [rows, setRows] = useState<SymbolRow[] | null>(null);
  const [filter, setFilter] = useState("");
  const [sortColumn, setSortColumn] = useState<SortColumn>("name");
  const [sortDirection, setSortDirection] = useState<SortDirection>("asc");
  const [colWidths, setColWidths] = useState<ColumnWidths>(DEFAULT_COLUMN_WIDTHS);

  useEffect(() => {
    const fetchSymbols = () => {
      invoke<SymbolRow[]>("get_symbols").then(setRows).catch((e) => console.error("get_symbols failed:", e));
    };
    fetchSymbols();
    const unlistenPromise = listen("symbols-changed", fetchSymbols);
    return () => { unlistenPromise.then((f) => f()); };
  }, []);

  useEffect(() => {
    invoke<ColumnWidths>("get_symbols_column_widths").then(setColWidths).catch((e) => console.error("get_symbols_column_widths failed:", e));
  }, []);

  const displayRows = useMemo(() => {
    if (rows === null) return null;
    const needle = filter.trim().toLowerCase();
    const filtered = rows.filter((row) => matchesFilter(row, needle));
    const sorted = filtered.sort((a, b) => compareRows(a, b, sortColumn));
    if (sortDirection === "desc") sorted.reverse();
    return sorted;
  }, [rows, filter, sortColumn, sortDirection]);

  /** Clicking the active sort column toggles direction; a different column becomes primary, sorted ascending. */
  const handleHeaderClick = (column: SortColumn) => {
    if (column === sortColumn) {
      setSortDirection((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortColumn(column);
      setSortDirection("asc");
    }
  };

  const sortIndicator = (column: SortColumn) => (column === sortColumn ? (sortDirection === "asc" ? " ▲" : " ▼") : "");

  // Drag-to-resize a column boundary. Tracked via a ref (not state) so the
  // window-level mousemove/mouseup listeners below can be registered once,
  // rather than re-subscribed on every pixel of drag.
  const dragRef = useRef<{ column: ResizableColumn; startX: number; startWidth: number } | null>(null);
  const [draggingColumn, setDraggingColumn] = useState<ResizableColumn | null>(null);

  const handleResizeStart = useCallback((column: ResizableColumn, e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    dragRef.current = { column, startX: e.clientX, startWidth: colWidths[column] };
    setDraggingColumn(column);
    // Dragging across row text would otherwise start a text selection.
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
  }, [colWidths]);

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      const drag = dragRef.current;
      if (!drag) return;
      const width = Math.max(MIN_COLUMN_WIDTH, drag.startWidth + (e.clientX - drag.startX));
      setColWidths((w) => ({ ...w, [drag.column]: width }));
    };
    const handleMouseUp = () => {
      if (!dragRef.current) return;
      dragRef.current = null;
      setDraggingColumn(null);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
      setColWidths((w) => {
        invoke("set_symbols_column_widths", { widths: w }).catch((e) => console.error("set_symbols_column_widths failed:", e));
        return w;
      });
    };
    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
    return () => {
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };
  }, []);

  const resizeHandle = (column: ResizableColumn) => (
    <span
      className={`symbols-col-resize-handle${draggingColumn === column ? " active" : ""}`}
      onMouseDown={(e) => handleResizeStart(column, e)}
      onClick={(e) => e.stopPropagation()}
    />
  );

  return (
    <div className="symbols-panel">
      <div className="symbols-toolbar">
        <input
          className="symbols-filter-input"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          spellCheck={false}
          placeholder="Filter symbols…"
        />
      </div>

      {rows === null || displayRows === null ? (
        <span className="symbols-empty">Waiting…</span>
      ) : displayRows.length === 0 ? (
        <span className="symbols-empty">{rows.length === 0 ? "No symbols" : "No matching symbols"}</span>
      ) : (
        <div className="symbols-table">
          <div className="symbols-header-row">
            <span className="symbols-col-name resizable sortable" style={{ flexBasis: colWidths.name }} onClick={() => handleHeaderClick("name")}>
              Name{sortIndicator("name")}
              {resizeHandle("name")}
            </span>
            <span className="symbols-col-address resizable sortable" style={{ flexBasis: colWidths.address }} onClick={() => handleHeaderClick("address")}>
              Address{sortIndicator("address")}
              {resizeHandle("address")}
            </span>
            <span className="symbols-col-source resizable sortable" style={{ flexBasis: colWidths.source }} onClick={() => handleHeaderClick("source")}>
              Source{sortIndicator("source")}
              {resizeHandle("source")}
            </span>
            <span className="symbols-col-aliases">Aliases</span>
          </div>
          <div className="symbols-body">
            {displayRows.map((row, index) => (
              <div key={`${row.name}-${row.source}-${index}`} className="symbols-row">
                <span className="symbols-col-name" style={{ flexBasis: colWidths.name }} title={row.name}>{row.name}</span>
                <span className="symbols-col-address" style={{ flexBasis: colWidths.address }}>{formatAddr(row.address)}</span>
                <span className="symbols-col-source" style={{ flexBasis: colWidths.source }} title={row.source_path ?? row.source}>{row.source}</span>
                <span className="symbols-col-aliases" title={row.aliases.join(", ")}>{row.aliases.join(", ")}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
