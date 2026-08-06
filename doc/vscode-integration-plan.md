VS Code Integration Spike Plan
===============================

This plan scopes a speculative sprint (branch `spike/vscode-integration`,
isolated from `main`) exploring whether emma65's debugger can be reasonably
re-hosted inside Visual Studio Code, instead of (or alongside) the existing
Tauri app.

The goal of the spike is to demonstrate **most** of the existing debugger's
capabilities through VS Code's own debugging UI (via the Debug Adapter
Protocol, DAP) and/or a small number of custom webviews, so the team can
judge whether the direction is worth pursuing further. Source-level
debugging (mapping VICE labels back to assembly source, build-task
integration, `launch.json` project conventions) is explicitly **out of
scope** for this spike — see "Deferred" below — pending a decision to
continue past the spike.

Every story below is scoped for a single commit or small commit sequence.
Each commit must touch exactly one of the three areas below — never mix
them in one commit:

- **Core library** (`src/emulator/**`) — expected to be rare. The library
  already treats the Tauri debugger as just one client of its public API
  (`exec`, `bus::symbol`, `Transport`, `disasm`, `watch`), so most stories
  should need nothing here. Flag any core change explicitly in the PR/commit
  message, since these are the ones worth evaluating for a backport to
  `main` independent of whether the spike itself proceeds.
- **`vscode/adapter`** (new Rust crate, new workspace member) — a DAP
  server binary, structurally analogous to `debugger/src-tauri`: depends on
  `emma65` as a path dependency, owns an `EmulatorSession`, and translates
  DAP requests/events (plus a small set of custom requests, see below) to
  and from the emulator's existing `exec`/`bus`/`disasm`/`watch` APIs.
- **`vscode/extension`** (new TypeScript project) — the VS Code extension
  itself: registers the debug adapter, contributes commands/menus, hosts
  the terminal `Pseudoterminal`, and hosts the trace/watchpoint webviews.

Project Structure
------------------

- **Workspace layout:** `vscode/adapter` (Rust, added to the Cargo
  workspace) and `vscode/extension` (TypeScript, `package.json`-based, not
  part of the Cargo workspace — mirrors the `debugger/src-tauri` /
  `debugger/frontend` split).
- **Transport between adapter and extension:** DAP over stdio (the
  adapter is spawned as a child process by the extension), the standard
  VS Code debug adapter integration pattern. Custom, emma65-specific
  affordances (NMI/IRQ, trace window, watchpoints) ride as DAP custom
  requests/events over the same stdio connection rather than opening a
  second channel.
- **CPU thread model:** unchanged from the Tauri debugger — a dedicated
  OS thread runs the CPU loop inside `vscode/adapter`; the adapter's async
  runtime handles DAP I/O and event emission, exactly as `debugger/src-tauri`
  already separates CPU execution from Tauri's IPC.

### Development & UAT Workflow

`cargo tauri dev` today gives one-command UAT with hot-reloading frontend
and fast Rust rebuild-and-relaunch. There is no single equivalent for the
VS Code split — the loop differs by which of the three areas changed, and
one of them (the adapter) has no hot reload at all:

