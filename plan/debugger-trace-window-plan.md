# Live trace window for the debugger UI

## Context

The recently overhauled trace facility (`Cpu::set_trace_callback`, binary trace
file format v2, and the new `emma65-tracer` CLI) currently only supports
offline analysis: record to a file during a run, then decode it afterward
with `emma65-tracer`. This plan brings that same trace data into the debugger
UI as a live, scrollable trace window, updated as the user steps or runs the
CPU — without holding the whole trace in the DOM or in memory.

Design decisions already made in discussion, which this plan builds on:
- The window opens via a new native menu bar (`File`/`Edit`/`Window`/`Help`),
  fixing an existing discoverability gap where Terminal has a shortcut but no
  visible control. `Window` gets checkable `Terminal`/`Trace` items.
- `Trace` also gets its own shortcut (Ctrl+Shift+T), mirroring Terminal's
  Ctrl+Shift+`.
- Trace events spool to a file (reusing the existing writer-thread machinery)
  rather than an in-memory buffer, so memory stays flat during long runs.
- The trace log is a fixed-viewport, uniform-row-height scroller — the same
  "re-fetch a window, replace the DOM" pattern `MemoryPanel.tsx` already uses
  for memory — not a growing/virtualized list. Expanded bus-op detail for the
  selected row renders in a separate fixed-height detail pane, not inline, so
  there's no "expand all" control and no variable row heights to lay out.
- Record opens a save dialog; Stop detaches the writer (next Record needs a
  new file); Pause is frontend-only (stops tail-following, file keeps
  writing); un-pause resumes following the live tail.
- The trace panel refreshes on the `debugger-halted` event (already emitted
  after every `step_into`) and `debugger-run-stopped` (already emitted once,
  on completion, by `run_cpu`/`step_over`/`step_return`) — both events already
  exist with exactly the cadence this feature needs, so no changes to the
  exec commands or the hot execution path are required.

## Library changes (`src/emulator`)

**`src/emulator/disasm/trace.rs`** — add a `TraceRowAssembler` that wraps the
existing `TraceDisassembler` and also tracks per-instruction bus ops and
cycle count, producing one `TraceRow` per completed instruction:

```rust
pub enum TraceBusOp { Read { addr: u16, value: u8 }, Write { addr: u16, value: u8 } }
pub struct TraceRow {
    pub instr_id: u64,
    pub regs: Registers,
    pub cycles: Option<u8>,
    pub line: DisassembledLine,
    pub bus_ops: Vec<TraceBusOp>,
}
pub struct TraceRowAssembler { /* wraps TraceDisassembler + pending bookkeeping */ }
impl TraceRowAssembler {
    pub fn new(variant: CpuVariant, symbols: SymbolTable) -> Self;
    pub fn feed(&mut self, rec: &TraceRecord) -> Option<TraceRow>;
}
```

This is a straight extraction of logic that currently exists, duplicated,
as the private `Pending`/`BusOp`/emit-grouping code in
`src/bin/tracer/main.rs:32–52,111–172`. Once extracted:
- Refactor `src/bin/tracer/main.rs`'s `run()` to use `TraceRowAssembler::feed`
  instead of its own `Pending` bookkeeping (keep `format.rs`'s text rendering
  as-is — only the grouping logic moves).
- The debugger's windowed-read command (below) uses the same assembler, so
  both consumers share one implementation of "group trace records into a row."

**`src/emulator/cpu/trace.rs`** — add seekable random access to
`BinaryTraceReader`, needed for jumping to an arbitrary row without scanning
the file from the start:

```rust
impl<R: Read + Seek> BinaryTraceReader<R> {
    pub fn seek_to_record(&mut self, record_index: u64) -> io::Result<()>;
}
```

Computes the byte offset from the existing private `HEADER_LEN`/`RECORD_LEN`
constants and seeks the inner `BufReader`. No wire-format change, no version
bump.

**`src/emulator/mod.rs`** — re-export `TraceRowAssembler`, `TraceRow`,
`TraceBusOp` (and `TraceDisassembler`, not currently re-exported at the top
level) alongside the existing `pub use cpu::trace::{...}` line, so the
debugger crate can use them directly.

Tests: unit tests for `TraceRowAssembler::feed` (same shape as the existing
`TraceDisassembler` tests in the same file) and for `seek_to_record`
round-tripping against a `Vec<u8>` cursor.

## Debugger backend (`debugger/src-tauri/src`)

**New `trace.rs` module**, following the existing panel-module pattern
(`disassembly.rs`, `memory.rs`):

- `TraceState` (managed, `Mutex`-wrapped): `path: Option<PathBuf>`,
  `row_index: Arc<Mutex<Vec<u64>>>` (record ordinal where each row's
  `Registers` record begins — grows as recording proceeds), `recording: bool`,
  `writer_handle: Option<JoinHandle<()>>`.
- A small local `RowIndexingTraceCallback` implementing `TraceCallback`,
  wrapping the `ChannelTraceCallback` from `spawn_trace_writer` exactly the
  way `DisassemblingTraceCallback` already wraps a callback in the library —
  it counts records and pushes the running count onto `row_index` whenever it
  sees a `TraceKind::Registers`, then forwards to the inner callback
  unchanged. (Kept debugger-local: it's UI scrubber bookkeeping, not a
  general library concern.)
- `record_trace(path: String, cpu_state, trace_state) -> Result<(), String>`:
  locks `CpuState` (same pattern as `step_into`/`reset_cpu`), creates the
  file, builds `BinaryTraceWriter::new(file, cpu.variant())`,
  `spawn_trace_writer(writer, capacity, OverflowPolicy::BlockOnFull)` —
  `BlockOnFull` because a debugger trace that silently drops instructions
  would be misleading, unlike the CLI capture use case `DropOnFull` was
  chosen for. Wraps the returned callback in `RowIndexingTraceCallback`,
  calls `cpu.set_trace_callback(Some(...))`, and populates `TraceState`.
- `stop_trace(cpu_state, trace_state) -> Result<(), String>`: calls
  `cpu.set_trace_callback(None)`, which drops the channel sender and lets the
  writer thread's existing drain-then-flush-then-exit loop (unmodified, in
  `spawn_trace_writer`) run to completion; `.join()` the handle so the file is
  guaranteed flushed before the command returns. Clears `recording` but keeps
  `path`/`row_index` so the just-recorded file stays browsable.
- `get_trace_window(start_row: usize, count: usize, cpu_state, trace_state) -> Result<TraceWindowPage, String>`:
  clamps `start_row` into `[0, row_index.len())`, opens the file fresh
  (`File::open` — simplest option; an extra open/close per viewport-sized
  fetch is cheap relative to a UI-paced scroll/poll action), seeks via
  `seek_to_record(row_index[start_row])`, builds a `TraceRowAssembler` from
  `cpu.variant()` + `cpu.bus().symbol_table().clone()` (same lookup
  `resolve_symbol` already uses), and collects up to `count` rows. Returns
  `TraceWindowPage { rows: Vec<TraceRowDto>, total_rows: usize }` —
  `total_rows` lets the frontend compute "follow the tail" as
  `start_row = total_rows.saturating_sub(viewport_rows)` on each poll, so no
  separate "get tail" command is needed.
  `TraceRowDto` mirrors `DisassembledRow` (`disassembly.rs:56–72`) plus `seq`,
  `cycles`, register fields, and `bus_ops: Vec<{addr,op,value}>` for the
  detail pane.
- `get_trace_status(trace_state) -> TraceStatus { recording: bool, path: Option<String> }`:
  lets the toolbar restore Record/Stop button state on window reopen, same
  pattern as `get_session_status`.
- `toggle_trace_visibility(app) -> Result<(), String>`: same shape as
  `terminal::toggle_terminal_visibility`, for the Ctrl+Shift+T shortcut;
  delegates to the shared helper in `menu.rs` (below) so the menu checkbox
  stays in sync regardless of which path toggled the window.

Record/Stop/Pause should be disabled in the frontend while the CPU isn't
halted (`execState !== "stopped"`), matching how other panels already gate
controls — `record_trace`/`stop_trace` will simply find `CpuState` empty
(`None`) during a free-run, same as any other command would.

**New `menu.rs` module**: builds the native app menu (`tauri::menu`) —
`File` (Quit, via `PredefinedMenuItem::quit()` wired to the existing `quit`
command), `Edit` (no items yet — a placeholder title), `Window`
(`CheckMenuItem` "Terminal" and "Trace", checked state initialized from each
window's `is_visible()`), `Help` (`PredefinedMenuItem::about()`). Exposes a
shared `fn toggle_window_visibility(app, label, check_item)` used by: the
menu item's own click handler, `toggle_terminal_visibility`, and
`toggle_trace_visibility` — one place that both shows/hides the window and
updates its checkbox, so all three trigger paths (menu click, shortcut,
native close button) stay consistent.

**`lib.rs`**: register the new `trace::*` commands and `menu`-built app menu
in `.setup()`; extend the existing close-to-hide `on_window_event` block
(`lib.rs:199–226`, currently Terminal-only, including the GTK
decoration-hit-test workaround) to also cover the `trace` window — both now
have identical lifecycle needs, so this is a good point to factor the
per-window listener into a small shared helper rather than duplicating the
block; wire it to also call the checkbox-sync helper from `menu.rs` on
hide/show.

**`tauri.conf.json`**: add a `trace` window entry mirroring the existing
`terminal` entry (own `url: "trace.html"`, `visible: false`, sized for a
columnar log + detail pane, e.g. 1100×700).

## Frontend (`debugger/frontend/src`)

- `trace.html` + `src/trace.tsx`: new window entry point, mirroring
  `terminal.tsx`.
- `src/TracePanel.tsx`:
  - Toolbar: Record/Stop/Pause buttons (styled like the app's other icon
    buttons, using the existing `@vscode/codicons` dependency). Record calls
    `save()` from `@tauri-apps/plugin-dialog` (same plugin already used for
    `open()` in `MemoryPanel.tsx`'s load-file flow) then `record_trace`;
    Stop calls `stop_trace`; Pause just flips a local boolean.
  - Windowed log: fixed number of uniform-height rows sized to the viewport,
    fetched via `get_trace_window` and replaced wholesale on
    scroll/keyboard/wheel — the same `fetchPage`-and-replace pattern
    `MemoryPanel.tsx` uses for memory, not a virtualization library.
  - Live-follow: while recording and not paused, re-fetch the tail window
    (`start_row = total_rows - viewportRows`) on `debugger-halted` and
    `debugger-run-stopped` events (`listen()`, same as `App.tsx` already does
    for `debugger-running-tick`). Un-pause immediately re-fetches the tail.
  - Row selection sets `selectedRow`; its `bus_ops` render in a separate
    fixed-height detail pane below/beside the log (not inline expansion).
  - On mount, calls `get_trace_status` to restore toolbar state (mirrors
    `App.tsx`'s `get_session_status` restore-on-load pattern).
- `useAppKeyBindings.ts`: add
  `{ matches: (e) => e.ctrlKey && e.shiftKey && e.code === "KeyT", command: "toggle_trace_visibility" }`
  alongside the existing Terminal binding.
- `src/styles/trace.scss`: new stylesheet for the toolbar/log/detail-pane
  layout.

## Verification

- `cargo test --workspace` — new `TraceRowAssembler`/`seek_to_record` unit
  tests, plus confirms the `emma65-tracer` refactor didn't change its
  behavior (existing tracer tests, if any, must still pass unchanged).
- `cargo clippy` (workspace root covers the debugger crate too).
- Manual UAT in the running debugger (per established practice, this is a
  user-driven check, not something to automate): open the Window menu,
  confirm Terminal/Trace both toggle with a checkmark that reflects actual
  visibility (including via the native close button and the Ctrl+Shift+T /
  Ctrl+Shift+` shortcuts); Record → file dialog → pick a path; single-step
  and confirm the trace window updates every instruction; Run and confirm it
  only updates once, on stop; select a row and confirm its bus ops appear in
  the detail pane; Pause, confirm the display freezes while stepping
  continues; un-pause, confirm it jumps back to the live tail; Stop, confirm
  Record requires picking a new file afterward; scroll back to the start of a
  long trace and confirm the windowed fetch stays responsive.

## Process note

Per this repo's established workflow, implementation will proceed on a
single dedicated branch with one commit per unit (library extraction →
backend recording/window commands → menu bar → frontend panel), rather than
one branch per unit.
