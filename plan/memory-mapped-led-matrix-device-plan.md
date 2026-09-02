# Memory-Mapped LED Matrix Device — Design & Implementation Plan

## Context

`plan/memory-mapped-led-matrix-device-spec.md` specifies this device's behavior from the 6502
program's point of view: configuration surface, bus-addressable memory map, command/data register
semantics, and per-matrix double-buffering/dirty-tracking/auto-refresh. It deliberately left open
the bus device trait shape, instantiation/wiring mechanics, the open questions originally raised in
its §8 (now resolved there directly, cross-referenced from design §6 below), and
the exact wire format of the transport protocol to an external companion process (its §7, §9) --
that last point explicitly deferred to "a follow-up document ... once this device-level contract
is settled."

This plan resolves the open items against Emma65's existing conventions -- most of which were
already established by `plan/memory-mapped-display-device-plan.md` for `CharDisplay`, whose
register-generalization, dirty/swap, and cycle-accounted-cadence patterns this device reuses almost
directly -- and lays out a phased implementation plan for the device itself, its compositing, and
the debugger's dockable/detachable panel. It does **not** cover the external wire protocol or the
SDL2 (or other) companion-process binary a real deployment would eventually want; per the spec's
own scoping and mirroring how `CharDisplay`'s external protocol and `emma65-display` binary were a
separate follow-on plan (the "SDL2 display peripheral plan") built only after `CharDisplay` existed
and was usable through the debugger, that work is left to an equivalent follow-on plan here. See
"Explicitly out of scope" below.

This device fully replaces the current register/blit-command `LedMatrix`
(`src/emulator/device/led_matrix.rs`) -- same config type string, same conceptual role on the bus,
completely incompatible register map and wire behavior. It is not a new device coexisting with the
old one.

## Design decisions

### 1. Bus device trait shape and module layout

No new trait. The device is a plain `IoDevice` implementation, keeping the name `LedMatrix`, the
same way `CharDisplay` needed no new trait either. Given the amount of device-specific machinery
(command/data state machine, per-matrix dirty tracking, compositing), it gets its own submodule
directory, mirroring `device/display/`'s structure instead of the current single-file
`device/led_matrix.rs`:

```
src/emulator/device/led_matrix/
  mod.rs           — LedMatrix: IoDevice impl, register map, command/data state machine,
                      dirty tracking, cycle-accounted auto-refresh
  compositing.rs   — pure fn: (pixel bytes, palette) -> RGBA byte buffer, plus the fixed
                      default 256-entry palette
```

`emulator::device` re-exports `LedMatrix` the same way it does today; the config type string stays
`display/matrix` (`LedMatrixModule::name()`), since this is a redesign of the same device slot, not
an additional one. No profile in this repo's bundled defaults configures `display/matrix` today
(confirmed by inspection of `emulator-template.toml`), so there is no default-config migration to
worry about -- only a breaking change for any user-authored config that used the old attributes,
which is expected and acceptable for a pre-1.0 emulator.

