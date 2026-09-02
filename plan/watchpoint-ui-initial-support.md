# Debugger: Initial Watchpoints (Display Only)

## Context

Story 12 of the debugger implementation plan
(`plan/debugger-implementation-plan.md:300-326`). Depends on #59 (closed) and
on the core-library additions in `plan/watchpoints-core-plan.md`
(`WatchEvaluator::evaluate_each`, `Cpu::evaluate_watchpoints`) — that unit
must land first.

This adds a read-only watchpoint panel: on startup, load watch expressions
from `~/.emma/debugger/default/watchpoints.emw`, compile them with
`emma65::watch::WatchCompiler`, and show each expression's current value and
true/false/error status, refreshed after every step/auto-step-tick/halt. No
add/edit/remove/enable/disable — that's stories 16-20, which explicitly build
on this story's infrastructure, so the shapes chosen here should stay
reusable.

The frontend already has a placeholder for this:
`debugger/frontend/src/App.tsx:86` has a literal
`{/* Watchpoints — story 12 */}` comment in `col-left`, right below
`MemoryPanel`.

Two deviations from the literal issue text, confirmed with the user:

- **Compile-error handling**: instead of `eprintln!` + `process::exit`, a
  `watchpoints.emw` compile error is shown *inside the watchpoint panel
  itself* (rows empty, error message displayed there). The rest of the
  debugger stays fully usable. A missing file is not an error — it just
  means zero watchpoints.
- Both choices avoid a side-feature (watchpoints) being able to either kill
  the whole GUI process or block the entire debugger behind the existing
  `session-status` splash screen (which today only covers
  emulator.toml/CPU-reset failures and blocks *all* panels when `ok: false`).

## Key existing pieces to reuse

- `WatchCompiler::new(map_register, map_flag, map_symbol)` /
  `.compile_all(source, &mut evaluator) -> (Vec<Watchpoint>, Vec<Error>)`
  (`src/watch/mod.rs:54-98`).