| Area | UAT loop |
|------|----------|
| `vscode/extension` chrome (commands, menus, activation) | Open `vscode/extension` as its own VS Code window and press `F5`. This launches a second, separate window — the *Extension Development Host* — with the extension loaded as a real user would see it, plus a debugger attached from the first window (breakpoints in the extension's TS code hit there). Contributed commands show up in that second window's Command Palette; interact with them like a user would. With `tsc --watch` running, `Ctrl+R` in the second window ("Developer: Reload Window") picks up new compiled output without a full F5 restart. |
| `vscode/extension` webviews (trace, watchpoints) | Same pattern `debugger/frontend` already uses: point the webview's HTML at a local Vite dev server when a dev flag is set, so edits hot-reload inside the webview panel with no window reload needed. |
| `vscode/adapter` (Rust DAP server) | No hot reload. `cargo build -p emma65-vscode-adapter`, then stop/restart the debug *session* (not the whole Extension Development Host) — VS Code respawns the adapter process fresh each time. |
| Core `src/emulator` changes | Same rebuild-and-restart as adapter changes, since `vscode/adapter` depends on it as a path dep — no separate step. |
| Terminal (`Pseudoterminal`) | Exercised directly in the Extension Development Host's own integrated terminal tab — simpler than today, since there's no separate terminal window to show/hide. |

The real gap vs. today: DAP-facing changes (stepping, breakpoints,
registers, memory — most of stories 2–9) require a `cargo build` plus a
debug-session restart every time, where `cargo tauri dev` gave near-instant
Rust iteration. Set up `vscode/extension/.vscode/launch.json` with a
`cargo build` pre-launch task as part of story 1, so the loop is at least
"save → F5 → done" from day one.

---

Stories
-------

### 1. Spike Branch and Workspace Scaffold

Establish the branch and the two new crates/projects with no behavior yet,
so every subsequent story is additive.

**Scope:**
- `vscode/adapter`: new Cargo workspace member, `emma65` path dependency,
  `main.rs` that reads a config path (reusing `emulator::config::Config`)
  and exits — no DAP wiring yet.
- `vscode/extension`: minimal VS Code extension skeleton (`package.json`,
  `extension.ts` with a no-op `activate`), no debug adapter registration
  yet.
- This plan doc itself, committed as the first commit on the branch.

**Acceptance criteria:**
- `cargo build --workspace` succeeds with the new member present.
- The extension loads in the VS Code Extension Development Host with no
  errors (`F5` from `vscode/extension`).

---

### 2. DAP Session Lifecycle

**Scope:**
- `initialize`/`launch` handling in `vscode/adapter`: builds an
  `EmulatorSession` from a config path passed via `launch.json`, falling
  back to the built-in default config (same fallback `main.rs` uses today)
  if none is given.
- CPU starts halted at the reset vector (mirrors the Tauri debugger's
  halted-start behavior), emits DAP's `stopped` event with reason
  `"entry"`.
- `configurationDone`, `disconnect`, `terminate` tear the session down
  cleanly.

**Acceptance criteria:**
- Launching the debug session from VS Code's Run and Debug view halts at
  the reset vector and VS Code shows a stopped session (no panels populated
  yet — that's stories 4–7).

---

### 3. Execution Control via DAP

**Scope:**
- `continue`, `pause`, `next` (step over), `stepIn` (step into), `stepOut`
  (step return), and `restart` (reset), wired to the equivalent `exec`
  operations (`run`/`RunStopper`, `step_over_subroutine`, `step_into`,
  `step_return`, and CPU reset).
- `stopped`/`continued` events on every transition, matching the
  `debugger-halted`/`debugger-run-stopped` event pairs the Tauri debugger
  already emits at the equivalent points.

**Acceptance criteria:**
- VS Code's standard debug toolbar (Continue/Pause/Step Over/Step
  Into/Step Out/Restart) fully drives the emulator with no custom UI.

---

### 4. Disassembly View Integration

**Scope:**
- `disassemble` request backed by `emulator::disasm::Disassembler`,
  returning `DisassembledInstruction`s around a given memory reference.
- On `stopped`, ensure VS Code's built-in Disassembly View (opened via
  "Open Disassembly View" or auto-shown when no source is mapped) is the
  view VS Code lands on, PC-highlighted.

**Acceptance criteria:**
- Stepping in the Disassembly View shows the same instruction stream the
  Tauri debugger's center column shows today.

---

### 5. Instruction Breakpoints

**Scope:**
- `setInstructionBreakpoints`, wired to the existing breakpoint
  add/remove/enable/disable operations (currently `disassembly.rs`'s
  `toggle_breakpoint`/`set_breakpoint`/`remove_breakpoint`/
  `disable_breakpoint`/`enable_breakpoint`).

**Acceptance criteria:**
- Clicking the gutter in the Disassembly View sets/clears a breakpoint;
  `continue` halts there, matching the Tauri debugger's breakpoint
  behavior.

---

### 6. Registers as a Variables Scope

**Scope:**
- `scopes`/`variables` expose a "CPU Registers" scope: A, X, Y, PC, S, P
  (with the 8-character flag string, as the Tauri register view shows).
- `setVariable` writes an edited register back, matching `set_register`'s
  existing "only while halted" constraint.

**Acceptance criteria:**
- The Variables view shows and allows editing registers in place, with
  changed values highlighted after a step (VS Code does this automatically
  for changed variables).

---

### 7. Memory Inspector Integration

**Scope:**
- `readMemory`/`writeMemory` backed by `Bus::peek_range`/`Bus::write`,
  covering both the Tauri debugger's Memory panel and Stack panel (the
  stack is just a memory range starting at `0x0100`).
- Wire a "View Binary Data" action on the register scope (or a fixed
  memory reference) so the built-in Memory Inspector is reachable without
  a custom command.

