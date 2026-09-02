# Memory-Mapped Display Device — Design & Implementation Plan

## Context

`plan/memory-mapped-display-device-spec.md` specifies this device's behavior from the
6502 program's point of view: configuration surface, bus-addressable memory map, and
double-buffering/swap semantics. It deliberately left open the bus device trait shape,
instantiation/wiring mechanics, timing integration, and four explicit questions (§8).
This document resolves those against Emma65's existing conventions, adds the pieces the
spec was silent on (glyph rendering and how frames actually reach a screen), and lays
out a phased implementation plan.

This plan covers the device, its library-level compositing, and the debugger's
dockable/detachable display panel — the infrastructure needed to *use* the device from
the debugger. It does **not** cover the "digital rain" demo profile/program itself
(default-profile wiring, the demo 6502 program) — that's a separate follow-on plan built
on top of what's delivered here, tracked separately once this device exists and is
usable.

Two architectural choices were confirmed with the user before writing this plan:

- **Compositing (char RAM + color RAM + palette + glyph bitmap → RGB pixels) happens in
  the Rust backend**, not the frontend. The debugger's canvas is a dumb blit target
  (`putImageData`); no glyph or palette logic lives in JS. This matches the spec's
  VIC-II-style hardware framing and keeps rendering testable/reusable outside Tauri.
- **Frames reach the panel via a device-driven push channel**, not a fixed-interval
  backend poll. The device pushes a composited frame only when its own vsync/swap logic
  actually fires; the debugger's bridge task forwards it to whichever window (docked or
  detached) currently owns the panel. See "Frame cadence" below for why this is also
  the answer to the unlimited-clock-speed corner case.

## Design decisions

### 1. Bus device trait shape

No new trait. The device is a plain `IoDevice` implementation, `CharDisplay`, matching
every other built-in device (`R6551`, `Via6522`, `LedMatrix`, …). It is *not* IRQ-capable
— nothing in the spec's register map asserts an interrupt, so `irq_active()` stays at the
trait's default (`false`) and the device is allocated a plain `DeviceId` via
`DeviceIdAllocator::next_available()` rather than `for_irq()`.

Given the amount of device-specific machinery involved (registers, compositing, an
embedded font), this device gets its own submodule directory rather than a single file,
mirroring `device::protocol`'s structure instead of `device::led_matrix`'s single file:

```
src/emulator/device/display/
  mod.rs           — CharDisplay: IoDevice impl, register map, double-buffer/swap, vsync
  font.rs          — Font type (indexed 8x8 1bpp glyph bitmap), embedded default
  compositing.rs   — pure fn: (char_buf, color_buf, palette, font) -> RGBA byte buffer
```

`emulator::device` re-exports `CharDisplay` the same way it re-exports every other
built-in device today.

### 2. Palette index resolution (spec §4.1, open Q1)

Use `index % palette.len()` unconditionally — no power-of-two masking special case.
For any power-of-two length (16, 32, 64, 128, 256), `i % len` and `i & (len - 1)` are
bit-identical for all `i`, so masking was never actually a distinct rule — it was modulo
restricted to a subset of lengths. One rule, no branching, well-defined for every
configured palette length from 1 to 256.

### 3. Vsync-flag clear semantics (spec §4.3, open Q3)

**Read-to-clear**, on the status register itself: `read()` returns the current value
then clears bit 0; `peek()` returns the value without clearing (same read/peek split
`R6551` already uses for RDRF — `peek` never has side effects, full stop). Writes remain
ignored, as the spec already states.

This was chosen over VIA/PTM-style "write 1 to clear" because that pattern exists to let
software clear individual bits of a *multi-bit* register selectively; with exactly one
status bit here, read-to-clear is simpler and is exactly the operation a poll loop
(`LDA STATUS / AND #1 / BEQ loop`) already performs — no extra instruction needed to
acknowledge the flag.

### 4. Double swap-request idempotency (spec §5.2, open Q2)

Keep it idempotent-ignore, as the spec already leans toward. No dropped-frame/error
signal is added. A program that checks status bit 7 before writing bit 0 again (the only
reasonable way to hit this path deliberately) already has the information it needs; a
program that doesn't check it wouldn't consume a distinct error signal either. Revisit
only if real usage surfaces a concrete need.

### 5. Unmapped-address behavior within the device's range (spec §4.3, open Q4)

Moot by construction, not a rule to implement. The map is fully contiguous — char RAM +
color RAM + control + status = exactly `2*cells + 2` bytes with no gaps — so `claims()`
is just `address_range.contains(addr)`, identical to every other fixed-range device.
Anything outside the device's claimed range is already the bus's concern
(`UnmappedPolicy`, unaffected by this device).

### 6. Frame cadence: cycle-accurate device vsync + wall-clock-limited delivery

