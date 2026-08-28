# Display/Keyboard Integration — Design & Implementation Plan

## Context

`CharDisplay` (`display/char`, `src/emulator/device/display/mod.rs`) currently streams composited
frames outbound only — over an in-process `mpsc` channel in the debugger (`attach_frame_sink`), or
over an external `PipeTransport` connected to a spawned `emma65-display` SDL2 child process for the
plain CLI (`attach_external_transport`; see `doc/char-display-external-protocol.md`). Keyboard
input is handled by a completely separate device, `Keyboard` (`src/emulator/device/keyboard.rs`,
`doc/memory-mapped-keyboard-device-plan.md`), which is debugger-only — the plain `emma65` CLI has
no way to feed it at all today.

This plan folds keyboard input directly into `CharDisplay`, for a concrete technical reason:
`PipeTransport` (`src/emulator/transport/pipe.rs`) is already bidirectional. It bridges the spawned
child's stdin (device→child, used today for frames) *and* stdout (child→device, completely unused
today — `src/emulator/config/display.rs:122` discards the inbound relay outright: `let (transport,
_relay) = ...`). Consuming that already-flowing, already-unused direction is what finally gives the
**plain CLI** keyboard input for the first time, by having `emma65-display` write captured
keystrokes back over the same pipe already connecting it to the emulator. In the debugger, folding
also collapses two parallel, near-identical device/transport-slot pairings (console+transport,
keyboard+transport) into one, and ties keyboard input to "does this display device support it"
rather than "is a separate `keyboard` device also configured."

## Architectural choices confirmed with the user

- **No back-compat.** `Keyboard` is deleted outright, not deprecated — existing profiles that use a
  standalone `keyboard` device (e.g. a `matrix` profile pairing it with `display/matrix`/`LedMatrix`)
  are updated by the user by hand. This plan makes no attempt at migration tooling or a compat shim.
- **Fold into `CharDisplay` specifically**, not a new shared device, because `InputBuffer`
  (`src/emulator/device/input_buffer.rs`) was already extracted from `Console`'s ring/latch/
  break-key/IRQ logic specifically so more than one device could share it — `Keyboard` was its first
  consumer, `Console` a planned future second one. `CharDisplay` becomes the real second (and, once
  `Keyboard` is deleted, only) consumer.
- **Restore `BusConfig::extend_device`** rather than inventing a new multi-range mechanism.
  `BusConfig::device()` (`src/emulator/bus/mod.rs`) currently allows exactly one `AddressRange` per
  registered device. An `extend_device(range, id)` builder that maps an *additional*, disjoint range
  onto an already-registered device's same `device_index` existed before commit `52951eee`
  ("implement CPU hot path optimizations") and was deleted incidentally during that refactor — its
  doc comment on `Region::Device` and `BusConfigError::UnknownDeviceId` (`src/emulator/error.rs`)
  are still present as dead leftovers referencing it. The multi-region-per-device data model was
  never actually removed, only the public entry point — restoring it (recoverable verbatim from
  `git show 52951eee^:src/emulator/bus/mod.rs`) is exactly the mechanism needed for `CharDisplay` to
  claim its framebuffer/control range plus a small, separately-configured 2-byte keyboard data/latch
  range elsewhere in the address space.

**A correctness issue this plan must account for, not just the new capability**: `PipeTransport`'s
inbound relay is backed by a bounded `crossbeam_channel` drained by a `ChannelRelay` background
thread. If nothing ever holds/drains that relay (today's situation — the config module drops
`_relay` immediately, and its receiving end closes with it), the *first* byte the child process ever
writes to its own stdout causes `run_pipe_task`'s `in_tx.send(buf[0])` to fail with a disconnected
channel, which today's code treats as `BrokenPipe`/"device channel closed" and tears down the
**entire transport — frames included, not just the keystroke**. Since this plan is the first thing
that will ever cause `emma65-display` to write to its own stdout, `CharDisplay` must hold onto the
inbound relay and drain it every `tick()` **unconditionally** — regardless of whether a keyboard
range is even configured for that device — forwarding into `InputBuffer` only when one is, and
discarding otherwise. This isn't a style choice; without it, the display pipeline breaks the instant
the peripheral sends its first keystroke.

## Design decisions

### 1. `CharDisplay` device core (`src/emulator/device/display/mod.rs`)

New sub-struct bundling the keyboard sub-range with its `InputBuffer` so the two can't drift apart:

