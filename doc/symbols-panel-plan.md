# Symbols Panel implementation plan (issue #489)

## Goal

Add a dockable "Symbols" panel to the debugger that displays the live `SymbolTable` (as redesigned in issue #490) as a sortable, filterable table: Name, Address, Source, and a comma-separated Aliases column (other names mapped to the same address) with ellipsis truncation and click-to-expand. Default sort is by Name ascending; clicking a column header sorts by that column (Name/Address/Source), with Name as the secondary sort key when the primary key isn't Name; clicking the active sort column's header toggles ascending/descending. The panel header has a filter input matching a substring against name, address, source, or alias text.

## Background

`SymbolTable` (`src/emulator/bus/symbol.rs`) now tags every entry with a `SymbolSource` (`File(PathBuf)`, `Assembler`, `User`) and exposes `iter()` yielding `(name, source, address)` for every live entry across all sources, plus `names_for(address)` for reverse lookup. This panel is read-only (no add/edit/remove UI) — it just needs a point-in-time snapshot plus refresh-on-change.

Existing dockable panels (`BreakpointPanel`, `WatchpointPanel`, etc.) establish the wiring pattern: a `debugger/src-tauri/src/<name>.rs` command module, registration in `lib.rs`'s `invoke_handler`, an entry in `panelRegistry.tsx` (`MainPanelId`, `PANEL_TITLES`, `panelComponents`), a View-menu item in `menu.rs`, and a `debugger/frontend/src/styles/<name>.scss` stylesheet. No panel currently supports column-header sorting or alias overflow-with-popover, so those are new UI patterns for this panel specifically (though the popover styling should follow the existing hand-styled-control approach used for `SelectPopover`/`ColorPickerPopover`, since native controls don't pick up app theming).

There's currently no live "symbol table changed" event; the table changes when the assembler runs (`assembler.rs`), when a labels file is loaded (`memory.rs`), and on profile switch/load (`lib.rs`). A new `symbols-changed` broadcast is needed at each of those points so the panel refreshes without polling.

## Units

### Unit 1 — Backend: symbol snapshot command + change event

- New `debugger/src-tauri/src/symbols.rs`:
  - `SymbolRow { name: String, address: u16, source: String, aliases: Vec<String> }` (serde `Serialize`). `source` is a display string: `"User"`, `"Assembler"`, or `"File: <basename>"` (full canonical path in a tooltip client-side, if useful — decide during implementation whether to also return the raw path). `aliases` is every other live name at the same address, across all sources, excluding `name` itself, sorted for deterministic output.
  - `get_symbols(cpu_state) -> Vec<SymbolRow>` Tauri command: snapshots `cpu.bus().symbol_table().iter()` into rows. One row per `(name, source)` live entry (matching the table's own model — a name can legitimately appear more than once if multiple sources define it).
  - Unit tests covering: empty table, multiple sources for one name, alias computation, aliases excluding self.
- Register `mod symbols;` and `symbols::get_symbols` in `lib.rs`.
- Emit `app.emit("symbols-changed", ())` at the same points `memory-modified` is already emitted for symbol-table-affecting operations: `assembler.rs` post-assemble, `memory.rs` post-label-load, and `lib.rs` post-profile-load. (Emit unconditionally — the panel doesn't need the payload, just a re-fetch trigger, matching `memory-modified`'s pattern.)

### Unit 2 — Frontend: SymbolsPanel with table, default sort, filter, and registration

- New `debugger/frontend/src/SymbolsPanel.tsx`:
  - Fetches `get_symbols` on mount, re-fetches on `symbols-changed` (same `listen`/`invoke` pattern as `BreakpointPanel`).
  - Renders a table: Name, Address (hex, same `formatAddr` style as breakpoints), Source, Aliases.
  - Filter input in the panel's own top toolbar row (not a dockview tab-header action — `usePanelHeaderAction` only supports a single button, not free text input) — substring match, case-insensitive, against name/address(hex)/source/aliases-joined.
  - Default sort: Name ascending.
  - Column header click: sort by that column; Name stays primary key only when Name is clicked; for Address/Source as primary, Name is the secondary key. Clicking the currently-active sort column toggles asc/desc; clicking a different column switches primary key and resets to ascending.
- New `debugger/frontend/src/styles/symbols.scss`, following existing panel stylesheet conventions.
- Registration: add `"symbols"` to `MainPanelId`, `PANEL_TITLES`, `panelComponents` in `panelRegistry.tsx`; add a View-menu entry in `menu.rs`'s `view_panels` array (lexically ordered, per that array's existing convention); give it a sensible default dock position in `DockLayout.tsx`'s default layout.
- Alias column in this unit: simple truncation via CSS `text-overflow: ellipsis` (no click-to-expand yet) — ships a working, if not fully spec-compliant, panel.

### Unit 3 — Alias overflow: ellipsis click-to-expand

- Replace the CSS-only truncation with an explicit affordance: detect overflow (or just always show a trailing `…` when there's more than one alias that doesn't fit / matches some threshold) and make it clickable.
- Clicking opens a small popover (styled per the existing hand-rolled popover pattern, not a native `<select>`/tooltip) anchored to the cell, showing the full alias list wrapped normally.
- Dismiss on outside click / Escape, consistent with `BreakpointPanel`'s add-dialog dismiss handling.

## Open questions to confirm before/during implementation

- Exact `source` display string for `File(path)` — basename only, or full path? (Leaning basename + full path in a `title` tooltip.)
- Row identity when a name has multiple sources (e.g. both a `.lbl` file and `Assembler` define `START`): the plan shows one row per live `(name, source)` entry rather than collapsing to the precedence-resolved address only, since the issue's "Source" column implies every source is visible. Confirm this matches intent.