- `map_register_name` / `map_flag_name` (`src/emulator/cpu/mod.rs:975-1001`).
- `Bus::symbol_table()` / `SymbolTable::address_for(name) -> Option<u16>`
  (`src/emulator/bus/mod.rs:312`, `src/emulator/bus/symbol.rs`) — `SymbolTable`
  is `Clone`, so `map_symbol` can close over a cloned snapshot (needed since
  `WatchCompiler::new`'s closures must be `'static`). The debugger's actual
  `~/.emma/debugger/default/emulator.toml` loads a `labels` file on the ROM
  device, so symbols are already populated by the time `load_session()`
  returns — no ordering problem.
- `theme::debugger_config_dir()` (`debugger/src-tauri/src/theme.rs:36-40`) →
  `~/.emma/debugger/default/` — join `"watchpoints.emw"` onto it, same as
  `emulator.toml`/`ui.toml`.
- `RegisterPanel.tsx`/`StackPanel.tsx`'s radix-cycle pattern (`DataRadix`
  union + `DATA_RADIX_CYCLE` array + local `formatData`, `.radix-btn` class)
  — the codebase's established convention is to duplicate this small block
  per panel rather than share a util module; follow that convention here too.
- `CpuBusPanel.tsx`'s event-driven fetch pattern (`invoke` on mount +
  `listen("debugger-halted"|"debugger-run-stopped", fetch)`), and its `●`
  indicator-dot-with-CSS-class-driven-color pattern for the true/false/error
  dot.
- `WatchEvaluator::evaluate_each` and `Cpu::evaluate_watchpoints` from
  `plan/watchpoints-core-plan.md` — the non-short-circuiting, non-halting
  evaluation primitives this unit is built on. **The debugger must own its
  own separate `WatchEvaluator` instance and never call
  `cpu.evaluator_mut()`** — that's the CPU's own internal evaluator, already
  wired into `Cpu::step()` to return `StepResult::WatchTriggered`/
  `WatchError` (real halting behavior).

## Implementation

### 1. `debugger/src-tauri/src/watchpoints.rs` (new module)

Following the `theme.rs` precedent (a small dedicated module for a cohesive
feature) rather than piling into `lib.rs`.

```rust
pub struct WatchState(pub Mutex<WatchData>);

pub struct WatchData {
    pub evaluator: WatchEvaluator,       // empty if nothing loaded or compile failed
    pub compile_error: Option<String>,   // Some(...) => watchpoints.emw failed to compile
}

#[derive(Clone, serde::Serialize)]
pub struct WatchpointRow {
    pub source: String,
    pub value: Option<u32>,     // None only when error is Some
    pub error: Option<String>,
}

#[derive(Clone, serde::Serialize)]
pub struct WatchpointsSnapshot {
    pub compile_error: Option<String>,   // whole-file compile error; rows is empty when Some
    pub rows: Vec<WatchpointRow>,
}

/// Loads and compiles `~/.emma/debugger/default/watchpoints.emw` against
/// `symbol_table`. A missing file means zero watchpoints (not an error).
pub fn load_watchpoints(symbol_table: &SymbolTable) -> Result<WatchEvaluator, String> {
    let path = theme::debugger_config_dir()?.join("watchpoints.emw");
    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WatchEvaluator::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let table = symbol_table.clone();
    let mut compiler = WatchCompiler::new(map_register_name, map_flag_name,
        move |name| table.address_for(name).map(|a| a as u32));
    let mut evaluator = WatchEvaluator::new();
    let (watchpoints, errors) = compiler.compile_all(&source, &mut evaluator);
    if !errors.is_empty() {
        let message = errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
        return Err(message);
    }
    for wp in watchpoints { evaluator.add(wp); }
    Ok(evaluator)
}

#[tauri::command]
pub fn get_watchpoints(cpu_state: State<CpuState>, watch_state: State<WatchState>) -> WatchpointsSnapshot {
    let mut watch = watch_state.0.lock().unwrap();
    if let Some(err) = &watch.compile_error {
        return WatchpointsSnapshot { compile_error: Some(err.clone()), rows: Vec::new() };
    }
    let cpu_guard = cpu_state.0.lock().unwrap();
    let Some(cpu) = cpu_guard.as_ref() else {
        return WatchpointsSnapshot { compile_error: None, rows: Vec::new() };
    };
    let results = cpu.evaluate_watchpoints(&mut watch.evaluator);
    let rows = watch.evaluator.watchpoints().iter().zip(results).map(|(wp, result)| {
        match result {
            Ok(value) => WatchpointRow { source: wp.source().to_string(), value: Some(value), error: None },
            Err(e) => WatchpointRow { source: wp.source().to_string(), value: None, error: Some(e.to_string()) },
        }
    }).collect();
    WatchpointsSnapshot { compile_error: None, rows }
}
```

`get_watchpoints` computes fresh on every call (locks `CpuState` directly),
same convention as `get_registers`/`get_disassembly`/`get_breakpoints` — no
extra cache struct needed. While genuinely free-running, `CpuState` is `None`
and it returns an empty snapshot, but the frontend only calls this in
response to `debugger-halted`/`debugger-run-stopped` (see below), so that
branch is essentially unreachable in normal use.

**Wiring into `lib.rs`**:

- `mod watchpoints;` declaration.
- `.manage(watchpoints::WatchState(Mutex::new(watchpoints::WatchData { evaluator: WatchEvaluator::new(), compile_error: None })))`
  in `run()` alongside the other `.manage(...)` calls (`lib.rs:1175-1195`).
- `get_watchpoints` added to the `tauri::generate_handler![...]` list.
- In the `.setup()` async task, right after `cpu.reset()` succeeds and before
  the existing `CpuBusCache`/`CpuState` population (`lib.rs:1273-1285`):

  ```rust
  let symbol_table = cpu.bus().symbol_table().clone();
  let watch_data = match watchpoints::load_watchpoints(&symbol_table) {
      Ok(evaluator) => watchpoints::WatchData { evaluator, compile_error: None },
      Err(message) => {
          eprintln!("watchpoints.emw: {message}"); // still log it, just non-fatal
          watchpoints::WatchData { evaluator: WatchEvaluator::new(), compile_error: Some(message) }
      }
  };
  *handle.state::<watchpoints::WatchState>().0.lock().unwrap() = watch_data;
  ```

  This never blocks or fails `emit_status`'s "Emulator session ready" — a bad
  `watchpoints.emw` is fully independent of session readiness.

### 2. `debugger/frontend/src/WatchpointPanel.tsx` (new)

- `interface WatchpointRow { source: string; value: number | null; error: string | null; }`
  and `interface WatchpointsSnapshot { compile_error: string | null; rows: WatchpointRow[]; }`.
- Local `DataRadix`/`DATA_RADIX_CYCLE`/`formatData` duplicated per the
  existing per-panel convention (hex/udec/sdec/oct/bin, same order the issue
  lists). One radix button in the panel header, applying to all rows
  (matches "a value radix cycle control", singular, and mirrors
  `RegisterPanel`'s one-state-per-group approach).
- `fetchWatchpoints` via `invoke<WatchpointsSnapshot>("get_watchpoints")`,
  mount-fetch plus `listen("debugger-halted", ...)` and
  `listen("debugger-run-stopped", ...)` (mirrors `CpuBusPanel.tsx`'s pattern;
  intentionally omits `debugger-running-tick` — the spec only requires
  refresh after step/auto-step-tick/halt, all of which fire one of those two
  events; there's no requirement to stream values during unlimited free-run,
  and doing so would need bus access that free-running snapshots don't
  carry).
- If `compile_error` is set: render it in place of the row list (e.g.
  `<div className="watchpoint-error">{compile_error}</div>`), styled with
  `--color-error`.
- Otherwise render one row per entry: expression `<span>` (CSS
  `text-overflow: ellipsis; white-space: nowrap; overflow: hidden` by
  default; `onClick` toggles an `expandedIndex: number | null`
  component-state entry that, when matching, switches that row's span to
  `white-space: normal` to reveal the full text — new pattern, no existing
  precedent found), value `<span>` (formatted via local `formatData` when
  `value !== null`, else "ERR"), and a status dot `<span>` colored via a
  class computed as `error ? "wp-error" : value !== 0 ? "wp-true" : "wp-false"`
  (mirrors `CpuBusPanel`'s indicator-dot pattern). Map `--color-error`
  (error), `--color-success` (true), `--color-idle` (false) — the same triad
  already used for other true/false/idle/error indicators in this codebase.
- New `debugger/frontend/src/styles/watchpoints.scss`, following
  `stack.scss`'s structure (header row with `.panel-title` + `.radix-btn`,
  scrollable row list below).

### 3. `App.tsx` / layout

- Replace `{/* Watchpoints — story 12 */}` (`App.tsx:86`) with
  `<WatchpointPanel />` (no props needed — it's fully self-contained via its
  own `invoke`/`listen` calls, same as `StackPanel.tsx`).
- `debugger/frontend/src/styles/memory.scss`: change `.memory-panel`'s
  `height: 100%` to `flex: 1 1 auto` so it shares `col-left`'s vertical space
  with the new panel instead of claiming all of it (today `MemoryPanel` is
  `col-left`'s only child). Give `.watchpoint-panel` a bounded/scrollable
  body (`flex: 0 1 auto`, `overflow-y: auto` on the row list) so a long
  watchpoint list scrolls instead of pushing `MemoryPanel` off.

## Verification

- `cargo test --workspace` — full suite, including the core-unit's
  `evaluate_each`/`evaluate_watchpoints` tests.
- `cargo clippy --all-targets`, run once from the repo root and once from
  `debugger/src-tauri/` (belt-and-suspenders per this repo's own
  convention).
- `npm run build` (tsc + vite) in `debugger/frontend/` for type-checking.
- Manual UAT (per this project's convention — driven by the user, not the
  assistant): create `~/.emma/debugger/default/watchpoints.emw` with a few
  expressions (e.g. one always-true like `1 == 1;`, one referencing `A`, one
  referencing a symbol from the loaded TaliForth labels, and one
  deliberately long expression to check truncation), launch the debugger,
  and confirm:
  - rows appear with correct values/status dots on load;
  - values update after Step Into, after auto-step ticks, and after a Run
    completes/halts;
  - the radix button cycles all rows' value formatting together;
  - clicking a long/truncated expression reveals the full text;
  - renaming the file to introduce a syntax error and relaunching shows the
    error *only* in the watchpoint panel, with the rest of the debugger
    (disassembly, registers, stepping) fully functional.
