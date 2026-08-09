# Foundational support for a priority interrupt controller (PIC)

Design discussion outcome, captured for a later implementation pass. This does **not** build a
PIC device — it adds the library-level primitives needed to make one buildable, both by hand
(`CpuBuilder`/`BusConfig`) and through the config-driven `DeviceModule`/`DeviceRegistry` system
used by every other built-in device.

## Context

`VectorResolver` (added in PR #313) already lets external hardware substitute the vector
address read during a RESET/NMI/IRQ/BRK vector fetch, and `InterruptController::poll_devices`
(PR #317) already aggregates every device's IRQ/NMI/RESET state once per CPU step. Both were
built anticipating a priority interrupt controller (PIC) — a memory-mapped device that ranks
multiple IRQ sources and tells the CPU which per-source vector to jump to — but no such device
exists yet, and today there's a real gap: `VectorResolver::resolve` has no way to see which
sources are currently interrupting, and `InterruptController`'s per-source state
(`irq_sources: u64`) is entirely private (only the aggregate `irq_active() -> bool` is public).

A PIC must not re-track interrupt state itself — `InterruptController` already owns exactly the
per-source state a priority encoder needs (which sources are asserting, NMI/RESET latches), and
duplicating it in a separate PIC-side struct would just create a second source of truth that has
to stay in sync with the first. The right fix is to give `VectorResolver` a live read path into
the *real* `InterruptController`, not a copy.

Priority convention (confirmed): lower `IrqSource` value = higher priority (`IrqSource(0)` is
highest), matching real PIC hardware. A typical PIC exposes 8 priority slots against our 64
possible sources, commonly folding sources 7..63 onto the lowest slot as a "wired OR" — that
folding policy belongs entirely inside the PIC implementation itself, not in
`InterruptController`, which stays a policy-free aggregator.

This pass adds only the library-level primitives needed to make a PIC buildable — vector
registers (read-only/write-once) and mask registers (consulted only at interrupt-service time,
not on every bus access) mean the PIC's `IoDevice` half never needs live interrupt state, so no
changes to `IoDevice` are needed. Building the actual PIC device is a separate, later pass.

That "buildable" claim has one more gap beyond the CPU-internal vector/interrupt plumbing: this
crate's real extensibility point is the config-driven `DeviceModule`/`DeviceRegistry` system (a
`type@address` device spec dispatches to a module's `instantiate`, which mutates and returns a
`BusConfig` — see `RamModule`, `Via6522Module`, etc.). Today `DeviceModule::instantiate` has
*no* channel back to the caller except that `BusConfig`, and `Cpu::builder(...)` isn't even
constructed until after every device has finished instantiating (`Config::build_devices`,
`src/emulator/config/emulator.rs`) — so a config-driven PIC module could add itself to the bus
but could never install itself as the CPU's `VectorResolver`; `CpuBuilder::vector_resolver(...)`
is currently dead code outside one unit test. A PIC built by hand via `CpuBuilder`/`BusConfig`
directly (bypassing `Config::build`) wouldn't hit this gap, but that would make it a second-class
citizen compared to every other built-in device, which is configurable via TOML/CLI like the
rest. Closing this is in scope for "making a PIC buildable."

## Design

Four small, additive changes:

### 1. `InterruptController` gains a read-only per-source accessor

In `src/emulator/bus/interrupt.rs`:

- Add `pub fn active_sources(&self) -> impl Iterator<Item = IrqSource> + '_`, yielding
  currently-asserting sources in ascending `IrqSource` order (i.e. priority order). Non-draining
  — unlike `take_nmi`/`take_reset`, it must not mutate state, since a `VectorResolver` is called
  with only `&self` access.
- Add `PartialOrd, Ord` to `IrqSource`'s derive list, formalizing "lower value = higher
  priority" as a property of the type itself rather than something every consumer has to
  redefine.
- Add a unit test for `active_sources()` alongside the existing `InterruptController` tests
  (multiple sources asserted, ascending order, empty when none active).

### 2. `VectorResolver::resolve` gains access to the controller

In `src/emulator/cpu/vector.rs`:

- Change the trait method to `fn resolve(&self, vector_addr: u16, interrupts: &InterruptController) -> u16;`
- Update `IdentityVectorResolver`'s impl and its test to match (it still ignores `interrupts`
  and returns `vector_addr` unchanged).