**Acceptance criteria:**
- The Memory Inspector opens, reads, and edits memory live, matching
  `get_memory`/`write_memory`'s existing side-effect-free-read /
  bus-write semantics.

---

### 8. Integrated Terminal via Pseudoterminal

**Scope:**
- In `vscode/extension`, implement a `vscode.Pseudoterminal` for the
  console device's byte stream.
- In `vscode/adapter`, reuse the exact bridge shape `debugger/src-tauri`'s
  `terminal.rs` already established on top of
  `InternalPipeTransport::pair()`/`into_split()`: a read loop that forwards
  bytes out (replacing `app.emit_to("terminal-output", ...)` with a DAP
  custom event carrying bytes to the extension), and a write path
  (replacing the `write_terminal` command with a custom DAP request the
  `Pseudoterminal`'s `handleInput` sends).

**Acceptance criteria:**
- A VS Code integrated terminal tab shows console output and accepts
  keyboard input, round-tripping through the emulated console device the
  same way the Tauri terminal window does today.

---

### 9. NMI/IRQ and Bus Signal Snapshot via Custom Commands

**Scope:**
- Custom DAP requests (`emma65/triggerNmi`, `emma65/assertIrq`,
  `emma65/releaseIrq`, `emma65/getBusState`) wired to the existing
  `cpu_bus.rs` operations, since DAP has no native concept for
  interrupt lines or bus signals.
- Contributed commands + toolbar buttons in `vscode/extension`'s
  `package.json` (`contributes.commands`/`contributes.menus`) invoking
  them via the active debug session's `customRequest`.

**Acceptance criteria:**
- Command palette entries (and toolbar buttons, time permitting) trigger
  NMI, assert/release IRQ, and show the current bus signal snapshot,
  matching the Tauri debugger's CPU/Bus panel.

---

### 10. Trace Webview

**Scope:**
- Port the trace panel's windowed-read/scroll behavior into a VS Code
  webview panel in `vscode/extension`.
- `vscode/adapter` exposes `emma65/recordTrace`, `emma65/stopTrace`,
  `emma65/getTraceWindow`, `emma65/getTraceStatus` as custom requests,
  implemented directly on top of the existing `TraceRowAssembler` /
  `BinaryTraceReader` / writer-thread machinery — no core changes expected.

**Acceptance criteria:**
- The webview records, browses, and displays trace rows equivalent to the
  Tauri debugger's trace window, opened via a contributed command.

---

### 11. Watchpoints Webview

**Scope:**
- Port the watchpoint list/editor into a VS Code webview panel.
- `vscode/adapter` exposes `emma65/getWatchpoints`, `emma65/addWatchpoint`,
  `emma65/removeWatchpoint`, `emma65/editWatchpoint`,
  `emma65/toggleWatchpoint` as custom requests over the existing
  `WatchCompiler`/`WatchEvaluator` session API. The watch DSL (walrus
  assignment, arbitrary expressions) doesn't map onto DAP's data-breakpoint
  model, which is why this needs a webview rather than `setDataBreakpoints`.

**Acceptance criteria:**
- The webview shows triggered/not-triggered status per watchpoint and
  supports add/remove/edit/toggle with persistence to `watchpoints.emw`,
  matching the Tauri debugger's watchpoint panel.

---

Deferred (out of scope for this spike)
---------------------------------------

- Source-level debugging: mapping VICE labels to assembly source lines,
  source-level breakpoints, `launch.json`/build-task conventions for
  assembling a project before debugging.
- Memory load/fill file commands (`load_memory`/`fill_memory`) as
  contributed commands.
- Theme sync between VS Code's theme and the emulator UI (moot if the
  webviews inherit VS Code's own theme, which they should by default).
- Native menu bar equivalent — VS Code's own command palette and
  `contributes.menus` replace `menu.rs` entirely; no port needed.

Dependencies
------------

- Stories 2–9 depend on story 1's scaffold.
- Story 3 (execution control) is a prerequisite for exercising stories
  4–7 interactively.
- Stories 10 and 11 are independent of each other and of stories 4–9;
  either can be dropped from the spike's scope under time pressure without
  affecting the others.

GitHub Issues
-------------

Not yet created — this is a speculative spike branch, not committed
implementation work. If the spike validates the direction, convert the
stories above into issues (and merge the branch, respecting the
core/adapter/extension commit partitioning already applied) at that point.
