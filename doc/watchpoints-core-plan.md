# Watchpoints core support: display-evaluation for `watch` / `emulator::cpu`

## Context

The debugger's initial watchpoints feature (a read-only panel showing each
watchpoint expression's current value and true/false/error status — see
`doc/watchpoint-ui-initial-support.md`) needs a way to evaluate *every*
watchpoint in a set against live CPU state and get a value back for each one,
independently of the CPU's own breakpoint-style watch-triggered halting.

Today's `emma65::watch` and `emulator::cpu` APIs don't support this:

- `WatchEvaluator::evaluate_all` short-circuits at the first non-zero result
  — it's designed for breakpoint-style halting, and is already used that way
  inside `Cpu::step()` (`StepResult::WatchTriggered`/`WatchError`).
- There's no public way to obtain a `WatchContext` for a live `Cpu` from
  outside `src/emulator/cpu/mod.rs` — the only implementer, `CpuWatchContext`,
  is private (and should stay that way).
- `cpu.evaluator_mut()` is the CPU's own internal evaluator, already wired
  into `Cpu::step()`'s real halting behavior. A display feature must **not**
  reuse it — that would make display watchpoints halt execution as a side
  effect.

This unit adds the two small, narrow additions needed to unblock the
debugger UI unit, entirely within the core library (no debugger-crate or
frontend changes here).

## Key existing pieces to reuse

- `WatchCompiler::new(map_register, map_flag, map_symbol)` /
  `.compile_all(source, &mut evaluator) -> (Vec<Watchpoint>, Vec<Error>)`
  (`src/watch/mod.rs:54-98`) — already collects every compile error in a
  source file rather than stopping at the first. Not modified by this unit,
  but the consuming (debugger) unit will rely on it.
- `map_register_name` / `map_flag_name` (`src/emulator/cpu/mod.rs:975-1001`)
  — already public, already the right signature for `WatchCompiler::new`.
  Not modified by this unit.
- `Bus::symbol_table()` / `SymbolTable::address_for(name) -> Option<u16>`
  (`src/emulator/bus/mod.rs:312`, `src/emulator/bus/symbol.rs`) — `SymbolTable`
  is `Clone`. Not modified by this unit.
- `WatchEvaluator::evaluate_all`'s existing loop shape
  (`src/watch/mod.rs:170-182`) — the new method mirrors this minus the early
  return.
- The private `CpuWatchContext` struct (`src/emulator/cpu/mod.rs:1005-1071`)
  — the new `Cpu` method wraps it internally; it does not need to become
  public.

## Implementation

### 1. `src/watch/mod.rs` — new `WatchEvaluator` method

```rust
/// Evaluates every watchpoint independently against `context`, in
/// `watchpoints()` order, without stopping at the first non-zero result
/// (unlike `evaluate_all`, which is designed for breakpoint-style halting).
/// One result per watchpoint — for displaying a per-row value/status rather
/// than triggering on the first truthy one.
pub fn evaluate_each(&mut self, context: &dyn WatchContext) -> Vec<Result<Operand, WatchError>> {
    let mut results = Vec::with_capacity(self.watchpoints.len());
    for wp in &self.watchpoints {
        results.push(eval(&wp.code, context, &mut self.var_storage));
    }
    results
}
```

Add a test next to the existing `evaluate_all_*` tests, reusing the same
`compiler()`/`MockMachine` helpers (`src/watch/mod.rs:202-228`): compile a
truthy expression, a falsy one, and a division-by-zero one; assert
`evaluate_each` returns all three results in order (proving no
short-circuiting, and that one watchpoint's error doesn't affect its
siblings' results).

### 2. `src/emulator/cpu/mod.rs` — new public `Cpu` method

```rust
/// Evaluates each of `evaluator`'s watchpoints against this CPU's current
/// register and (peek-only, side-effect-free) bus state. Independent of
/// `self.evaluator` and does not affect execution — for a caller-owned
/// evaluator driving a display-only watchpoint view, as opposed to
/// `Cpu::step()`'s own watch-triggered halting.
pub fn evaluate_watchpoints(&self, evaluator: &mut WatchEvaluator) -> Vec<Result<Operand, WatchError>> {
    let ctx = CpuWatchContext { regs: &self.regs, bus: &self.bus };
    evaluator.evaluate_each(&ctx)
}
```

Add to the existing `impl Cpu` block. Add a test near the existing
`watch_step`/`make_compiler` helpers (`src/emulator/cpu/mod.rs:1864`, `2029`)
proving:

(a) it returns correct per-watchpoint values against real `Cpu` state;

(b) populating a *separate* `WatchEvaluator` via this method and calling it
does **not** cause a subsequent `cpu.step(...)` to return
`StepResult::WatchTriggered` — i.e. it's provably independent of
`cpu.evaluator_mut()`.

## Verification

- `cargo test --workspace` — new `evaluate_each` and `evaluate_watchpoints`
  unit tests, plus the full existing suite.
- `cargo clippy --all-targets` from the repo root.

## Downstream dependency

`doc/watchpoint-ui-initial-support.md` (the debugger backend/frontend unit)
depends on both `WatchEvaluator::evaluate_each` and
`Cpu::evaluate_watchpoints` existing and merged before it can call them from
`debugger/src-tauri/src/watchpoints.rs`.