```rust
struct KeyboardInput {
    range: AddressRange,
    input: InputBuffer,
}
```

New `CharDisplay` fields: `keyboard: Option<KeyboardInput>`, `keyboard_relay: Option<TransportRelay>`
(drained every `tick()` regardless of whether `keyboard` is set — see the correctness note above),
and `keyboard_transport: Option<Box<dyn Transport>>` (debugger path only — the CLI path's inbound
relay rides the same `PipeTransport` as `external_transport`, whose `shutdown()` already tears down
both directions of that one child's stdio).

New/changed methods, modeled directly on the deleted `Keyboard`'s own builder/attach methods:

- `with_keyboard_range(mut self, range: AddressRange) -> Self` — must be called before the device is
  boxed onto the bus, paired with a `BusConfig::extend_device` call at the same range/`DeviceId`.
- `set_break_key(&mut self, break_key: u8)` — no-op if no keyboard range configured.
- `attach_keyboard_transport(&mut self, transport: Box<dyn Transport>, relay: TransportRelay)` —
  debugger path, mirrors `Keyboard::attach_transport`.
- `attach_external_transport(&mut self, mut transport: Box<dyn Transport>, relay: TransportRelay)` —
  **signature change**: now also takes the transport's own inbound relay (previously discarded by
  the config module). Header-send behavior is unchanged; the relay is stored for `tick()` to drain
  unconditionally.

`read`/`write`/`peek`/`claims` each gain an early keyboard-range check *before* the existing
framebuffer-offset arithmetic (`address - self.address_range.start`) — required, not stylistic: if
the keyboard address is lower than the framebuffer's base, that subtraction underflows.

`tick()` drains `keyboard_relay` unconditionally (forwarding into `keyboard.input.push` when
`keyboard` is `Some`, discarding otherwise), then runs the existing vsync-cadence loop unchanged.
`reset()` also resets `keyboard.input` when present. A new `irq_active()` override delegates to
`keyboard.input.irq_active()` (the device was previously never IRQ-capable at all). `shutdown()`
also shuts down `keyboard_transport` when present, in addition to the existing
`external_transport.shutdown()` (which already covers the CLI keyboard path, since it's the same
transport).

`src/emulator/device/keyboard.rs` is deleted, along with its `pub mod`/`pub use` in
`src/emulator/device/mod.rs`. `input_buffer.rs`'s doc comment (which currently cross-references
`Keyboard`) is updated to reference `CharDisplay` instead.

### 2. Config wiring (`src/emulator/config/display.rs`)

New `CharDisplayAttributes` fields: `keyboard_address: Option<u16>`, `#[serde(rename = "break")]
break_key: Option<u8>`, `irq: Option<u32>`. Plain snake_case field names, matching this struct's
*existing* convention (`double_buffered`, `frame_rate_hz` — unlike `KeyboardAttributes`, this struct
has no `#[serde(rename_all = "kebab-case")]`, and adding kebab-case for only the new fields would be
inconsistent within the same struct without touching the whole config surface). `DEFAULT_KEYBOARD_IRQ:
u32 = 7`, reused verbatim from the deleted `KeyboardModule`.

`instantiate()` changes:

- Device-ID allocation becomes conditional: `next_available()` as today when `keyboard_address` is
  absent (unchanged, not IRQ-capable); `id_allocator.for_irq(config.irq.unwrap_or(DEFAULT_KEYBOARD_IRQ))`
  when it's present — required so `Bus::device_interrupt_states`/`InterruptController::poll_devices`
  actually notices this device's `irq_active()` (a plain `next_available()` ID falls outside the IRQ
  bitmask range and is silently ignored by `poll_devices`).
- After constructing `device`, if `config.keyboard_address` is `Some(addr)`:
  `device = device.with_keyboard_range(AddressRange::new(addr, addr + 1))`, then apply
  `config.break_key` via `set_break_key` if present.
- The `transport_spec` branch now captures the relay instead of discarding it and passes it to the
  widened `attach_external_transport(transport, relay)`.
- New block (only when `config.keyboard_address.is_some()`): consume `context.keyboard_transport`
  exactly the way `KeyboardModule` did (`.and_then(|slot| slot.lock().ok()?.take())`, bind the
  reporter, `device.attach_keyboard_transport(...)`). Gating on `keyboard_address` matters: an
  earlier `display/char` device with no keyboard configured must not consume and discard the
  debugger's only keyboard slot, starving a later device that does configure one.