Unlike the current `LedMatrix`, this device is **not** IRQ-capable (spec §2: "No control/status
registers and no IRQ") -- swaps are synchronous, so there is nothing for a status bit or interrupt
to report. The config module allocates its `DeviceId` via `next_available()`, not `for_irq()`.

### 2. Config surface

The spec's §3 table lists `base_address` descriptively; it is not a separate attribute -- like
every other device, the base address is the standard `type@address` address already passed into
`DeviceModule::instantiate()`. The actual `LedMatrixAttributes` struct:

| Attribute | Type | Default |
|---|---|---|
| `matrices` | u32 | — (required); must be 1, 2, 4, or 8 |
| `frame_rate_hz` | u32 | `display::DEFAULT_FRAME_RATE_HZ` (60) — reused directly rather than a second constant with the same value |

No `palette=` attribute, unlike `CharDisplay`. Spec §2.1 fixes the device's initial palette as one
built-in default with no user-configurable source -- there was never a `palette` row in this
device's own §3 config table to begin with, and now that the transport doesn't transfer palette
contents at startup either (spec §7), there is no path by which a config-time custom palette could
even reach a companion process consistently. Runtime `CMD_PALETTE_WRITE` (§4.2) remains the only
way to change any entry, same as real LED matrix driver hardware.

No `transport` attribute in this plan -- with the external protocol and companion process deferred
(see Context), there is nothing yet for a transport to connect to. It's added in the follow-on plan
alongside the protocol itself, the same way `CharDisplay`'s `transport=` attribute was added in the
SDL2 display peripheral plan rather than in `memory-mapped-display-device-plan.md`.

**Validation at configuration time:** `matrices` must be one of `{1, 2, 4, 8}` (spec §3).

### 3. Palette storage: `Rgb565`, not `Rgb24`

Unlike `CharDisplay`, this device's palette entries are 16-bit RGB565 (spec §2) -- matching real
LED matrix driver hardware's actual color depth, not `CharDisplay`'s full RGB24. `Rgb24` is *not*
reused here; `led_matrix::compositing::Rgb565` is a new packed-`u16` type (5 bits red, 6 bits
green, 5 bits blue) with the constructor and conversion spec §4.2.1 requires:

```rust
impl Rgb565 {
    /// Packs already-5/6/5-bit components (each masked defensively with `& 0x1F`/`& 0x3F`).
    /// The building block both `from_rgb888` and `default_palette()` (design §4) are built on.
    fn new(r5: u8, g6: u8, b5: u8) -> Self { ... }
    /// Mask (spec §4.2.1): shifts each 8-bit component down to its native bit width, discarding
    /// low-order bits, then packs via `new`. Used by CMD_PALETTE_WRITE.
    fn from_rgb888(r: u8, g: u8, b: u8) -> Self { ... }
    /// Scale (spec §4.2.1): expands each stored component back to 8 bits by bit-replication (not
    /// a bare left-shift, which would fall short of 0xFF at the top of the range). Used by
    /// CMD_PALETTE_READ and by compositing.
    fn to_rgb888(self) -> (u8, u8, u8) { ... }
}
```

Both directions are implemented exactly once here and reused by every consumer (design §6, §10) --
the CPU-visible register behavior and the rendered pixels always agree on what a given `Rgb565`
value actually looks like, which is the whole point of specifying §4.2.1 precisely rather than
leaving masking/scaling to each call site.

### 4. Default palette: ported verbatim from spec §2.1

`led_matrix::compositing::default_palette()` builds spec §2.1's fixed 256-entry default directly in
RGB565 component space via `Rgb565::new` -- 16 named primary/secondary colors at half then full
intensity, a 6×6×6 color cube (`round(i * 31/5)`/`round(i * 63/5)` per lane), and a 24-step
grayscale ramp (`round(level * 31/23)`/`round(level * 63/23)`) -- **not** derived from an 8-bit
truecolor palette quantized down through `Rgb565::from_rgb888`, since the reference scheme's
component values (e.g. red's "half intensity" is literally `r5 = 15`, not `31 >> 1` reinterpreted
through an 8-bit round trip) only reproduce exactly when built directly at 5/6/5 precision. This
matters specifically because spec §7 no longer transfers the palette over the transport at
startup -- the device and the eventual companion process (out of scope here, but written against
this same spec) must each independently reconstruct a *bit-identical* result, so the generation
formula has to be followed exactly rather than approximated via any other color space.

### 5. Command/data register state machine

`CharDisplay`'s `PaletteUpdateState` enum hard-codes one fixed 4-byte sequence. This device needs a
handful of different sequence lengths (1, 2, or 4 write bytes; a 3-byte *read* sequence for
`CMD_PALETTE_READ`), so it generalizes to two small pieces of state instead of one bespoke enum per
command:

```rust
enum PendingOp {
    Idle,
    Write { command: Command, buffer: [u8; 4], filled: usize, expected: usize },
    Read { remaining: [u8; 3], next: usize },
}
```

Writing the command register (spec §4.2) always replaces `PendingOp` wholesale, regardless of
whatever was previously in progress -- re-issuing a command, including mid-sequence, resets the
state machine, matching the spec's "discarding whatever partial sequence was in progress." Writing
`CMD_PALETTE_READ` immediately resolves the palette entry at the given index, expands its stored
`Rgb565` back to three 8-bit bytes via `Rgb565::to_rgb888` (design §3, spec §4.2.1), and populates
a `Read` state with those as the 3 remaining channel bytes; each subsequent data-register *read*
pops the next byte. Writing the data register while `PendingOp::Write` is armed appends to `buffer`
and, once `filled == expected`, applies the command's effect and returns to `Idle` --
`CMD_PALETTE_WRITE`'s effect is `palette[index] = Rgb565::from_rgb888(red, green, blue)` (design
§3, spec §4.2.1), masking the three written bytes down as it stores them. All other reads/writes of
the data register (nothing armed, or past the expected count) are ignored/return 0, per spec §4.3.