The spec asks for a single `on_vsync_tick`-equivalent hook but Emma65 devices have no
host-driven wall-clock hook at all — only `IoDevice::tick(cycles)`, called after each
instruction with the number of CPU cycles it took. This device needs no new hook: it
derives its own vsync entirely from cycle accounting against the CPU's configured clock
speed, the same technique `Mc6840` already uses for its timers (`tick_batch`/
`fast_forward` in `device/mc6840.rs`), just simpler (no prescaler/dual-8-bit modes to
consider — this is a fixed-rate free-running counter, not a programmable one).

At construction time the device computes `cycles_per_frame = clock_hz / frame_rate_hz`
(`frame_rate_hz` defaults to 60, `clock_hz` comes from `InstantiationContext::clock_hz`).
`tick(cycles)` accumulates cycles; when the accumulator crosses `cycles_per_frame` it
sets the vsync status bit and performs a pending swap if one was requested with
swap-on-vsync enabled (spec §5.3), exactly as specified.

**The unthrottled-clock case.** `InstantiationContext::clock_hz` is `None` when the CPU
runs at `ClockSpeed::unlimited()` — cycles then execute as fast as the host allows, with
no wall-clock correlation at all, so *no* cycle-based computation can produce a
wall-clock-accurate vsync in that mode. Rather than special-case the device around this,
cadence is split into two independent layers:

- **Device-side vsync** (cycle-accurate, CPU-visible): when `clock_hz` is `None`, fall
  back to a nominal reference rate (the default profile's own WDC65C02 clock,
  1.8432 MHz) purely so the status bit/timing stays plausible for a program polling it;
  document plainly that wall-clock accuracy is not guaranteed unless the emulator is run
  at a throttled clock speed, same as every other timing-sensitive thing in this
  emulator.
- **Delivery-side rate limiting** (wall-clock, host-visible, entirely independent of the
  above): the debugger's bridge task (Work Unit 4) never emits a `display-frame` event
  more often than roughly `frame_rate_hz` times per *real* second, dropping/coalescing
  extra frames if the device produces them faster — which happens whenever the CPU runs
  faster than real time, not just in unlimited mode. This is the layer that actually
  protects the UI, and it's needed regardless of clock throttling, so it isn't a
  workaround for the unlimited case — it's just correct.

### 7. Glyph bitmap source (new — not addressed by the spec)

The spec fixes `columns`/`rows`/`palette` at configuration time but says nothing about
where glyph *shapes* come from — "glyph index per cell" (§2) implies a font, and nothing
supplies one. This device needs one to render anything.

Treat it the same way the spec already treats the palette: an 8×8, 1bpp, 256-glyph
bitmap font (2048 bytes: 8 bytes per glyph, indexed by the same byte stored in char RAM),
fixed for the device's lifetime, supplied at configuration time with a bundled default
that's a plain `include_bytes!` constant (following the same embedding technique
`emulator::config::default` already uses for the bundled ROM/labels, but simpler — this
default is compiled into the device module directly, not materialized into a profile
directory, since a font/palette default is closer to "how this simulated hardware is
built" than to per-profile user content like the ROM image). Both the palette and the
glyph bitmap are supplied via file-path attributes (`palette=...`, `font=...`), not
inlined into the CLI `DeviceSpec` grammar — that grammar (`type@address,key=val,...`)
only supports scalar attribute values (see `config::device::parse_attributes`), and a
256-entry palette or a 2048-byte font can't be expressed as one. This exactly mirrors how
the default ROM is supplied via `image=<path>` rather than inline bytes.

**Obtaining the actual default font bitmap is an implementation task, not a design
decision** — a public-domain 8×8 bitmap font (e.g. a Codepage-437-style set) needs to be
sourced or hand-generated and checked in as a binary asset during Work Unit 2. Flagged
explicitly so it isn't missed or improvised late.

The C64 chargen ROM (`characters.901225-01.bin`) was considered and rejected as this
default: the "Commodore granted blanket permission" story that lets emulators like VICE
ship it is unwritten community folklore, not a documented license, and Cloanto actively
asserts copyright over the C64 ROM set today — too shaky a basis for a byte checked into
this MIT-licensed repo's history. A user who wants that look can still supply the real ROM
themselves via the `font=` attribute (design §8); it just isn't the bundled default.

**Chosen source: `font8x8` by Daniel Hepper** (github.com/dhepper/font8x8), public domain,
derived from Marcel Sondaar's `font8_8.asm` which in turn derives from IBM's public-domain
VGA font set — a clean provenance chain with no licensing ambiguity, and the de facto
standard 8×8 bitmap font reached for in OSDev/embedded/retrocomputing projects doing
VGA-text-mode-style rendering, i.e. exactly this device's use case. Row-encoded 8×8 1bpp
glyphs, matching the layout this device wants almost exactly.