- After `bus_config.device(address_range, device_id, Box::new(device))`, if `config.keyboard_address`
  is `Some(addr)`, also call `.extend_device(AddressRange::new(addr, addr + 1), device_id)`.

`InstantiationContext::keyboard_transport` itself is untouched — the debugger already unconditionally
builds and injects this slot in `load_session` regardless of any consumer; only the *consumer* moves
from `KeyboardModule` to `CharDisplayModule`. None of its ~16 existing construction sites need
touching.

`src/emulator/config/keyboard.rs` is deleted; its registration removed from
`DeviceRegistry::with_builtins()` (`src/emulator/config/registry.rs`) and its `mod`/`pub use` removed
from `src/emulator/config/mod.rs`. `InstantiationContext::keyboard_transport`'s doc comment (which
currently describes "the keyboard device module") is reworded to describe the display device's
keyboard sub-feature instead.

`doc/memory-mapped-display-device-spec.md` (the locked formal spec) is left unamended, with this plan
doc plus `CharDisplay`'s own module doc comment serving as the spec for the extension — matching the
precedent `doc/memory-mapped-keyboard-device-plan.md` itself set for `Keyboard`. A one-line
cross-reference is added at its top instead.

### 3. External protocol + `emma65-display` (`doc/char-display-external-protocol.md`, `display/src/main.rs`)

The protocol doc currently states explicitly (§2, §7) that the device consumes no inbound data at
all. Revise: §2 notes the connection is now used in both directions; a new section documents the
inbound stream — one byte per keystroke, no length prefix or framing (unlike the strict fixed-size
outbound frames), sent whenever the peripheral captures a key press, forwarded into `CharDisplay`'s
keyboard `InputBuffer` when a keyboard range is configured and silently discarded otherwise. Encoding
mirrors the scheme `debugger/src-tauri/src/keyboard.rs` already forwards from `DisplayPanel.tsx`'s
`keyboardByteForEvent` table (printable chars send their char code, `Enter`/`Backspace`/`Tab`/
`Escape` send standard ASCII control codes, `Ctrl+<letter>` sends `charCode(letter) - 64`). §7's "no
inbound messages" line is dropped; "no reconnection support, no protocol negotiation" stays.

`display/src/main.rs` gains keystroke capture in the existing main-thread SDL event loop (alongside
the existing `Event::Quit` check), writing single encoded bytes directly to its own `io::stdout()`.
Proposed mapping, refined against a live window during this unit's implementation the same way
`DisplayPanel.tsx`'s own table was originally settled: `Event::TextInput { text, .. }` for ordinary
printable characters (handles shift/layout correctly without a keycode table); `Event::KeyDown` for
`Return`/`Backspace`/`Tab`/`Escape`/`Ctrl+<letter>`, none of which also fire `TextInput` in SDL2, so
there's no double-send to guard against. No changes to `display/src/protocol.rs` — this is a wholly
separate, unframed stream on the same connection, unrelated to frame/header decoding.

### 4. Debugger backend (`debugger/src-tauri/src/`)

`keyboard.rs`'s `KeyboardTx`/`write_keyboard` move into `display.rs` (module deleted). Justification:
per `CLAUDE.md`'s own "one module per UI panel" convention, `keyboard.rs` never corresponded to its
own panel — it owns no window, no dock/detach lifecycle, nothing but the send side of a pipe serving
the Display panel specifically. The Tauri command name and signature are unchanged, and it's invoked
by bare string (`invoke("write_keyboard", ...)`) rather than scoped by Rust module path, so
**`debugger/frontend/src/DisplayPanel.tsx` needs no changes at all** — its keydown capture, encoding
table, and `write_keyboard` call already exist and already work generically.

`lib.rs`: `mod keyboard;` removed; `.manage(keyboard::KeyboardTx(...))` →
`.manage(display::KeyboardTx(...))`; `keyboard::write_keyboard` → `display::write_keyboard` in
`generate_handler!`. `load_session`/`load_or_reload_session`'s existing second
`InternalPipeTransport::pair()` call and its wiring are unchanged — only doc comments referencing "the
memory-mapped keyboard device plan" / "the (possibly absent) `keyboard` device" are reworded to point
at this plan and describe the display device's optional keyboard sub-range instead.