### 6. Implementing spec §8's resolved questions

- **`CMD_SET_POWER` encoding**: 1 argument byte -- a power-state bitmask, bit *n* set meaning
  matrix *n* is powered on -- wholesale-replacing the persistent power-state mask exactly like
  `CMD_SET_AUTOREFRESH` replaces its own mask (spec §4.2), rather than a separate target-mask +
  on/off-value pair. This is both simpler (1 argument byte instead of 2) and more consistent with
  the command set's existing wholesale-mask precedent than a selective-toggle design would be; the
  trade-off is that changing one matrix's power without disturbing the others requires the CPU to
  already know (or track) the full current mask, since neither this command nor
  `CMD_SET_AUTOREFRESH` exposes a readback -- an accepted constraint since it already applies to
  auto-refresh today. Defaults to all matrices powered on at construction and after `reset()`,
  mirroring auto-refresh's own all-enabled default (design §7).
- **`CMD_SET_BRIGHTNESS` encoding**: 1 argument byte, `0..=255`, applied **globally** and
  uniformly to every attached matrix -- no bitmask, no per-matrix targeting. Confirmed: real LED
  matrix driver hardware commonly supports a single shared brightness/intensity setting across a
  chained panel array rather than per-panel brightness, so this mirrors what the emulated hardware
  would plausibly expose.
- **Command register read value**: stays fixed at 0, as the spec already leans toward -- no echo of
  the last-armed command code. Simplest option, and consistent with the register being described as
  "not meaningful" to read at all.

All three are implemented as pure state in this plan (they set/clear internal power/brightness
fields) with no observable effect anywhere yet -- see "Explicitly out of scope."

### 7. Dirty tracking and per-matrix swap

Since `matrices` is capped at 8, both the dirty flags and the auto-refresh mask fit in a single
`u8` bitmask each -- no `Vec<bool>` needed. `dirty` defaults to all bits set (masked to the
configured matrix count) at construction and after `reset()` (spec §4.1: an untouched matrix still
gets one initial swap); `autorefresh_mask` defaults to all-enabled, likewise masked (spec §6).
`CMD_SWAP` swaps every requested matrix unconditionally and clears its dirty bit regardless of
whether it was set; auto-refresh only swaps matrices that are both in `autorefresh_mask` and
currently dirty (spec §5.3). A swap, wherever triggered, copies that matrix's 1,024-byte
CPU-addressable buffer into its scanout buffer -- the same fixed-identity double-buffering model as
`CharDisplay` (spec §5.1), just per-matrix instead of per-device.

### 8. Auto-refresh cadence

Reuses `CharDisplay`'s cycle-accounted approach exactly: `cycles_per_frame` derived once at
construction from `clock_hz` (or a nominal fallback when the CPU runs unthrottled) and
`frame_rate_hz`; `tick(cycles)` accumulates and fires on each cadence crossing. Reuses
`display::NOMINAL_CLOCK_HZ` directly as that fallback rather than a duplicate constant -- the
rationale (no cycle-based computation can be wall-clock accurate under `ClockSpeed::unlimited()`)
applies identically here.

### 9. Compositing

A pure function, per matrix, with no font/glyph involved at all -- a much simpler job than
`CharDisplay`'s: `compositing::composite_matrix(pixels: &[u8], palette: &[Rgb565]) -> Vec<u8>` maps
each of the matrix's 1,024 palette-index bytes to an RGBA pixel (32×32×4 = 4,096 bytes), row-major,
via `palette[index].to_rgb888()` (design §3) -- so the debugger panel (and, later, the SDL2
companion process) renders the *same* quantized-then-expanded color a `CMD_PALETTE_READ` would
report, not the original pre-quantization write. This is what keeps rendered color fidelity
"reasonably comparable to the actual hardware" rather than accidentally showing more precision
than the emulated device actually has. Index resolution is `index as usize % palette.len()`,
matching the modulo rule `CharDisplay`'s compositing already established -- implemented as its own
one-line helper here rather than loosening the visibility of `display::compositing`'s existing
`pub(super)` `resolve_palette_index` for a single line of logic shared across an otherwise-unrelated
module.

### 10. Debugger frame delivery

`CharDisplay` recomposites and pushes its *entire* grid once per vsync because its double-buffering
is device-wide. This device's swap granularity is per-matrix (design §7), so recompositing and
resending all `matrices` buffers on every single swap would waste work when, say, auto-refresh only
actually swapped one of eight. Instead:

```rust
pub struct LedMatrixFrame {
    pub matrix_index: u8,
    pub pixels: Vec<u8>,   // RGBA, 32x32x4 bytes, from compositing::composite_matrix
}
```

One `LedMatrixFrame` is composited and pushed (via `mpsc::Sender::try_send`, same never-blocks
contract as `DisplayFrame`) per matrix actually swapped, whether by `CMD_SWAP` or auto-refresh.
`InstantiationContext` gains `led_matrix_frame_sink: Option<LedMatrixFrameSlot>` and
`led_matrix_geometry_sink: Option<LedMatrixGeometrySlot>`, mirroring `DisplayFrameSlot`/
`DisplayGeometrySlot` exactly. `LedMatrixGeometry { matrices: u32 }` is all the panel needs to size
itself before any frame arrives -- per-matrix dimensions are fixed at 32×32 by the spec, so unlike
`DisplayGeometry` there's no columns/rows/pixel-size to compute.

### 11. Debugger panel: one dockable/detachable panel, N canvases

One new panel (not one panel per matrix) containing `matrices` HTML canvases laid out in a single
horizontal row, left to right by index -- each matrix is an independent framebuffer on real
hardware, so independent canvases updated independently (keyed by `matrix_index` on each incoming
`led-matrix-frame` event) is a more direct mapping than compositing everything into one shared image
buffer whose layout would need to change with the configured matrix count. Follows the Terminal/
Display panels' established dockable/detachable pattern (`terminal.rs`/`display.rs`,
`layout.rs`, `menu.rs`) function-for-function:

- **Backend module** `debugger/src-tauri/src/led_matrix.rs`: `LedMatrixTargetWindow`, a bridge task
  forwarding `LedMatrixFrame`s as a `"led-matrix-frame"` event (payload includes `matrix_index`),
  `detach_led_matrix`/`attach_led_matrix`/`get_led_matrix_geometry` commands, detached-window
  install/restore.
- **Statically declared detached window**: `led-matrix-detached` entry in `tauri.conf.json`, hidden
  at startup like `display-detached`/`terminal-detached`.
- **Layout persistence**: `DockLayoutData` gains `led_matrix_detached: bool`.
- **Window menu**: a "Detach LED Matrix…"/"Attach LED Matrix" toggle in `menu.rs`.
- **Shutdown**: `LedMatrix::shutdown()` drops its frame sink, closing the bridge task's channel.

### 12. Frontend panel

New `debugger/frontend/src/LedMatrixPanel.tsx`: on mount, calls `get_led_matrix_geometry` to learn
`matrices` and render that many `<canvas>` elements in a row; listens for `"led-matrix-frame"` and
routes each payload's `pixels` to the canvas at `matrix_index` via `putImageData`. No compositing
logic in the frontend, matching `DisplayPanel.tsx`'s precedent. Registered in
`layout/panelRegistry.tsx` (`MainPanelId` gains `"ledMatrix"`, plus `PANEL_TITLES`/
`panelComponents` entries) with a `led-matrix-detached.tsx`/`.html` entry point mirroring
`display-detached.tsx`/`.html`.

## Work Units

One branch and PR per unit; stop after each and await review before starting the next.

### 1. Library device core

`src/emulator/device/led_matrix/mod.rs`: `LedMatrix` struct and `IoDevice` impl -- pixel memory
region, command/data register pair, the generalized `PendingOp` state machine (design §5) covering
every command in spec §4.2 including the §8 resolutions (design §6) and the RGB565 masking/scaling
of `CMD_PALETTE_WRITE`/`CMD_PALETTE_READ` (design §3, spec §4.2.1), per-matrix dirty tracking and
swap (design §7), cycle-accounted auto-refresh (design §8). Tests must cover the masking/scaling
round-trip explicitly (e.g. a written byte that isn't a multiple of 8/4 reads back changed, and
`0x00`/`0xFF` round-trip exactly), since spec §4.2.1 makes this CPU-observable behavior, not an
implementation detail. `frame_source(matrix_index)`-style accessor(s) returning a matrix's current
scanout buffer, used by Work Unit 2's compositing and by tests. Unlike the current `LedMatrix`,
this unit has **no** `Transport`/relay code at all -- external protocol wiring is deferred (see
Context) -- so tests are direct `read`/`write`/`peek` calls only, no pipe/relay harness needed, the
way `led_matrix.rs`'s current register tests already work minus the transport-facing half.

