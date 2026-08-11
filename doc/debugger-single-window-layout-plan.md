# Single-window IDE-style layout for the debugger UI

## Context

The debugger's main window (Registers/Disassembly/Memory/Stack/Watchpoints/CpuBus) is a hand-rolled 3-column flex layout that works well — it puts a lot of useful information in front of the user at once. Trace and Log, however, are separate native OS windows the user has to manage manually, which doesn't fit modern full-screen workflows: there's no way to group them with the main window's panels, and toggling multiple windows in and out of view is friction. Terminal is a separate window too, which was originally the right call for designing full-screen 6502 target applications (the terminal needs to look and behave like a real, undecorated console), but Terminal should default to being a dockable panel like everything else, while preserving the ability to pop it back out to its own window for that kind of target-app work.

This plan moves the debugger to a VS Code-style single window with drag/resize/tab-arrangeable panels for all six existing panels plus Trace and Log, with Terminal joining them as a panel that can still be explicitly detached to a dedicated OS window on demand. It is sequenced into independently-landable phases, each its own GitHub issue, so the app is never left broken between them.

## Key decisions

**Docking library: [`dockview`](https://github.com/mathuo/dockview)**, used only for in-window docking/tabs/splits/resize. It's modeled on VS Code's own grid/group/tab system (tabbed groups, drag-to-rearrange/split, resizable splits, serializable layout via `api.toJSON()`/`fromJSON()`), and its plain id→component registration model is a near-zero-friction fit for the existing pattern where each panel (`RegisterPanel.tsx`, `DisassemblyPanel.tsx`, etc.) is already a self-contained component owning its own `invoke`/`listen` wiring. Re-verify the exact package name/version against npm at Phase 0 spike time.

**Detach-to-window: custom-built, NOT dockview's own popout feature.** Every docking library's popout is `window.open()`-based, which in Tauri 2 produces a webview with no Tauri window label — `get_webview_window`/`emit_to` can't address it, `capabilities/default.json`'s label-scoped permissions don't cover it, it doesn't get the native menu treatment, and it doesn't participate in the close-lifecycle hooks the project already relies on (`install_toggleable_window_lifecycle`, `request_exit`). The project has already shipped the right mechanism for this — `terminal.rs`'s `TERMINAL_WINDOW_LABEL` + a `tauri.conf.json`-declared window + `emit_to` targeting — Phase 6 just generalizes it to a **dynamically created** window using `WebviewWindowBuilder` instead of a statically declared one, reusing a single fixed label (`terminal-detached`) across every detach cycle. This sidesteps any need for Tauri 2's glob-pattern capability matching, since at most one detached Terminal window can exist at a time.

**Scrollback on detach: accept the loss for now, but architect for future restoration.** Detach/reattach will unmount and remount `TerminalPanel`, which drops xterm's in-memory scrollback — acceptable given the actual use case (an occasional full-screen switch, not rapid toggling). To keep the door open for a future "preserve scrollback" mode without a rework, Phases 5/6 isolate the terminal's I/O plumbing (the `emit_to` target redirection, and any future output-buffering) behind a boundary that doesn't assume a 1:1 mount↔session lifetime.

**Window menu: keep Trace/Log/Terminal in the Window menu for now**, changed from checkboxes to plain "Reveal …" actions that activate the relevant dock tab (or, for Terminal post-Phase-6, "Detach"/"Attach"). A broader reorganization (e.g. a dedicated View menu for tool visibility) is a sensible follow-up but explicitly out of scope for this plan.

## Phase sequence

Each phase leaves the app fully functional.

### Phase 0 (Spike) — De-risk dockview-in-Tauri + detach mechanics

Answers the open technical questions before later phases are scoped in detail. Prototype quality is fine; nothing here needs to meet the doc-comment/clippy/test bar unless salvaged into a real phase branch.

Questions to answer, each needing a human UAT round (an agent can't verify drag/resize/detach visually):
1. Does dockview render/theme/drag/resize correctly under WebKitGTK (the actual dev target)? Can its theme hooks be driven from the existing `--color-*` CSS custom properties in `global.scss` rather than adopting a shipped dockview theme wholesale?
2. **Critical**: does dockview keep an inactive tab's panel mounted, or unmount/remount it on tab switch? This determines whether Phase 5's `terminal_ready` handshake can even fire if Terminal's tab isn't active at startup — if dockview lazy-mounts, this spike must identify a mitigation (force eager-mount of all registered panels, or decouple session bring-up from Terminal's specific mount timing).
3. Does a `ResizeObserver`-driven `fitAddon.fit()` correctly refit xterm when a dockview split is dragged (not an OS window resize) and when a zero-size inactive tab becomes active?
4. Prototype the general detach mechanism end-to-end using a simple, low-risk panel (`StackPanel.tsx` — no event subscriptions) rather than Terminal: build a `WebviewWindowBuilder` window with a fixed reused label, mount the same component in a small standalone entry, confirm `invoke()` commands keep working unchanged, confirm the window can be destroyed and rebuilt under the *same* label repeatedly (rules out needing glob-pattern capabilities).
5. Prototype `emit_to` retargeting: a small `Mutex<String>` "current target label," read per-emission by a background emitter loop (mirroring `run_terminal_bridge`), flipped by attach/detach commands — confirm no obvious event loss when flipped at the right point in the sequence.
6. Confirm dockview's `toJSON()`/`fromJSON()` round-trips through a Tauri command boundary and that a missing/corrupt persisted blob falls back to a hardcoded default without crashing.

**Output**: a written recommendation (confirm/reject dockview; confirmed detach design; confirmed emit_to-retargeting design; confirmed persistence approach), appended to this doc, informing exact scoping of Phases 2–6. Phase 1's props-to-context refactor doesn't depend on the spike's findings, so it can proceed in parallel with or before Phase 0.

**Validation checklist**: dockview drag/resize/theme in light+dark; tab-switch mount behavior observed via logging; xterm fit after simulated resize; detach/reattach of the throwaway Stack panel repeated 3+ times; layout JSON round-trip via a manual restart.

---

### Phase 1 — Props-to-context refactor (no behavior change)

Pulls the shared execution state that `App.tsx` currently threads down as props (`lastSnapshot`, `execState`, and callbacks `onStep`/`onExecStateChange`/`onEdit`/`onReset`) into a React context, **against the existing flex layout** — no dockview, no visual change. Done first and in isolation because it's a pure refactor (should produce zero observable behavior change) versus Phase 2's dockview integration, which is a real feature change; bundling them would make a UAT failure ambiguous about which change caused it. Isolating it here also means Phase 2 starts from a codebase where panels already read shared state via context, shrinking that phase's blast radius.

**Frontend**:
- Introduce a lightweight `ExecutionContext` (React context, following the existing `ThemeContext.tsx` precedent) carrying `lastSnapshot`/`execState` and the update callbacks.
- `App.tsx` provides the context instead of passing props into each panel it mounts directly (still the same direct-mount tree as today — only the plumbing changes).
- Update `RegisterPanel.tsx`, `DisassemblyPanel.tsx`, `CpuBusPanel.tsx`, and likely `MemoryPanel.tsx`/`WatchpointPanel.tsx` to read from context instead of props.

**Rust**: none.

**Risk**: subtle regressions in behavior that depends on prop-change timing (register changed-flag highlighting, exec-state-driven button disabling) — since this phase's entire purpose is "no observable change," any UAT finding here is a real bug, not a design tradeoff to weigh.

**Validation checklist**: behavioral A/B against pre-refactor — every panel's live updates during run/step, changed-flag highlighting, and exec-state-driven enabling/disabling should look and behave identically to before this phase.

---

### Phase 2 — Dockview shell for the six main-window panels

Replaces `App.tsx`'s hardcoded 3-column flex layout (`.app-layout`/`.col-left`/`.col-center`/`.col-right` in `global.scss`) with a dockview grid hosting Register/Disassembly/Memory/Stack/Watchpoint/CpuBus, in a default arrangement mirroring today's columns as **splits, not tabs** (today nothing is hidden behind a tab — the default layout shouldn't regress that). Terminal/Trace/Log stay exactly as-is (separate windows, untouched) — this phase is purely the main window's internal layout engine. Panels already read shared state via `ExecutionContext` (Phase 1), so dockview's ownership of mount/unmount doesn't require any further prop-drilling changes.

**Frontend**:
- Add `dockview` to `debugger/frontend/package.json`.
- New `debugger/frontend/src/layout/DockLayout.tsx` (first departure from the flat `src/` structure) wrapping dockview, plus `panelRegistry.ts` mapping panel ids → components, plus a hardcoded default layout (no persistence yet — Phase 3).
- `App.tsx` renders `<DockLayout />` (wrapped in `ExecutionContext.Provider`) in place of today's `.app-layout` div tree.
- Strip each panel's own `.panel-title` header markup in favor of dockview's tab/group headers (audit-and-edit pass across all six panel files, to avoid a double-header look).
- `global.scss`: remove the `.app-layout`/`.col-*` rules; add CSS mapping dockview's theme hooks onto the existing `--color-*` custom properties (confirmed feasible in Phase 0).

**Rust**: none — fully frontend-scoped, keeping this large phase reviewable in isolation.

**Risks**: default layout proportions looking worse than today's fixed split at first run; dockview's own focus/keyboard handling swallowing panel-internal shortcuts (Disassembly's F10 Step Over, breakpoint toggles).

**Validation checklist**: each of the 6 panels renders/updates live during run/step; breakpoints toggle; light/dark/auto themes render dockview chrome correctly; drag a panel to a new position without corruption; resize splits; window resize is sane; F10 and other panel shortcuts still work.

---

### Phase 3 — Layout persistence

Persists the dockview arrangement across restarts, following issue #342's non-profile-scoped `config/` convention (panel layout is a workspace preference like theme, not emulator config).

**Frontend**: `DockLayout` listens for layout-change events (debounced), serializes `api.toJSON()`, calls a new `set_dock_layout` command; on mount calls `get_dock_layout`, falls back to Phase 2's hardcoded default (and re-persists it) if missing/unparseable.

**Rust**: new `debugger/src-tauri/src/layout.rs`, mirroring `preferences.rs`'s state/`config_dir()` pattern but its own file — **`config/layout.json`, not TOML**: dockview's serialization is an arbitrary nested JSON tree; Rust treats the persisted value as opaque `serde_json::Value` (validates only "is this parseable JSON," never couples to dockview's internal schema). Add `layout::get_dock_layout`/`layout::set_dock_layout` to `lib.rs`'s `invoke_handler!` and a `LayoutState(Mutex<Option<Value>>)` mirroring `UiConfigState`'s write-through pattern.

**Risks**: schema drift across future dockview versions — mitigated by fallback-to-default. Scope note: this phase persists only the docked arrangement; Phase 6 must extend the schema with a "Terminal detached" flag.

**Validation checklist**: rearrange panels, restart, confirm layout survived; manually corrupt `config/layout.json`, confirm graceful fallback rather than a crash/blank screen.

---

### Phase 4 — Trace + Log become dock panels

Retires `trace.html`/`trace.tsx` and `log.html`/`log.tsx` as separate windows; `TracePanel.tsx`/`LogPanel.tsx` mount as dockview panels in a bottom tabbed group (VS Code convention — Output/Problems-style panels share a bottom dock, tabbed against each other; this also anticipates Terminal joining the same group in Phase 5).

**Frontend**:
- Register `trace`/`log` in `panelRegistry.ts`, add to the default layout as a tabbed bottom group.
- Delete `trace.html`/`trace.tsx`/`log.html`/`log.tsx`; strip their entries from `vite.config.ts`'s `rollupOptions.input`.
- Strip each component's own header markup, same as Phase 2.
- **The one real technical change**: `LogPanel.tsx`'s `listen("log-record", …)` currently receives events the backend sends via `emit_to(LOG_WINDOW_LABEL, …)`; once there's no `log` window, this must become `emit_to(MAIN_WINDOW_LABEL, …)`. Easy to miss — `emit_to` targeting a label with no live listener fails *silently*, no compile error, event just vanishes — must be explicitly called out and UAT'd.
- Trace is confirmed poll-based (`trace.rs` has no `emit_to` call — `get_trace_window` is invoked on demand), so no retargeting concern there; its `get_trace_status` hydrate-on-mount is unaffected by "window shown" becoming "dock tab first rendered."
- `toggle_trace_visibility`/`toggle_log_visibility` become frontend-only "reveal this dock tab" actions (dockview's `panel.api.setActive()`) driven by the existing Ctrl+Shift+Y/L bindings in `useAppKeyBindings.ts` — delete both commands from Rust.

**Rust**:
- `logging.rs`: `emit_to(LOG_WINDOW_LABEL, …)` → `emit_to(MAIN_WINDOW_LABEL, …)`; remove `toggle_log_visibility`.
- `trace.rs`: remove `toggle_trace_visibility`.
- `tauri.conf.json`: remove the `trace`/`log` static window entries.
- `capabilities/default.json`: remove `"trace"`/`"log"` from the `windows` array.
- `menu.rs`: change `TOGGLE_TRACE_ID`/`TOGGLE_LOG_ID` from `CheckMenuItem`s to plain `MenuItem`s labeled "Reveal Trace"/"Reveal Log" — click handler in `lib.rs` calls the frontend-facing reveal action instead of `toggle_window_visibility`.
- `lib.rs`: remove `install_toggleable_window_lifecycle` calls and `.manage`/`invoke_handler!` entries for trace/log; update the `#340` process-exit comment to reflect that only Terminal remains a potentially-separate window at this point (removed in Phase 6, not yet).
- `vite.config.ts`: drop `trace`/`log` from `rollupOptions.input`.

**Risks**: silent `emit_to` no-op if the Log retarget is missed; Ctrl+Shift+Y/L's behavior changing from "show a window" to "activate a dock tab" needs UAT for both "tab already visible in an inactive group" and "tab needs its group revealed" cases.

**Validation checklist**: Trace panel renders/pages correctly in its dock tab; Log panel receives live `log-record` events after the retarget; Ctrl+Shift+Y/L and the Window-menu "Reveal" items correctly reveal the tab from any starting state.

---

### Phase 5 — Terminal becomes a dock panel (still no detach)

Terminal joins the same bottom dock group as Trace/Log. Deliberately **not** adding detach yet, to isolate Terminal's genuinely harder lifecycle questions (below) from Phase 4's already-proven pattern.

**Frontend**:
- Extract `TerminalWindow.tsx`'s xterm instantiation/theming/keybinding logic into a new `TerminalPanel.tsx`, mounted as a dockview panel. Name/shape it for reuse — Phase 6 mounts this same component inside a standalone detached-window entry.
- Add a `ResizeObserver` on the panel's container driving `fitAddon.fit()` — genuinely new versus today's window-resize-only fit trigger, since a dockview split drag resizes the pane without resizing the OS window.
- **Architecture note for future scrollback restoration**: keep the terminal's output-consumption boundary (the `listen("terminal-output", …)` handler and xterm-write path) isolated inside `TerminalPanel` rather than smeared across app-level state, so a future "keep a live buffer independent of mount" change is additive rather than a rework.
- `useAppKeyBindings.ts`'s `isMainWindow`/`hasMainWindowAccelerator` machinery needs no changes — it's already a no-op once Terminal only lives in the main window, and becomes load-bearing again, unmodified, in Phase 6.

**Rust**:
- Remove the `show_terminal_window`/`hide_terminal_window` startup show-then-hide dance in `lib.rs`'s `setup()` — this WebKitGTK-realize workaround exists only because a hidden *separate* webview never runs its JS; once Terminal lives inside the main window's single, always-realized webview, the underlying problem no longer exists. Net simplification.
- The `terminal_ready` handshake itself (gating `load_or_reload_session` on `ready_rx`) still matters — same startup race — just drop the show/hide wrapper.
- **Headline risk, must be resolved by the Phase 0 spike before this phase is scoped in detail**: if dockview lazily mounts inactive tabs and Terminal's tab isn't active at startup, `TerminalPanel` never mounts, `terminal_ready` never fires, and `load_or_reload_session` deadlocks on `ready_rx.await`. Mitigation depends on Phase 0's finding: force eager-mount of all registered panels, or decouple session bring-up from Terminal's mount timing (buffer early PTY output backend-side until a listener attaches — a bigger change to `load_session`'s flow, and also a natural first step toward the future scrollback-preservation architecture noted above).
- `emit_to(TERMINAL_WINDOW_LABEL, …)` in `run_terminal_bridge` → `emit_to(MAIN_WINDOW_LABEL, …)` for this phase (Phase 6 makes the target dynamic again).
- Remove `toggle_terminal_visibility`; same frontend-only "reveal" treatment as Phase 4.
- `tauri.conf.json`: remove the static `terminal` window entry. `capabilities/default.json`: remove `"terminal"` from `windows` (Phase 6 re-adds it for the detached case).
- `menu.rs`: change `TOGGLE_TERMINAL_ID` to "Reveal Terminal" for this phase — Phase 6 changes it again to "Detach"/"Attach Terminal" once that concept exists.
- Clipboard support (#346, Ctrl+Shift+C/V) moves unchanged into `TerminalPanel.tsx`; `main`'s capabilities already cover clipboard read/write, so no capabilities change — still needs an explicit UAT re-check given #346's history of subtle regressions (invisible-selection theming, double-paste).

**Validation checklist**: Terminal panel renders in its dock tab; session bring-up completes at startup regardless of which bottom-group tab is initially active (no deadlock); typing echoes; output renders live during a running program; resizing the dock split refits the terminal; switching away and back preserves scrollback and refits correctly; Ctrl+Shift+C/V still work; xterm theme follows app theme changes.

---

### Phase 6 — Detach Terminal to its own native OS window

Restores Terminal's own-window capability, built on Phase 0's dynamic-`WebviewWindowBuilder` mechanism rather than dockview's popout. The largest and riskiest phase.

**Frontend**:
- New standalone entry `terminal-detached.html`/`terminal-detached.tsx`, mirroring the *removed* `TerminalWindow.tsx`'s independent theme-sync wrapper (its own `get_theme`/`theme-changed` subscription, since React context can't cross a window boundary) — wrapping the shared `TerminalPanel` component from Phase 5.
- Add `terminal-detached` back into `vite.config.ts`'s `rollupOptions.input`.
- A "Detach" action on the Terminal tab's header (dockview supports custom tab actions) calling a new `detach_terminal` command and removing the panel from the dock model.
- Closing the detached window (native chrome or shortcut) re-attaches the panel into the main layout — reuses the existing close-lifecycle pattern rather than inventing a separate "attach" gesture.
- `useAppKeyBindings.ts` needs no changes — its main-window-label check already generalizes correctly.

**Rust** (the largest chunk of new backend code in the plan):
- `terminal.rs`: fixed, **reused** label constant `TERMINAL_DETACHED_WINDOW_LABEL` (`"terminal-detached"`), rebuilt each detach cycle rather than uniquely generated — per Phase 0's confirmation that Tauri allows rebuilding under a previously-used label once fully destroyed, and rules out any "which detached terminal is real" ambiguity since at most one exists at a time.
- `detach_terminal(app)` command: builds the window, calls `remove_menu()` on it immediately (required, easy to forget), installs a lifecycle hook, flips a new shared target-label state.
- New `TerminalTargetWindow(Mutex<String>)` state, read by `run_terminal_bridge` before each `emit_to` call — replaces the hardcoded label with the dynamic target. This is the concrete implementation of the "redirect `emit_to` by panel location" mechanism.
- Closing the detached window: **destroy** it (not hide, deliberately diverging from `install_toggleable_window_lifecycle`'s hide-not-destroy pattern) and flip the target label back to `MAIN_WINDOW_LABEL`, emitting an event the main layout listens for to reinsert the Terminal panel.
- `terminal_ready`'s oneshot is already safe against a repeat call from a freshly (re)mounted `TerminalPanel` — `.take()` returns `None` after first use, confirmed safe by inspection, no change needed.
- `capabilities/default.json`: add `"terminal-detached"` as a static entry (fixed label ⇒ no glob needed) with the same clipboard permissions `main` has.
- `menu.rs`: change the Window > Terminal item from a checkbox to a plain item whose label toggles "Detach Terminal…" / "Attach Terminal", mutated in place (reusing the existing `rebuild_open_recent_submenu` in-place-mutation pattern).

**Headline risks**:
1. **`emit_to`-retarget race**: a PTY-output chunk could theoretically be read between the old target tearing down and the new one existing. Mitigate by flipping the target-label state only after the new window/panel is fully constructed and before tearing down the old one; accept a narrow, low-consequence window where a byte or two could be lost mid-detach (comparable to a terminal multiplexer detach). UAT case: type continuously while clicking Detach, confirm no dropped/garbled output.
2. **Scrollback loss on detach/reattach — accepted**, with the isolation work from Phase 5 keeping the door open for a future preserving mode.
3. Extend Phase 3's persisted layout schema with a "Terminal currently detached" flag, so a restart mid-detach reopens the detached window rather than silently re-docking it.
4. Re-verify `request_exit`'s "closing main means the whole app exits" behavior once a second real window can exist again — `request_exit` already unconditionally calls `app.exit(0)` once confirmed, so this should already hold, but deserves an explicit UAT case: detach Terminal, close Main via native chrome, confirm the confirm-dialog (or skip-preference) fires and the whole app exits together.

**Validation checklist**: detach → type → verify no drop → reattach, repeated 3+ times (label-reuse correctness, not just once); close via native chrome vs. in-app action; clipboard in the detached window; theme sync in the detached window; exit-while-detached.

---

## Cross-cutting concerns

**Layout persistence**: `config/layout.json` via `layout.rs`, non-profile-scoped per #342. JSON (not TOML), Rust treats it as opaque. Phase 3 persists the docked arrangement only; Phase 6 extends the schema with the detached-Terminal flag.

**Capabilities scoping for dynamically-created windows**: simpler than it first appears — because Phase 6 reuses one fixed label rather than per-instance labels, a plain static `capabilities/default.json` entry is sufficient; Tauri 2's glob-pattern matching isn't needed under this design (and avoiding unique labels is precisely why — there can only ever be zero or one detached Terminal).

**Fate of existing lifecycle workarounds**:
- `install_toggleable_window_lifecycle` (hide-not-destroy + the Wayland `Focused(true)` decoration-hit-test workaround): fully obsolete for Trace/Log (Phase 4) and docked-Terminal (Phase 5). Phase 6's detached-Terminal window needs a new, different lifecycle function keeping the Wayland workaround but replacing hide-not-destroy with real destroy-on-close + reattach.
- The `#340` "hidden windows keep the process alive" fix: still applies, narrower after Phase 5 (only a detached Terminal can be a second live window at all) — must be re-verified once that possibility returns in Phase 6 (risk #4 above), not removed.
- `remove_menu()`'s accelerator-collision workaround: obsolete for Trace/Log/docked-Terminal, but Phase 6's `detach_terminal()` must call it again on the newly created window for the same underlying reason (`app.set_menu()` still attaches to every window lacking its own menu) — flag as a required, easy-to-forget line.

## Effort summary

| Phase | Scope | Effort |
|---|---|---|
| 0 | Spike: dockview-in-Tauri + detach mechanics | M |
| 1 | Props-to-context refactor (no behavior change) | S/M |
| 2 | Dockview shell, 6 main panels | L |
| 3 | Layout persistence | S/M |
| 4 | Trace + Log → dock panels | M |
| 5 | Terminal → dock panel (no detach) | L |
| 6 | Terminal detach-to-window | XL |

## Critical files

- `debugger/frontend/src/App.tsx` — layout root; Phase 1 adds `ExecutionContext`, Phase 2 replaces its layout tree with `DockLayout`
- `debugger/frontend/src/TerminalWindow.tsx` — split into `TerminalPanel.tsx` (Phase 5) + `terminal-detached.tsx` (Phase 6)
- `debugger/frontend/src/useAppKeyBindings.ts` — main-window-label scoping, already anticipates this migration
- `debugger/frontend/src/styles/global.scss` — theme custom properties to integrate with dockview
- `debugger/frontend/vite.config.ts` — multi-entry `rollupOptions.input`
- `debugger/src-tauri/src/lib.rs` — `setup()`, `request_exit`, `install_toggleable_window_lifecycle`, `invoke_handler!`
- `debugger/src-tauri/src/terminal.rs` — `emit_to` targeting, `terminal_ready` handshake
- `debugger/src-tauri/src/menu.rs` — `WindowMenuState`, `toggle_window_visibility`
- `debugger/src-tauri/src/preferences.rs` — pattern to mirror for new `layout.rs`
- `debugger/src-tauri/tauri.conf.json`, `debugger/src-tauri/capabilities/default.json`

## Verification

Each phase: `cargo build --workspace`, `cargo clippy` (covers the debugger crate), `cargo test --workspace` for any Rust changes, then a manual UAT pass by the user against that phase's validation checklist above — per project convention, Claude does not drive the Tauri GUI programmatically. Phase 0's spike findings should be written up and confirmed with the user before Phase 2's scope is finalized, since they can change Phases 5/6's design (Phase 1 doesn't depend on the spike and can proceed independently).

## Process note

Unlike most prior single-branch, multi-commit plans in this repo, this plan is broken into **one GitHub issue per phase**, each its own branch and PR (`Closes #N`), matching the debugger-profiles-plan arc rather than the trace-window-plan arc — chosen because each phase here is independently substantial (up to XL) and phases are expected to be picked up in separate sessions, sometimes with a gap between them. See the "Dockview" label for the full set of issues and their dependency order.

## GitHub issues

| Phase | Issue | Depends on |
|---|---|---|
| 0 | #379 — Spike: dockview-in-Tauri + detach mechanics | — |
| 1 | #380 — Props-to-context refactor | — (independent of #379) |
| 2 | #381 — Dockview shell for the six main-window panels | #379, #380 |
| 3 | #382 — Dock layout persistence | #381 |
| 4 | #383 — Trace + Log become dock panels | #381 |
| 5 | #384 — Terminal becomes a dock panel | #379, #383 (needs the bottom dock group established) |
| 6 | #385 — Detach Terminal to its own native window | #379, #384 |