In `src/emulator/cpu/mod.rs`, update both call sites to pass the sibling `interrupts` field —
confirmed borrow-safe: neither `reset()` nor `service_interrupt()` touches `self.interrupts`
anywhere else in the function body, and the one `self.interrupts`-mutating call near each site
(`take_reset`/`take_nmi` gating entry, `consume_irq_pulses` after return) is always sequenced
strictly before or after the `resolve()` call, never interleaved:
  - `reset()` (line 244): `self.vector_resolver.resolve(RESET_VECTOR, &self.interrupts)`
  - `service_interrupt()` (line 934): `self.vector_resolver.resolve(vector_addr, &self.interrupts)`

### 3. Expose the vector address constants

`RESET_VECTOR`, `NMI_VECTOR`, `IRQ_VECTOR` (`src/emulator/cpu/mod.rs` lines 27-29) are currently
private `const`s. Make them `pub const` so a `VectorResolver` implementation outside this module
(a PIC) can compare the nominal `vector_addr` it's given against named constants instead of
hardcoding `0xFFFC`/`0xFFFA`/`0xFFFE`.

### 4. `BusConfig` carries a `VectorResolver` through to CPU construction

`BusConfig` (`src/emulator/bus/mod.rs`) is already the single object threaded, by value, through
every iteration of the device-instantiation loop (`bus_config = registry.instantiate(bus_config, ...).await?`)
— it's the primary channel, not a side one, and every existing device module already extends it
via the same consuming-builder style (`.ram()`, `.rom()`, `.device()`, each `Result<Self, BusConfigError>`).
Fits far more naturally than a parallel `InstantiationContext` side-channel:

- Add `vector_resolver: Option<Box<dyn VectorResolver>>` field to `BusConfig`.
- Add `pub fn vector_resolver(mut self, resolver: Box<dyn VectorResolver>) -> Result<Self, BusConfigError>`,
  following the exact convention `.device()` already uses for its `DuplicateDeviceId` check:
  errors with a new `BusConfigError::DuplicateVectorResolver` variant if one is already installed
  (naming matches the existing `DuplicateDeviceId`/`DuplicateIrq` variants), rather than silently
  overwriting.
- Add `pub fn take_vector_resolver(&mut self) -> Option<Box<dyn VectorResolver>>` — a non-consuming
  accessor (mirrors the `take_nmi`/`take_reset` "drain" idiom already used elsewhere in this
  codebase), called by `Config::build_devices` immediately before `.build()` consumes the rest of
  `BusConfig` into a `Bus`.

In `src/emulator/config/emulator.rs`'s `build_devices`, between `let bus = bus_config.build();`
and `Cpu::builder(...)`, take the resolver first and conditionally chain it onto the builder:

```rust
let vector_resolver = bus_config.take_vector_resolver();
let bus = bus_config.build();
let mut builder = Cpu::builder(variant)
    .clock_speed(self.clock_speed_hz.map_or(ClockSpeed::unlimited(), ClockSpeed::hz))
    .bus(bus);
if let Some(resolver) = vector_resolver {
    builder = builder.vector_resolver(resolver);
}
let cpu = builder.build().map_err(BuildError::Cpu)?;
```

A future PIC's `DeviceModule::instantiate` would then just call
`bus_config.vector_resolver(Box::new(my_resolver)).map_err(DeviceModuleError::BusConfig)?` as
part of its normal builder chain, alongside `.device(...)` — the exact same
`.map_err(DeviceModuleError::BusConfig)?` boilerplate `Via6522Module` already uses for its own
`BusConfigError`-returning calls. No new concept for module authors, and **no changes needed to
`InstantiationContext`, `DeviceModule::instantiate`'s signature, or any of its existing
struct-literal construction sites** (`main.rs`, debugger `lib.rs`, tests) — all of that stays
untouched since `BusConfig` was already threaded everywhere it's needed.