Not all 256 glyph slots are populated by a single header, though — the project splits
glyphs across multiple headers (`font8x8_basic.h` for 0x00-0x7F, plus separate
`font8x8_ext_latin.h`, `font8x8_box.h`, `font8x8_greek.h`, `font8x8_hiragana.h`, etc. for
the upper range), and not every codepoint in 0x80-0xFF is covered by any one of them.
**Deciding which headers to combine into the bundled 256-glyph table (e.g. basic + ext_latin
+ box, leaving Greek/Hiragana/etc. out) is a Work Unit 2 decision**, best made while looking
at the actual glyph coverage each header provides, not speculatively here.

### 8. Config module and registry wiring

New `CharDisplayModule` in `src/emulator/config/display.rs`, registered under a new
device-type string `display/char` (parallel to the existing `display/matrix` for
`LedMatrix`) in `DeviceRegistry::with_builtins()`. Attributes, all following existing
`DeviceModule` deserialization conventions (`figment` extraction into a small
`#[derive(Deserialize)]` struct, as `LedMatrixAttributes` does):

| Attribute | Type | Default |
|---|---|---|
| `columns` | u32 | 40 |
| `rows` | u32 | 25 |
| `palette` | path | bundled default (16-entry) |
| `font` | path | bundled default (see §7) |
| `double_buffered` | bool | `true` |
| `frame_rate_hz` | u32 | 60 |

### 9. Frame delivery: injected channel, mirroring `console_transport`

The debugger needs to hold the *receiving* end of the device's frame-push channel before
bus construction runs, the same problem `console_transport` already solves for the
Console device's `PipeTransport`. Add a matching extension point to
`InstantiationContext` rather than inventing a new mechanism:

```rust
pub type DisplayFrameSlot = Arc<Mutex<Option<mpsc::Sender<DisplayFrame>>>>;

pub struct InstantiationContext {
    // ...existing fields...
    pub display_frame_sink: Option<DisplayFrameSlot>,
}
```

`DisplayFrame` is a small struct: the composited RGBA byte buffer plus `columns`/`rows`
(pixel dimensions are derivable but cheap to include for a self-describing message).
The debugger's setup code creates the channel, stashes the sender in the slot, and keeps
the receiver for its bridge task — exactly as it does today for the console pipe.
`CharDisplayModule::instantiate()` takes the slot's contents (if present) and calls a new
`CharDisplay::attach_frame_sink(sender)`, the same shape as `LedMatrix::attach_transport`.
When run outside the debugger (plain `emma65` CLI), the slot is simply absent and the
device composites nothing — no frame consumer, no wasted work, matching how
`console_transport` is `None` for the CLI today.

### 10. Debugger panel: dockable and detachable

Follows the Terminal panel's established pattern (`plan/debugger-terminal-architecture.md`,
`terminal.rs`, `layout.rs`) as closely as the two devices' different data shapes allow:
Terminal streams bytes through an OS pipe and a Tokio `AsyncFd` poll loop; this device
pushes pre-composited frames through an in-process `mpsc` channel — simpler, since there's
no OS-level byte stream or transport to bridge, just a channel-to-event forwarder.

- **Backend module** `debugger/src-tauri/src/display.rs`: owns `DisplayTargetWindow`
  (mirrors `TerminalTargetWindow`), a bridge task that receives `DisplayFrame`s and
  `emit_to`s a `"display-frame"` event to the current target window with wall-clock rate
  limiting (§6), and `detach_display`/`attach_display` commands plus
  `install_detached_window`/`restore_detached_window_if_needed` mirroring `terminal.rs`
  function-for-function. A `get_display_geometry` command (columns/rows/pixel
  width/height) lets the panel size its canvas correctly on mount, since those are fixed
  at configuration time and otherwise unknown to the frontend until the first frame
  arrives.
- **Statically declared detached window**: a new `display-detached` entry in
  `tauri.conf.json` (hidden at startup, shown/hidden rather than built/destroyed), same
  rationale as `terminal-detached` — avoids the whole-app freeze issue #385 hit with
  dynamic window construction.
- **Layout persistence**: `DockLayoutData` gains a `display_detached: bool` field
  alongside the existing `terminal_detached: bool` — a second dedicated field, not a
  generalized per-panel map, since two fields doesn't yet justify that refactor.
- **Window menu**: a "Detach Display…"/"Attach Display" toggle item in `menu.rs`'s
  `WindowMenuState`, mirroring the existing Terminal item exactly (`set_terminal_menu_label`
  gets a `set_display_menu_label` counterpart).