### 2. Compositing and default palette

`src/emulator/device/led_matrix/compositing.rs`: `Rgb565` and its `new`/`from_rgb888`/`to_rgb888`
functions (design §3, spec §4.2.1), `composite_matrix` (design §9), and `default_palette()` (design
§4, porting spec §2.1's exact scheme). Golden-pixel unit tests: known pixel-index/palette
combinations composited and asserted byte-for-byte; dedicated `from_rgb888`/`to_rgb888` unit tests
covering the range-boundary cases (`0x00`, `0xFF`, and at least one mid-range value per channel
width) called out in Work Unit 1; and a `default_palette()` test asserting specific entries against
spec §2.1's worked values (e.g. index 0 is black, index 9 is `Rgb565::new(31, 0, 0)`, index 255 is
the top of the grayscale ramp) so a transcription error in the ported formula is caught immediately
rather than only once a follow-up plan's companion process fails to match it.

### 3. Config module and registry wiring

Rewrite `src/emulator/config/led_matrix.rs`: `LedMatrixAttributes`/`LedMatrixModule` per design §2
(no `palette=` attribute), `matrices` validation, non-IRQ device ID allocation (design §1),
address-range sizing (`pixel_bytes + 2`), constructing the device with `compositing::default_palette()`
unconditionally. No `InstantiationContext` changes yet -- `led_matrix_frame_sink`/
`led_matrix_geometry_sink` stay unset at this point, same as `CharDisplay`'s equivalent config-only
unit; the device is configurable and instantiable, just has nothing to push frames to outside the
debugger.

### 4. Debugger backend integration

`InstantiationContext::led_matrix_frame_sink`/`led_matrix_geometry_sink` (design §10), debugger
setup code creating the channel and stashing the sender before bus construction (mirroring the
existing `display_frame_sink` wiring); `led_matrix.rs` (design §11): bridge task,
`LedMatrixTargetWindow`, `detach_led_matrix`/`attach_led_matrix`/`get_led_matrix_geometry` commands,
detached-window install/restore. `tauri.conf.json` gains the `led-matrix-detached` window
declaration. `DockLayoutData::led_matrix_detached`. `menu.rs`'s Window menu item.

### 5. Frontend integration

`LedMatrixPanel.tsx`, `led-matrix-detached.tsx`/`.html`, `panelRegistry.tsx` wiring,
`DockLayout.tsx` handling for detach/reattach events mirroring the existing
`display-detach-requested`/`display-reattached` handling. Decide during this unit (not before)
whether a keyboard accelerator is warranted for the detach toggle and how the per-matrix canvases
scale/space themselves as the panel's dock cell is resized -- UI details best settled while looking
at the running panel.

### 6. Manual verification

`cargo tauri dev`; drive the device by hand through the Memory panel (write pixel bytes into one or
more matrices' regions, write command/data register sequences for `CMD_SWAP`,
`CMD_SET_AUTOREFRESH`, and `CMD_PALETTE_WRITE`) since no demo program exists yet. Confirm: the
panel shows spec §2.1's default palette colors on first swap without any prior configuration;
docked and detached windows both receive per-matrix frames correctly after a detach/reattach cycle;
a profile switch or session reload tears down the old device's channel cleanly; configs with 1, 2,
4, and 8 matrices each render the right number of canvases; auto-refresh only redraws matrices that
are both enabled and actually dirty (toggling `CMD_SET_AUTOREFRESH` and confirming an untouched
matrix's canvas stops updating).

## Explicitly out of scope for this plan

- The external wire protocol (spec §7) and the `emma65-led-matrix` (or similarly named) SDL2
  companion-process binary (spec §9) -- a separate follow-on plan once this device exists and is
  usable through the debugger, mirroring how the SDL2 display peripheral plan followed
  `memory-mapped-display-device-plan.md` for `CharDisplay`. The `transport=` config attribute is
  part of that follow-on plan, not this one. That follow-on plan's companion process must
  reconstruct spec §2.1's default palette independently (design §4) rather than receiving it, since
  the header no longer carries palette contents.
- Any visible effect of `CMD_SET_POWER`/`CMD_SET_BRIGHTNESS` beyond internal state (design §6) --
  with no companion process yet and no dimming/power-off rendering designed for the debugger panel,
  there's nothing for these commands to visibly drive in this plan.
- Any demo 6502 program exercising the device.