**A system config may install at most one PIC.** Since `BusConfig` is threaded by ownership
through `self.devices.iter()` sequentially in `Config::build_devices`, if a *second* configured
device module also calls `.vector_resolver(...)` (e.g. two `type=pic@...` entries, or any other
module mistakenly trying to install one), `BusConfig` already holds `Some(_)` from the first and
returns `Err(BusConfigError::DuplicateVectorResolver)` — no new error-handling mechanism needed.
That propagates exactly like any other device's `BusConfigError` already does: through
`DeviceModuleError::BusConfig(...)`, out of `registry.instantiate(...)`, and into
`Config::build_devices`'s existing `.map_err(|e| BuildError::Device { module_name, address, source: e })?`,
aborting the build with a clear per-device error rather than silently keeping the first or last
resolver. This should be covered by an integration-style test in `src/emulator/config/emulator.rs`
(or wherever `Config::build`/`build_devices` is already tested) with two device specs that each
attempt to install a resolver, asserting the second fails with `BuildError::Device { source: DeviceModuleError::BusConfig(BusConfigError::DuplicateVectorResolver), .. }`.

### What's deliberately unchanged

- `IoDevice` trait: no new methods. Vector-table registers are read-only/write-once and mask
  registers are consulted only when interrupt status is evaluated (i.e. inside `resolve()`), so
  the PIC's device-facing register reads/writes never need live `InterruptController` access.
- `InterruptController`: no vectoring/priority logic added — it stays a faithful, dumb
  aggregator of raw IRQ/NMI/RESET state, matching its existing design ("real 6502 hardware
  model... faithful to the actual chip").
- `DeviceModule::instantiate`'s signature and `InstantiationContext` are both unchanged — the
  new wiring rides on `BusConfig`, which every module already threads through, so no existing
  module implementation or call site needs to change.
- The PIC's own two trait-object halves (`IoDevice` registered via `BusConfig::device()`,
  `VectorResolver` installed via `BusConfig::vector_resolver()`) will still need to share the
  PIC's *own* small, rarely-mutated config (vector table entries, priority mask) between them —
  that remains an implementation detail of the PIC itself, not something this pass needs to
  provide.

## Files to modify

- `src/emulator/bus/interrupt.rs` — `active_sources()`, `IrqSource` `Ord`/`PartialOrd`, new test.
- `src/emulator/cpu/vector.rs` — `VectorResolver::resolve` signature, `IdentityVectorResolver`, test.
- `src/emulator/cpu/mod.rs` — `pub` on the three vector constants; update the two `resolve(...)` call sites.
- `src/emulator/bus/mod.rs` — `BusConfig.vector_resolver` field, `.vector_resolver()` builder method, `.take_vector_resolver()`.
- `src/emulator/error.rs` — new `BusConfigError::DuplicateVectorResolver` variant.
- `src/emulator/config/emulator.rs` — `build_devices` takes the resolver off `bus_config` and chains it onto `CpuBuilder`.

## Verification

- `cargo build --workspace` — confirms the signature changes compile through the debugger crate.
- `cargo test --workspace` — existing `IdentityVectorResolver`, `InterruptController`, and
  `BusConfig` tests must still pass with updated call signatures; new tests should cover
  `active_sources()` (ascending order, multiple sources, empty when none active),
  `BusConfig::vector_resolver()`/`take_vector_resolver()` (installs, round-trips through
  `take_vector_resolver()`, and the "already installed" `DuplicateVectorResolver` conflict), and
  an integration-style `Config::build_devices` test with two device specs each attempting to
  install a resolver, asserting the second fails with `BuildError::Device { source: DeviceModuleError::BusConfig(BusConfigError::DuplicateVectorResolver), .. }`.
- `cargo clippy` — run after the edits, not just after the initial pass.
- All newly-`pub` items (`active_sources`, `BusConfig::vector_resolver`/`take_vector_resolver`,
  the three vector constants, `DuplicateVectorResolver`) need doc comments before committing.

## Follow-up (out of scope for this pass)

Building the actual PIC `IoDevice` + `VectorResolver` pair: local register storage (vector table,
priority/enable mask), the 8-slot priority encoding with sources 7..63 folded onto the lowest
slot as a wired-OR, and — separately — whether/how it becomes a config-driven built-in device
module (`DeviceRegistry::with_builtins()`, a `type=pic@...` spec) versus a hand-wired example.
