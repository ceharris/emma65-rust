# Memory-Mapped Keyboard Input Device — Design & Implementation Plan

## Context

The recently-shipped `CharDisplay` device (`display/char`, PRs #496-500) is output-only and
debugger-only — nothing in the plain `emma65` CLI ever populates its `display_frame_sink`. A
config built around `display/char` with no `console` currently has no way to receive input at
all. This plan adds a companion **keyboard input device** so such a config can receive input,
routed from the debugger's already-shipped Display panel/window
(`debugger/frontend/src/DisplayPanel.tsx`, `debugger/src-tauri/src/display.rs`).

Unlike the display device, there is no separate spec document — the keyboard device's behavior
is simple enough, and close enough to `Console`'s already-shipped, well-tested input half, that
this single plan doc covers both what a spec would (register map, semantics) and how it gets
built.

Architectural choices confirmed with the user before writing this plan:

- **Separate device**, not folded into `Console`. `Console` conflates an output side (writes
  forwarded to a transport peer) with an input side (ring + latch + break key); a keyboard is
  input-only and has no peer to write to.
- **Shared input-buffer support module**, extracted from `Console`'s existing ring/latch/break-key
  logic, consumed by the new `Keyboard` device from day one — in anticipation of a **future,
  separate** effort that refactors `Console` itself onto the same module. That refactor is
  explicitly **not** part of this plan; `Console` is not touched here.
- **Debugger routing**: keyboard input events from the Display panel/window route to the keyboard
  device if one is configured; if none is configured, those events are simply dropped (no error
  surfaced to the user).
- **Transport attachable for testing**, but the config module exposes no `transport=` TOML/CLI
  attribute — only `break=` and `irq=`. The device-level `attach_transport` capability exists for
  direct library tests and for the debugger's injected-slot wiring, never for user-configured
  peers.
- **Debugger-only** — no CLI/stdin wiring for the keyboard device in this plan (`CharDisplay`
  itself has no CLI presence, so there's nothing for CLI keyboard input to pair with yet).

## Design decisions

### 1. Shared input-buffer module

New file `src/emulator/device/input_buffer.rs`, declared `pub(crate) mod input_buffer;` in
`src/emulator/device/mod.rs` (sibling to the existing private `mod ring;` — `pub(crate)` because,
unlike `ring`, two different device modules eventually need to reach it: `keyboard.rs` now,
`console.rs` in the future follow-up). There's no existing "shared"/"common" convention under
`device/` to follow here — the closest precedent is `device::protocol`, named after what it does
rather than after being shared; this module follows the same naming style.

Transport-agnostic: no `Transport`/`TransportRelay` field of its own. The owning device drains its
own relay and calls `push(byte)`; this type only decides what happens to a byte once it has one.
(`device::lfsr::Lfsr16` is existing proof that `IoDevice::tick` carries no inherent transport
assumption — a device can tick purely on internal state with no external byte source at all.)

```rust
pub(crate) struct InputBuffer {
    ring: Ring<u8>,
    latch: u8,
    break_key: Option<u8>,
    interrupt_flag: bool,
}

impl InputBuffer {
    pub fn new() -> Self
    pub fn set_break_key(&mut self, break_key: u8)
    pub fn read_data(&mut self) -> u8      // offset-0 semantics (below)
    pub fn read_latch(&mut self) -> u8     // offset-1 semantics (below)
    pub fn write_latch(&mut self, value: u8)
    pub fn peek_data(&self) -> u8
    pub fn peek_latch(&self) -> u8
    pub fn push(&mut self, byte: u8)       // one byte drained from a relay by the owning device
    pub fn reset(&mut self)                // no logging — owning device logs with its own identity
    pub fn irq_active(&self) -> bool
}
```

This is a direct 1:1 extraction of `Console`'s current inline logic (`src/emulator/device/console.rs`):
`read(0)`/`read(1)`/`write(1,_)`/`peek(0)`/`peek(1)`/`tick`'s closure body/`reset`/`irq_active` map
onto `read_data`/`read_latch`/`write_latch`/`peek_data`/`peek_latch`/`push`/`reset`/`irq_active`
respectively. `write(0, value)`'s transport-send stays entirely in `Console` — there's nothing
generic about "send to my attached transport."

Tested directly via plain method calls (no `Transport`/`ChannelRelay` scaffolding needed at this
layer): latch/ring precedence on read, interrupt-flag clear on either register read, break-key
detection and ring-clear on both `write_latch` and `push`, tail-drop behavior on a full ring,
`reset` clearing all state.

### 2. `Keyboard` device (`src/emulator/device/keyboard.rs`)

```rust
pub struct Keyboard {
    name: &'static str,
    address: u16,
    transport: Option<Box<dyn Transport>>,
    relay: Option<TransportRelay>,
    input: InputBuffer,
    log_sender: LogSender,
}
```

Builder methods mirror `Console`'s exactly: `new(name)`, `with_address(address)`,
`attach_transport(transport, relay)`, `set_break_key(key)`, `set_log_sender(sender)`.

Register map — same two-register shape as `Console`, `BUS_SIZE = 2`:

| Offset | Name  | Read                    | Write                                                        |
|--------|-------|-------------------------|---------------------------------------------------------------|
| 0      | Data  | `input.read_data()`     | **no-op** — this device has no outbound byte stream           |
| 1      | Latch | `input.read_latch()`    | `input.write_latch(value)`                                    |

`peek()` delegates to `input.peek_data()`/`input.peek_latch()`, no side effects. `tick(_cycles)`
drains `self.relay` (if present) via `relay.drain_bytes_into(|b| self.input.push(b))` — one line,
versus `Console`'s current inline closure, since all break-key/ring/latch logic now lives in
`InputBuffer`. `reset()` calls `input.reset()` then logs via `log_msg!` with `self.identity()`,
matching `Console::reset()`. `irq_active()` delegates to `input.irq_active()`. `name()`/
`identity_address()`/`shutdown()` (calls `transport.shutdown()` if present, for symmetry and
testability) are copied from `Console`'s.

The module doc comment explicitly notes the Data-register write is an intentional no-op — this
device is input-only, conceptually the input half of the console pattern, meant to pair with
`display/char` (output-only).

Device-type string: **`"keyboard"`** — flat, like `"console"`, not namespaced under `display/`
(there's no existing precedent for an input device living under that prefix, and it's conceptually
a sibling of `console`, not a display variant).

Tests mirror `Console`'s suite, minus everything `InputBuffer`'s own tests already cover: thin
read/write/peek delegation checks, `tick_buffers_input_from_transport`/`tick_latches_break_key_...`
using the same `InternalPipeTransport::pair_direct()` + hand-fed `ChannelRelay` harness `Console`'s
tests already establish, `reset_logs_device_message`, and a `write_data_register_is_noop` test (no
`remote.try_recv()` to check, so it just asserts no panic and no state change). No output-side
integration test (nothing is ever written out); keep an input-side integration test mirroring
`Console`'s `integration_transport_input_readable_by_cpu`.

### 3. `KeyboardModule` config (`src/emulator/config/keyboard.rs`)

Modeled on `ConsoleModule`, with the `transport=` attribute and its `TransportSpec` branch removed
entirely — the injected-slot consumption is the *only* path to a transport:

```rust
const BUS_SIZE: u16 = 2;
const DEFAULT_IRQ: u32 = 7;

#[derive(Clone)]
pub struct KeyboardModule;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct KeyboardAttributes {
    #[serde(rename = "break", skip_serializing_if = "Option::is_none")]
    break_key: Option<u8>,
    irq: Option<u32>,
}
```

`instantiate()` parses attributes via the same figment pattern as `ConsoleAttributes`, allocates a
`DeviceId` via `id_allocator.lock().unwrap().for_irq(irq)?` (this device is IRQ-capable, same as
`Console`'s break-key interrupt), consumes `context.keyboard_transport` exactly the way
`ConsoleModule` consumes `context.console_transport` today (`.and_then(|slot| slot.lock().ok()?.take())`,
`reporter.bind(dev.identity())`, `dev.attach_transport(...)`), sets `break_key`/`log_sender` if
configured, and calls `bus_config.device(...)`.

**`DEFAULT_IRQ = 7`**: verified every existing default — `via/6522`=1, `mc6840`=2, `console`=3,
`mc6850`=4, `r6551`=5, `display/matrix` (LedMatrix, which *is* IRQ-capable, unlike `display/char`)=6.
7 is the next free slot in that sequence, consistent with how every other built-in device picked
its default. (`MAX_IRQ_SOURCES = 64`; the debugger separately reserves `MAX_IRQ_SOURCES - 1` for
its own UI-driven IRQ toggle — 7 is nowhere near that.)

`KeyboardAttributes` has no `transport` field; since nothing in this config layer uses
`#[serde(deny_unknown_fields)]`, a stray `transport=` attribute on a `keyboard` device entry is
silently ignored rather than rejected — consistent with existing behavior elsewhere in this layer,
worth one doc-comment note but not a functional gap.

Registration: `mod keyboard;` + `pub use keyboard::KeyboardModule;` in `src/emulator/config/mod.rs`;
`r.register(KeyboardModule);` added to `DeviceRegistry::with_builtins()`
(`src/emulator/config/registry.rs`).

### 4. `InstantiationContext::keyboard_transport`

Add `pub keyboard_transport: Option<TransportSlot>` to `InstantiationContext`
(`src/emulator/config/registry.rs`) — reuses the existing `TransportSlot` type verbatim (keyboard
input, like console I/O, is a plain byte stream; no new slot type needed).

There is no `Default` impl for `InstantiationContext`; every construction site lists all fields
explicitly. Verified by grepping `InstantiationContext {` across the whole repo — **16 literal
constructions need `keyboard_transport` added** (re-grep at implementation time in case something
else lands first):

| File | Sites |
|---|---|
| `src/emulator/config/console.rs` (test fns) | 5 |
| `src/emulator/config/registry.rs` (own test module) | 6 |
| `src/emulator/config/pic_finch.rs` (test helper) | 1 |
| `tests/config_integration.rs` (test helper) | 1 |
| `src/emulator/config/emulator.rs` — `Config::build()` | 1 |
| `src/bin/emulator/main.rs` — real CLI construction | 1 (`keyboard_transport: None`, the concrete expression of "no CLI wiring") |
| `debugger/src-tauri/src/lib.rs` — `load_session()` | 1 (needs a *real* slot, see §5) |

`Config::build_with_context()` (`emulator.rs`) needs **no change** — it already builds its context
via struct-update syntax (`InstantiationContext { error_sender: ..., ..context }`), so it picks up
whatever the caller passed automatically.

### 5. Debugger backend wiring

New `debugger/src-tauri/src/keyboard.rs`, mirroring only the `TerminalTx`/`write_terminal` slice of
`terminal.rs` — none of its window dock/detach lifecycle, since keyboard input has no window of
its own (it rides along on the Display panel/window that already exists):

```rust
pub struct KeyboardTx(pub Mutex<Option<File>>);

#[tauri::command]
pub fn write_keyboard(bytes: Vec<u8>, state: State<KeyboardTx>) -> Result<(), String> {
    let mut guard = state.0.lock().unwrap();
    let tx = guard.as_mut().ok_or("Keyboard not ready")?;
    tx.write_all(&bytes).map_err(|e| e.to_string())
}
```

`load_session()` gains a second, independent `InternalPipeTransport::pair(reporter)` — built and
injected exactly like the console one already there — and its return tuple grows by one element
(the keyboard remote pipe end). `load_or_reload_session()` resets `KeyboardTx` to `None` on reload
and, on success, splits the keyboard remote pipe and stores the tx half:

```rust
let (_kbd_remote_rx, kbd_remote_tx) = kbd_remote.into_split();
*app.state::<keyboard::KeyboardTx>().0.lock().unwrap() = Some(kbd_remote_tx);
```

The rx half is intentionally dropped: `into_split()` (`src/emulator/transport/internal_pipe.rs`)
returns two independently-`try_clone()`d `File`s with no coupling requiring both to stay alive, and
nothing ever writes to that pipe from the emulator side — `Keyboard::write(0, _)` never calls
`transport.send()`, by design (§2). Add a one-line comment at the drop site explaining this.

`mod keyboard;`, `.manage(keyboard::KeyboardTx(Mutex::new(None)))`, and
`keyboard::write_keyboard` in `generate_handler!` — added in `lib.rs` alongside the terminal
equivalents. No `KeyboardTargetWindow`: unlike `TerminalTargetWindow`/`DisplayTargetWindow`, there's
no outbound `emit_to` to retarget between docked/detached windows — `write_keyboard`'s effect is
the same regardless of which window the invoke came from.

**`KeyboardTx` is populated on every successful session load, regardless of whether the profile's
config actually has a `keyboard` device** — mirroring exactly how `console_transport`/`TerminalTx`
work today for `console`. If no `keyboard` device is configured, nothing ever consumes the
injected slot, and writes into that pipe are simply absorbed by OS pipe buffering with no visible
effect — a different mechanism than `write_keyboard` returning `Err`, but the same result from the
user's perspective (silently dropped), and it avoids `load_session` needing to know ahead of time
whether a `keyboard` device is configured. No extra "is a keyboard configured" plumbing is added.

**Multiple `keyboard` devices in one config**: nothing prevents this (only IRQ collisions are
checked). As with `console_transport` today for multiple `console` devices, exactly one — whichever
`keyboard` entry instantiates first — consumes the injected slot; later ones simply get no
transport attached (silently receive no input). This mirrors existing behavior, not a new class of
problem, and isn't validated against here.

### 6. Frontend wiring — `DisplayPanel.tsx`

New surface — `DisplayPanel.tsx` currently has zero keyboard/focus handling (no `tabIndex`, no key
listeners; the canvas isn't even DOM-focusable as written).

- Canvas gains `tabIndex={0}` to become focusable.
- A `keydown` listener, gated first against `APP_KEY_BINDINGS.some(b => b.matches(e))`
  (`useAppKeyBindings.ts`) so `Ctrl+Shift+T` (reveal/reattach Terminal) and `Ctrl+Shift+D`
  (reveal/reattach *this* Display panel) always pass through uncaptured — mirroring
  `TerminalPanel.tsx`'s `attachCustomKeyEventHandler` guard.
- Encoding, kept deliberately small and principled rather than an ad hoc list: printable
  single-character keys (`e.key.length === 1`) send their char code directly; `Enter`→`0x0D`,
  `Backspace`→`0x08`, `Tab`→`0x09`, `Escape`→`0x1B`; and **`Ctrl+<letter>` → `charCode(letter) - 64`**
  (the standard ASCII control-code derivation) rather than hardcoding "Ctrl+C → break key" —
  this one rule produces `0x03` for Ctrl+C for free, plus every other control code, with no
  break-key-specific logic in the frontend at all (the backend's `break=` attribute is what gives
  any particular byte break-key significance). `e.preventDefault()` on every forwarded key; bare
  modifier keydowns and `APP_KEY_BINDINGS` matches pass through untouched.
- `invoke("write_keyboard", { bytes }).catch(() => {})` — the existing fire-and-forget pattern,
  which is also exactly the mechanism satisfying "drop when no keyboard device configured" (no
  separate check needed anywhere in the frontend).
- **No auto-focus on mount**, unlike Terminal's `term.focus()`. If Console and `display/char` are
  both configured (a plausible layout), unconditional auto-focus on Display would nondeterministically
  steal focus from Terminal depending on mount order. Focusable via `tabIndex`, focused only by
  explicit user click.
- `keydown` only, repeats **allowed through unfiltered** — matches ordinary physical-keyboard
  typematic-repeat behavior with no extra repeat-tracking state.
- No changes needed to `display-detached.tsx` — it already renders the shared `DisplayPanel`
  component, so this is picked up by both docked and detached hosts for free.

The exact encoding table is a starting point, refined against the actual running panel during this
unit's own review — matching the display plan's own precedent of settling UI details while looking
at the panel rather than speculatively up front.

### 7. `Console` is untouched by this plan

`src/emulator/device/console.rs` and `src/emulator/config/console.rs` are read-only references —
never edited here. Migrating `Console` onto `InputBuffer` is tracked as a future follow-up, made
possible (not required) by this plan.

## Work Units

One branch + PR per unit; stop after each and await review before starting the next.

### 1. Shared input-buffer module + `Keyboard` device library core

`src/emulator/device/input_buffer.rs` (§1) and `src/emulator/device/keyboard.rs` (§2), plus
`pub mod keyboard;` / `pub use self::keyboard::Keyboard;` in `src/emulator/device/mod.rs`. No
config module, no `InstantiationContext` changes, no debugger wiring yet — purely bus-facing
behavior, tested the way `Console` and `Lfsr16` test themselves (direct `read`/`write`/`peek`/`tick`
calls, plus the `InternalPipeTransport::pair_direct()` + hand-fed-`ChannelRelay` harness already
established by `Console`'s tests). `InputBuffer` and `Keyboard` are kept in one unit rather than
split further — `InputBuffer` has no independently observable behavior until something consumes
it, and `Keyboard` is a thin enough wrapper that reviewing them apart mostly adds ceremony.

### 2. Config module and registry wiring

`src/emulator/config/keyboard.rs` (§3), registered as `"keyboard"`, plus `mod keyboard;` /
`pub use keyboard::KeyboardModule;` in `src/emulator/config/mod.rs`. Adds
`InstantiationContext::keyboard_transport` (§4) and touches every site in the table above except
`debugger/src-tauri/src/lib.rs` — the debugger's *real* production wiring is deliberately left to
Work Unit 3, since it needs the `keyboard.rs` backend module (`KeyboardTx`) to exist first, and
keeping "config plumbing" (mechanical, low-risk) separate from "debugger session wiring" (real
runtime behavior) makes both easier to review. Tests mirror `ConsoleModule`'s existing suite,
minus the transport-spec-attribute case (there is none).

### 3. Debugger backend integration

`debugger/src-tauri/src/keyboard.rs` (`KeyboardTx`, `write_keyboard`, §5), `load_session()`'s
second `InternalPipeTransport::pair()` call plus the widened return tuple, `load_or_reload_session()`'s
reset-on-reload and success-path wiring (including the confirmed-safe unused-rx-half drop), and
`mod keyboard;` / `.manage(...)` / `generate_handler!` additions in `lib.rs`. No frontend changes
yet — at the end of this unit, `write_keyboard` is a fully working, testable Tauri command with no
UI caller.

### 4. Frontend integration

`DisplayPanel.tsx`'s keydown capture (§6): `tabIndex`, the `APP_KEY_BINDINGS` bypass, the encoding
table (refined against the running panel), `invoke("write_keyboard", ...)`. No changes to
`panelRegistry.tsx`, `display-detached.tsx`, or any dock/menu/window-lifecycle code — the Display
panel and its detached window already exist and already route focus into whichever window
currently hosts them; this unit only adds behavior inside the existing component.

### 5. Manual verification

`cargo tauri dev` checklist:

- `display/char` + `keyboard` (no `console`): click into the Display panel, type — confirm bytes
  arrive at the keyboard device (Memory panel reading the Latch register address, or a small 6502
  program that echoes received bytes onto the display's char RAM).
- `display/char` only (no `keyboard` device): confirm keydown events in the Display panel produce
  no visible error — silently dropped.
- `Ctrl+Shift+T` / `Ctrl+Shift+D` while the Display panel has keyboard focus: confirm they still
  reveal/reattach their respective panels rather than being sent to the keyboard device as bytes.
- Break-key config (`keyboard@addr,break=3`): confirm `Ctrl+C` in the Display panel asserts the
  keyboard device's IRQ (CPU/Bus panel's IRQ line state), matching `Console`'s existing behavior.
- Detach the Display window, click into it, type — confirm keyboard routing still works there
  (proves `write_keyboard`'s effect is independent of which window the invoke came from).
- A session reload/profile switch while typing is in flight: confirm no panic, no stale-session
  bytes delivered to the new session's keyboard device.

## Explicitly out of scope for this plan

- CLI/stdin wiring for the keyboard device — `main.rs`'s `InstantiationContext` literal explicitly
  sets `keyboard_transport: None` as the concrete expression of this.
- Refactoring `Console` to consume `InputBuffer`.
- Any special routing for multiple `keyboard` devices in one config beyond "first one wins,"
  mirroring existing `console_transport` behavior.
- Scancode/hold semantics (key-up events, N-key rollover) — this is a one-byte-per-keypress latch,
  not a scancode-based controller; only `keydown` matters.
- IME/unicode input beyond basic ASCII — the encoding table only covers ASCII-range keys and a
  handful of named control keys.
- Any change to `Console`'s device-type string, register map, or behavior.
- A formal `memory-mapped-keyboard-device-spec.md` — this plan doc serves that role directly.

## Verification

- **Unit 1**: `cargo test` (targeted: `device::input_buffer`, `device::keyboard`), `cargo clippy`.
- **Unit 2**: `cargo test` (targeted: `config::keyboard`), plus extending `tests/config_integration.rs`
  if it already exercises `ConsoleModule` similarly (check at implementation time). `cargo clippy`
  across touched files — an `InstantiationContext` shape change is exactly the kind of edit that
  silently breaks unrelated call sites without a compiler-checked pass.
- **Unit 3**: `cargo build --workspace` / `cargo test --workspace` to catch the widened
  `load_session` return-tuple ripple; `cargo clippy` (covers the debugger crate too).
- **Units 4-5**: manual `cargo tauri dev` checklist above — no automated frontend test exists for
  `TerminalPanel.tsx`'s equivalent key-capture logic today, so this plan doesn't introduce one for
  Display either; manual verification is the established precedent here.
