# Plan: Debugger integration for issue #474 (assembler)

## Context

Issue #474's foundational scope — a bespoke 6502/65C02 assembler library at
`src/assembler/` (`pub fn assemble(source: &str) -> Result<AssembledProgram, Vec<Error>>`)
— is complete and merged to `main` (PRs #475–480). The issue stays open
pending its remaining named scope, per the tracking comment posted on the
issue: wiring the assembler into the debugger UI via **a Tauri command, `Bus`
write-through using the existing `Bus::patch`, and a CodeMirror-based editor
panel**. This plan breaks that remaining work into four sequential units,
each its own branch + PR, following this repo's established per-unit
workflow (used for issues #462, #467, and #474's own 5-unit library plan).

The plan doc itself should be committed to `plan/assembler-debugger-integration-plan.md`
on `main` before Unit 1 starts (mirrors how `plan/assembler-plan.md` and
`plan/debugger-terminal-preferences-plan.md` were committed ahead of their
unit branches), so each unit's implementing session can read it cold.

This plan was drafted after reading the relevant code directly (`memory.rs`,
`watchpoints.rs`, `breakpoints.rs`, `menu.rs`, `panelRegistry.tsx`,
`symbol.rs`, `scanner.rs`, `TerminalPanel.tsx`/`EditMenuContext.tsx`) and
validated by a dedicated planning pass that cross-checked every claim
against the actual source (see "Verified findings" below) — this is not a
first-pass sketch.

## Decisions locked in (do not revisit mid-implementation)

- **Symbol merge is additive-only.** `assemble_and_load` calls
  `bus.symbol_table_mut().insert_from(&program.symbols)` with **no
  preceding `clear()`** — unlike `load_memory`'s clear-then-insert VICE-label
  flow (`memory.rs:147-151`). This deliberately preserves ROM-loaded labels
  across an assemble.
  **Known, accepted limitation** (verified in `src/emulator/bus/symbol.rs`):
  `SymbolTable::insert` pushes a *new* entry and repoints `by_name`, but
  never evicts the old address's `by_address` entry when a name's address
  changes. So editing source, moving a label, and re-assembling leaves a
  stale `names_for(old_address)` entry — a ghost/duplicate label in the
  Memory/Disassembly symbol gutter. Do **not** "fix" this now (a `clear()`
  would defeat the whole point of additive merge). Document it as a known
  limitation with a named deferred mitigation: track symbol names
  contributed by this panel's last successful assemble in `AssemblerState`,
  `remove()` them before merging the next assemble's symbols.
- **No bulk `Bus` write helper.** `Bus::find_region_index`
  (`src/emulator/bus/mod.rs:267`) is an O(1) precomputed 64K-entry address
  lookup table, so a per-byte `bus.patch()` loop — identical to
  `memory.rs::fill_memory`'s existing pattern — is cheap even for a full
  64KB `.res` segment. This closes out `plan/assembler-plan.md`'s old hedge
  ("a bulk helper would be small and additive if it turns out to be worth
  having") with "not worth having" — do not add one.
- **Failure-channel split.** `Err(String)` from `assemble_and_load` is
  reserved for infrastructure failure only ("CPU not ready"). A bad
  assembly source is never a command-level `Err` — always
  `Ok(AssembleReport{ success: false, diagnostics, .. })`, mirroring
  `WatchState`'s `compile_error`-inside-a-success-payload precedent
  (`watchpoints.rs`), not `load_memory`'s plain-`Err` style.
- **Syntax highlighting is out of scope for this plan, but the editor unit
  must not foreclose it.** No built-in CodeMirror 6 language exists for
  6502/65C02 mnemonics; a custom `StreamLanguage` is real, separate work.
  Per explicit instruction: Unit 2 should structure its CodeMirror
  extensions as a plain composable array/list (not a single opaque
  preset), so a `StreamLanguage`-based highlighter can be added later as
  one more extension with no rework of the mount/lifecycle code. Note the
  future extension point in a code comment at the extensions array; do not
  build the highlighter itself.
- **No auto-persisted/profile-scoped default filename.** Source files are
  user-managed documents via explicit Open/Save/Save As — closer to Memory
  panel's arbitrary-file model than Watchpoints'/Breakpoints'
  profile-config model. "Remember last-opened file per profile" is a named
  future/stretch goal, not built now.
- **Dedicated native `Assembler` menu**, mirroring the Memory-menu
  precedent (issue #411, `menu.rs`'s `Memory` submenu) rather than in-panel
  toolbar buttons — consistent with this app's established native-menu-first
  direction (per explicit instruction this session).
  Accelerators use the **Alt** modifier rather than `CmdOrCtrl+Shift`,
  deliberately distinguishing the Assembler menu's shortcuts from Memory's
  (per explicit instruction this session) — confirmed no `Alt+`-modified
  accelerator exists anywhere in `menu.rs` today (all existing bindings are
  `CmdOrCtrl(+Shift)` combos or bare F-keys), so this is a fresh, collision-free
  namespace: `Alt+O` (Open…), `Alt+S` (Save), `Alt+N` (New), no accelerator
  on Save As… (matches this app's existing pattern for secondary items,
  e.g. Restore Layout…), and `F9` (Assemble & Load) — free, and extends the
  Run menu's F5/F10/F11 "execution control" vocabulary.
- **CodeMirror 6, scoped packages only — no `basicSetup` meta-package.**
  `@codemirror/state`, `@codemirror/view`, `@codemirror/commands`,
  `@codemirror/lint`. This matches the codebase's existing style of hand-
  composing library config rather than reaching for an all-in-one preset
  (c.f. `TerminalPanel.tsx`'s hand-built `xterm.Terminal` options).

## Work Units

### Unit 1 — Backend: `assemble_and_load` Tauri command

New `debugger/src-tauri/src/assembler.rs`; register in `lib.rs`'s
`invoke_handler`.

```rust
#[derive(Clone, serde::Serialize)]
pub struct AssembleDiagnostic { pub line: usize, pub column: usize, pub message: String }

#[derive(Clone, serde::Serialize)]
pub struct SegmentSummary { pub origin: u16, pub length: usize }

#[derive(Clone, serde::Serialize)]
pub struct AssembleReport {
    pub success: bool,
    pub diagnostics: Vec<AssembleDiagnostic>,
    pub segments: Vec<SegmentSummary>,
    pub symbol_count: usize,
}

/// Free function, unit-testable without a Tauri runtime.
fn assemble_and_patch(source: &str, cpu: &mut Cpu) -> AssembleReport { .. }

#[tauri::command]
pub fn assemble_and_load(source: String, cpu_state: State<CpuState>, app: AppHandle) -> Result<AssembleReport, String> { .. }
```

Field names stay snake_case (no `#[serde(rename_all = "camelCase")]`
anywhere in this crate today — `WatchpointsSnapshot`/`WatchpointRow` are the
precedent; match it).

- `assemble_and_patch` calls `emma65::assembler::assemble(source)`.
  - `Err(errors)` → map each `Error` via its existing `line()`/`column()`/
    `message()` accessors into `AssembleDiagnostic`; `success: false`;
    **zero bus writes**, even for a source with multiple `.org` segments
    where only a later one fails.
  - `Ok(program)` → for each segment, loop
    `bus.patch(segment.origin.wrapping_add(i as u16), byte)` per byte
    (mirrors `fill_memory`'s existing loop, see decision above on why no
    bulk helper is needed); then
    `bus.symbol_table_mut().insert_from(&program.symbols)` (no `clear()`).
    `SymbolTable` today exposes no count/len accessor — check before
    writing tests whether one needs adding (a small
    `pub fn len(&self) -> usize` on `SymbolTable`, `src/emulator/bus/symbol.rs`)
    to populate `symbol_count` cheaply, or whether iterating
    `program.symbols`' existing public surface is enough.
- `assemble_and_load` (thin wrapper): lock `CpuState`,
  `guard.as_mut().ok_or("CPU not ready")?` (same as `write_memory`/
  `fill_memory` — already rejects calls made while the CPU is
  free-running, since `exec_run_from` takes ownership out of `CpuState` for
  the run's duration, no new state needed), call `assemble_and_patch`,
  capture `pc`, drop the guard, then unconditionally
  `app.emit("debugger-halted", pc).ok(); app.emit("memory-modified", ()).ok();`
  — even on `success: false`, matching `fill_memory`'s "emit regardless"
  style (a no-op refresh is harmless).
- **Not `async`** — no file I/O in this command, unlike `load_memory`/
  `save_memory` (matches `write_memory`/`fill_memory`'s plain shape).
- Tests, free-function against a `make_cpu()` helper (mirroring
  `memory.rs`/`breakpoints.rs`'s test style):
  - good source → correct segments/symbol_count, bytes actually landed
    (`peek_range` check).
  - bad source → `success:false`, correct `line`/`column`/`message`,
    **and a full memory snapshot before/after shows zero writes**.
  - a pre-existing symbol in the bus's `SymbolTable` survives an assemble
    that defines unrelated new symbols (additive merge, no `clear()`).
  - multiple `.org` segments all land at their respective origins.
- `cargo test`, `cargo clippy --all-targets` clean.

### Unit 2 — Frontend: CodeMirror editor skeleton, panel registration

New `debugger/frontend/src/AssemblerPanel.tsx`; `package.json` (new
`@codemirror/*` deps); `layout/panelRegistry.tsx` (`MainPanelId` add
`"assembler"`, `PANEL_TITLES`, `panelComponents`); `menu.rs` (`view_panels`
array +1 entry `("assembler", "Assembler")`, bump the array-length
annotation).

- Imperative mount, matching `TerminalPanel.tsx`'s `useRef<HTMLDivElement>`
  + `useEffect` xterm-mount pattern (no third-party React wrapper is used
  anywhere else in this codebase, don't introduce one here).
  `EditorState.create({ doc, extensions: [...] })` with extensions as a
  plain array (see the syntax-highlighting groundwork decision above —
  leave a comment marking where a future `StreamLanguage` extension would
  slot in); mount `new EditorView({ state, parent: containerRef.current })`;
  keep the `EditorView` in a ref; `view.destroy()` on cleanup.
- Extensions for this unit: `lineNumbers()`, `history()`,
  `keymap.of([...defaultKeymap, ...historyKeymap])`. No lint extension yet
  (Unit 3). In-memory buffer only — no `invoke` calls yet.
- **Required in this unit, not deferred**: register an `EditMenuContext`
  override via `useEditMenuOverride`/`registerOverride`, exactly like
  `TerminalPanel.tsx` does for xterm (`canCut`/`canCopy` from
  `!view.state.selection.main.empty`, `canPaste: true`, `cut`/`copy`
  through the clipboard-manager plugin + `view.dispatch` to replace the
  selection, `paste` reading clipboard text and dispatching an insert at
  the cursor). Without this, the native Edit menu's Cut/Copy/Paste
  silently no-ops while focus is in the editor — `EditMenuContext.tsx`'s
  generic fallback only recognizes `<input>`/`<textarea>`, not
  CodeMirror's contenteditable surface.
- UAT (no frontend test framework exists in this repo — confirmed no
  vitest/jest/test script in `package.json`; verified manually per
  established precedent): panel appears via View > Assembler and the dock;
  typing/line numbers/scrolling work; Ctrl+Z/Ctrl+Y undo/redo; Cut/Copy/
  Paste via the native Edit menu while focused in the editor; panel
  survives dock/undock and app restart (persisted layout) with an empty
  buffer (content persistence is explicitly out of scope for this unit).
- `npm run build` (tsc + vite) clean; `cargo build`/`cargo clippy` clean.

### Unit 3 — Wire "Assemble & Load", diagnostics, panel refresh

Extends `AssemblerPanel.tsx` only.

- On-demand trigger only (button + `F9`/menu, wired in Unit 4) — **not** a
  live-as-you-type `linter()` source — since assembling has a real side
  effect (writing memory), it should never fire implicitly on a debounce
  timer the way pure-syntax linting normally does. Add `@codemirror/lint`'s
  `lintGutter()` extension; apply diagnostics via
  `setDiagnostics(view.state, diagnostics)` after each `assemble_and_load`
  call returns.
- **Diagnostic position mapping — must match the Rust scanner exactly.**
  `AssembleDiagnostic.line`/`.column` are 1-based, and `column` is
  *tab-expanded*: `src/assembler/scanner.rs` adds `TAB_SIZE = 8` per `\t`
  and `1` per any other character (confirmed at `scanner.rs:5,51,55`).
  CodeMirror positions are 0-based absolute character offsets. Write
  `lineColToOffset(doc: Text, line: number, column: number): number`:
  clamp `line` to `[1, doc.lines]`, walk `doc.line(line).text` from the
  start incrementing a counter by 8 per `\t` / 1 per other character until
  it reaches `column`, return `lineObj.from + charIndex` clamped to
  `lineObj.to`. A naive `doc.line(n).from + (column - 1)` is **wrong** on
  any line with a tab before the error column — this needs a real regression
  check (see Verification below), not just code review.
- Also render a plain-text error list below the editor, mirroring
  `WatchpointPanel.tsx`'s `compile_error` banner — one line per diagnostic,
  `line:column: message` — for accessibility/consistency alongside the
  gutter markers.
- On success: a summary line (`N bytes across M segments, K symbols`) from
  `AssembleReport.segments`/`symbol_count`.
- UAT: assemble valid source, confirm Memory/Disassembly/Stack panels
  (each with their own `listen("debugger-halted"/"memory-modified")`
  handler) and Register panel (via `ExecutionContext.tsx`'s central
  listener) all refresh — zero new listeners needed anywhere outside
  `AssemblerPanel.tsx`, since both are already-existing events. Assemble
  source with a tab before an error column; confirm the marker lands on
  the correct character. Assemble bad source; confirm zero bus writes
  (Memory panel unchanged) and correct diagnostics/list rendering.

### Unit 4 — File Open/Save/Save As, native `Assembler` menu

New async commands in `assembler.rs` (`read_source_file`,
`write_source_file` via `tokio::fs::read_to_string`/`write`, same shape as
`load_memory`/`save_memory` — no new capability needed, no
`@tauri-apps/plugin-fs` dependency); `menu.rs` (`AssemblerMenuState`,
`NEW_ASSEMBLER_ID`/`OPEN_ASSEMBLER_ID`/`SAVE_ASSEMBLER_ID`/
`SAVE_AS_ASSEMBLER_ID`/`ASSEMBLE_LOAD_ID`, new `Assembler` `Submenu`);
`lib.rs` (`on_menu_event` branch emitting `assembler-menu-action`,
"assembler" reveal-panel case, `.manage(assembler_menu_state)`, command
registration); `AssemblerPanel.tsx` (listen for `assembler-menu-action`,
open/save dialogs via `@tauri-apps/plugin-dialog` + existing
`get_last_file_dialog_dir`/`set_last_file_dialog_dir` prefs commands,
current-file-path + dirty-flag state, Save vs. Save As distinction).

- Follow `MemoryPanel.tsx`'s file-dialog pattern exactly (filters e.g.
  `{name: "Assembly Source", extensions: ["s", "asm", "a65"]}`,
  `defaultPath` from `get_last_file_dialog_dir`, `set_last_file_dialog_dir`
  on pick).
- Dirty-tracking via CodeMirror's `updateListener` (`update.docChanged`);
  "Save" with no current path behaves like "Save As…" (prompts).
  Unsaved-changes indicator in the tab title is discretionary — cut if
  scope needs to shrink, call it out either way in the PR description.
- Menu: New (`Alt+N`), Open… (`Alt+O`), Save (`Alt+S`), Save As… (no
  accelerator), separator, Assemble & Load (`F9`) — `F9` works from both
  the menu and the in-panel action from Unit 3, funneled through one
  shared handler (this app's established "menu click and panel control
  share one handler" convention).
- "New" clears the buffer (dirty-check confirm if unsaved changes exist)
  and resets the current-file-path — included per explicit instruction
  this session, not discretionary.
- Tests: check `memory.rs`'s test module for an existing async
  read/write-round-trip test precedent before writing new ones for
  `read_source_file`/`write_source_file` from scratch; mirror it if found.
- UAT: New with unsaved changes prompts before clearing; Open a `.s`/`.asm`
  file, edit, Save (in place), Save As… (new path, confirm original
  untouched), Assemble & Load from the menu, confirm `Alt+N`/`Alt+O`/
  `Alt+S`/`F9` don't collide with any existing shortcut (spot-check
  Memory's `Ctrl+L`/`Ctrl+S`, Run's `F5`/`F10`/`F11` still work
  unaffected).

## Verified findings behind key decisions (traceable, not assumed)

- `Bus::find_region_index` (`src/emulator/bus/mod.rs:267`) is an O(1)
  64K-entry lookup table → per-byte `bus.patch()` looping is cheap at any
  segment size.
- `SymbolTable::insert` (`src/emulator/bus/symbol.rs:33-40`) never evicts
  an old `by_address` entry when a name's address changes → confirms the
  additive-merge ghost-symbol limitation above.
- `src/assembler/scanner.rs:5,51,55`: `TAB_SIZE = 8`, columns start at `1`,
  tabs add 8 not 1 → confirms the diagnostic position-mapping requirement
  in Unit 3.
- `TerminalPanel.tsx:11,366,476` (`useEditMenuOverride`/`registerOverride`)
  and `EditMenuContext.tsx`'s `isPlainEditable` (only recognizes
  `<input>`/`<textarea>`) → confirms Unit 2 needs its own override, not a
  free ride from the existing fallback.
- `debugger/frontend/package.json` has no CodeMirror/Monaco/editor
  dependency and no test framework (no vitest/jest/test script) today —
  confirmed by direct read, not assumed.
- `menu.rs`'s existing accelerators (`Ctrl+N/O/Q`, `Ctrl+L`, `Ctrl+S`,
  `Ctrl+Shift+E/F`, `Ctrl+Shift+T`, `F5/F10/F11` + Shift variants) →
  informs the collision-free accelerator choices above.

## Workflow

Four sequential units, one branch + PR each, following
`feedback_issue_462_workflow` (also used for #467 and #474's library
plan): create a branch named for the unit, implement, run the
verification below, commit, push, open a PR describing any manual UAT the
reviewer still needs to do, then **stop and await explicit instruction**
before merging or starting the next unit. Never batch units into one PR;
never auto-merge; never start the next unit unprompted.

## Verification (per unit, before opening its PR)

- `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings` clean (Units 1
  and 4 add real Rust code; Units 2–3 still touch `menu.rs`'s array, so
  build/clippy still apply every unit).
- `npm run build` (tsc + vite) clean after every unit that touches the
  frontend (2–4).
- No frontend automated tests exist in this repo — Units 2–4 rely on the
  manual UAT checklists above, run by the human reviewer post-PR, not
  claimed as done by the implementing agent.
- Regression check to re-run at the end of Unit 3 and again after Unit 4:
  assemble source with a tab character before a deliberately-triggered
  error and confirm the diagnostic lands on the correct character.

## Key files

- `debugger/src-tauri/src/assembler.rs` (new)
- `debugger/src-tauri/src/lib.rs`
- `debugger/src-tauri/src/menu.rs`
- `debugger/frontend/src/AssemblerPanel.tsx` (new)
- `debugger/frontend/src/layout/panelRegistry.tsx`
- `debugger/frontend/package.json`
- Reference/reused, not modified: `debugger/src-tauri/src/memory.rs`,
  `watchpoints.rs`; `debugger/frontend/src/TerminalPanel.tsx`,
  `EditMenuContext.tsx`, `WatchpointPanel.tsx`, `MemoryPanel.tsx`;
  `src/emulator/bus/symbol.rs`; `src/assembler/mod.rs`,
  `src/assembler/scanner.rs`.