- **Shutdown**: `CharDisplay::shutdown()` drops its channel sender, which closes the
  bridge task's receiver and ends its loop — the channel equivalent of the terminal
  bridge seeing EOF on the pipe.

### 11. Frontend panel

New `debugger/frontend/src/DisplayPanel.tsx`: on mount, calls `get_display_geometry` to
size an HTML `<canvas>`, then listens for `"display-frame"` and does
`ctx.putImageData(new ImageData(new Uint8ClampedArray(bytes), width, height), 0, 0)` —
no glyph or color logic in the frontend at all, per the compositing decision above.
Registered in `panelRegistry.tsx` (`MainPanelId` gains `"display"`, plus `PANEL_TITLES`
and `panelComponents` entries) and a `display-detached.tsx`/`display-detached.html`
entry point mirroring `terminal-detached.tsx`/`.html` for the detached window. No
`dockPanelApi` threading in this first pass — the canvas has a fixed intrinsic pixel size
driven by device config, not a user-resizable content area the way Terminal's
size-preset menu is; CSS scaling/letterboxing behavior inside a resized dock cell is a
Work Unit 5 detail, not an architectural one.

## Work Units

One branch and PR per unit; stop after each and await review before starting the next.

### 1. Library device core

`src/emulator/device/display/mod.rs`: `CharDisplay` struct and `IoDevice` impl —
register map (§4 of the spec: char RAM, color RAM, control, status), double-buffer
copy-on-swap (spec §5.1), cycle-accounted vsync (design §6 above, nominal-rate fallback
included), read-to-clear status bit (design §3), idempotent swap requests (design §4).
`frame_source()` accessor returning the (char, color) buffer pair currently intended for
scanout (spec §6), used by Work Unit 2's compositing and by tests. No compositing, no
config module, no debugger wiring yet — this unit is bus-facing behavior only, tested the
way `led_matrix.rs`/`r6551.rs` test their register semantics (direct `read`/`write`/`peek`
calls, no bus needed).

### 2. Compositing and default glyph font

`src/emulator/device/display/font.rs` (`Font` type: parse/hold an 8×8 1bpp 256-glyph
bitmap, embedded default via `include_bytes!` — **assembling the actual default font asset
from `font8x8`, including the header-selection decision, is part of this unit**, see design
§7) and `compositing.rs` (pure function:
cell buffers + palette + font → RGBA byte buffer, using the modulo palette-resolution
rule from design §2). Golden-pixel unit tests: a handful of known glyph/color
combinations composited and asserted byte-for-byte.

### 3. Config module and registry wiring

`src/emulator/config/display.rs`: `CharDisplayModule`, attribute parsing per design §8
(columns/rows/palette path/font path/double_buffered/frame_rate_hz), registered as
`display/char` in `DeviceRegistry::with_builtins()`. No `InstantiationContext` changes
yet — `display_frame_sink` stays `None` at this point, same as running the CLI emulator
today; the device is configurable and instantiable, just has nothing to push frames to
outside the debugger.

### 4. Debugger backend integration

`InstantiationContext::display_frame_sink` (design §9), debugger setup code that creates
the channel and stashes the sender before bus construction; `display.rs` (design §10):
bridge task, `DisplayTargetWindow`, rate-limited `display-frame` emission,
`detach_display`/`attach_display`/`get_display_geometry` commands, detached-window
install/restore. `tauri.conf.json` gains the `display-detached` window declaration.
`DockLayoutData::display_detached`. `menu.rs`'s Window menu item.

### 5. Frontend integration

`DisplayPanel.tsx`, `display-detached.tsx`/`.html`, `panelRegistry.tsx` wiring,
`DockLayout.tsx` handling for detach/reattach events mirroring the existing
`terminal-detach-requested`/`terminal-reattached` handling. Decide during this unit
(not before) whether a keyboard accelerator is warranted for the detach toggle, and how
the canvas behaves when its dock cell is resized (scale vs. letterbox) — both are UI
details best settled while looking at the running panel, not speculatively here.

### 6. Manual verification

`cargo tauri dev`; drive the device by hand through the Memory panel (write char/color
cells, toggle the control register, confirm the display panel updates) since no demo
program exists yet. Confirm: docked and detached windows both receive frames correctly
after a detach/reattach cycle; a profile switch or session reload tears down the old
device's channel cleanly (bridge task ends, no stale frames delivered to a new session's
panel); single-buffered mode (`double_buffered=false`) still updates the panel with swap
requests as no-ops per spec §5.1.

## Explicitly out of scope for this plan

- The "digital rain" demo profile and 6502 program — a separate follow-on once this
  device exists and is usable standalone.
- C64-compatible packed color mode (spec §7) — already out of scope in the spec itself.
- Any font-authoring/editing UI — the glyph bitmap is a fixed configuration input, like
  the palette.