### 5. `doc/memory-mapped-keyboard-device-plan.md`

Gets a short "superseded by `doc/display-keyboard-integration-plan.md`" banner at the top rather than
being deleted, matching this repo's convention of keeping past design-decision docs as history (e.g.
`doc/memory-mapped-display-device-plan.md` still exists after later work built on it).

## Work Units

One branch + PR per unit, stop and await review after each. No GitHub issue tracks this work, same
as the display and keyboard device plans. Unit 0 is a direct commit to `main`, matching how the prior
two plan docs were landed.

- **0. Plan doc.** This document + superseded-banner edit to `doc/memory-mapped-keyboard-device-plan.md`
  + cross-reference note in `doc/memory-mapped-display-device-spec.md`.
- **1. Bus: restore `extend_device`.** `src/emulator/bus/mod.rs`: restore
  `BusConfig::extend_device(range, id) -> Result<Self, BusConfigError>`, recovered verbatim from
  `git show 52951eee^:src/emulator/bus/mod.rs` (looks up `device_index` by `id`, returns
  `BusConfigError::UnknownDeviceId` if not found, runs the same overlap check as `device()`, pushes an
  additional `Region::Device` sharing that `device_index`). Restore `device()`'s doc comment line
  pointing at `extend_device`. Recover the pre-`52951eee` tests for it verbatim, plus a new test
  proving both ranges dispatch reads/writes to the same device instance. Independent of the other
  units; can land first.
- **2. `CharDisplay` device core.** Design §1 above. Deletes `src/emulator/device/keyboard.rs`. Tests
  re-home every behavior from the deleted `Keyboard` test module (delegation, no-op data-register
  write, tick buffers input, break-key IRQ, reset, shutdown) plus a new test proving the relay is
  drained even with no keyboard range configured (the correctness fix). No config module changes yet —
  tests feed keyboard-range reads/writes/ticks directly, not through a real `BusConfig`.
- **3. Config wiring.** Design §2 above. Deletes `src/emulator/config/keyboard.rs`. Tests mirror the
  deleted `KeyboardModule`'s config-level suite against `CharDisplayModule`, plus a test proving
  `keyboard_address` absence doesn't consume the injected slot, and a test confirming both ranges are
  live on a real built `Bus` under one `DeviceId`. Depends on Units 1 and 2.
- **4. External protocol doc + `emma65-display`.** Design §3 above. Manually verified against a real
  spawned `emma65-display` process (no automated SDL2 test harness exists in this crate today).
- **5. Debugger backend fold.** Design §4 above. `cargo build --workspace` / `cargo clippy` to catch
  the module-path rename ripple; no frontend changes.
- **6. Manual verification.** `cargo tauri dev` checklist mirroring `doc/memory-mapped-keyboard-device-plan.md`'s
  own §5: type into the Display panel with `keyboard_address` configured (bytes arrive at the
  configured address); with it absent (no error); with `break=` configured (`Ctrl+C` asserts IRQ);
  detached-window case; session reload while typing. Plus a first-ever plain-CLI checklist: `emma65`
  + `display/char,keyboard_address=...,transport=pipe:.../emma65-display`, type into the SDL2 window
  and confirm bytes reach the emulator; same config with no `keyboard_address` and an extended typing
  session, confirming no stall in frame delivery (the correctness fix, under real conditions); closing
  the SDL2 window still behaves as before.

## Verification

- Unit 1: `cargo test` (bus module), `cargo clippy`.
- Unit 2: `cargo test` (`device::display`), `cargo clippy`.
- Unit 3: `cargo test` (`config::display`), `cargo clippy`.
- Unit 4: `cargo build -p emma65-display`; manual verification (no SDL2 test harness exists).
- Unit 5: `cargo build --workspace` / `cargo test --workspace`, `cargo clippy`.
- Unit 6: the manual checklists above, run by the user via `cargo tauri dev` and the plain `emma65`
  CLI.

## Explicitly out of scope

- Migrating `Console` onto `InputBuffer` (tracked separately, as in the original keyboard plan).
- Any migration tooling/compat shim for existing profiles using a standalone `keyboard` device — the
  user updates their own profiles by hand.
- A `display/char` device configuring both `transport=` and running under the debugger with a
  keyboard slot available in an unusual multi-device configuration — not a new class of problem,
  mirrors the existing "first one wins" precedent for multiple `console` devices.
