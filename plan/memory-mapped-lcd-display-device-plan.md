# Memory-Mapped LCD Display Device — Design & Implementation Plan

## Context

`plan/memory-mapped-lcd-display-device-spec.md` specifies this device's behavior from the 6502
program's point of view: its two-register interface, instruction set and timing, 4-bit/8-bit
access, DDRAM/CGRAM addressing per supported geometry, and rendering. It deliberately left open the
bus device trait shape, instantiation/wiring mechanics, and the external wire protocol to a
companion process (spec §9) — "a follow-up document ... once this device-level contract is
settled."

This plan resolves those open items against Emma65's existing conventions — reusing
`CharDisplay`'s cycle-accounted timing approach and `LedMatrix`'s command/data register state
machine generalization where they fit — and lays out a phased implementation for the device itself,
its compositing, and the debugger's dockable/detachable panel. It does **not** cover the external
wire protocol or the SDL2 companion-process binary issue #554 also asks for; per the spec's own
scoping, and mirroring how both `CharDisplay`'s and `LedMatrix`'s own external protocols and
companion binaries were separate follow-on plans built only after each device existed and was
usable through the debugger, that work is left to an equivalent follow-on plan here (see
"Explicitly out of scope"). This is a deliberate scope split, not an oversight of issue #554's
request for a companion binary — flag if a single combined plan is preferred instead.

This is a new device, not a redesign of an existing one — there is no prior `LcdDisplay` to
migrate from or config compatibility to preserve.

## Design decisions

### 1. Bus device trait shape and module layout

No new trait; a plain `IoDevice` implementation, mirroring `CharDisplay`/`LedMatrix`. Given the
amount of device-specific machinery (instruction decode, nibble pairing, busy timing, DDRAM/CGRAM
addressing, shift offsets, compositing), it gets its own submodule directory:

```
src/emulator/device/lcd_display/
  mod.rs        — LcdDisplay: IoDevice impl, register access, instruction decode, nibble
                   pairing, busy timing, DDRAM/CGRAM storage, address counter, shift offsets
  compositing.rs — pure fn: (DDRAM/CGRAM view, geometry, cursor state, colors) -> RGBA byte buffer
  cgrom.rs        — the built-in default character ROM table plus the `cgrom=` file format parser
```

`emulator::device` re-exports `LcdDisplay`. Config type string: `display/lcd`, alongside `display`
and `display/matrix`. Not IRQ-capable (spec has no interrupt output at all) — the config module
allocates its `DeviceId` via `next_available()`.

### 2. Config surface

```rust
struct LcdDisplayAttributes {
    geometry: Option<String>,   // one of the 9 supported values; default "16x2"
    cgrom: Option<ExpandedPathBuf>,
    background: Option<String>, // hex RGB24, e.g. "0000AA"; default matches spec §3
    foreground: Option<String>, // default matches spec §3
}
```

`base_address` is the standard `type@address` address, as with every other device. Bus size is
always 2 bytes (spec §4.1) — the only device whose mapped size doesn't scale with any config
attribute.

**Geometry table**: a `const` array of `Geometry { rows: u8, columns: u8, segments: &'static
[&'static [(u8, u8)]] }` (one row's segment list per entry), matching spec §7.1's table exactly,
looked up by the config's `geometry` string. Rejecting anything outside the 9 supported strings
happens here, at config time, the same way `LedMatrix`'s `arrangement` string is validated.

**Validation at configuration time:** `geometry` (if present) must match one of the 9 supported
strings; `cgrom` (if present) must parse as a valid CGROM table (§5); `background`/`foreground` (if
present) must parse as 6-hex-digit RGB24.

### 3. Register access: nibble pairing per register

Per spec §6, nibble pairing is tracked per register (instruction vs. data), independent of each
other:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum NibbleState {
    Idle,             // 8-bit mode, or 4-bit mode awaiting the first (high) nibble
    HighReceived(u8), // 4-bit mode: high nibble already received, awaiting the low nibble
}
```

`LcdDisplay` holds one `NibbleState` for the instruction register and one for the data register.
`interface_width` (`DL`, from the most recent `Function Set`, default 8-bit) is read to decide
whether a raw incoming byte is a complete value (`Idle` stays `Idle`, value used directly) or a
nibble (`Idle` -> `HighReceived(value & 0xF0)`, or `HighReceived(hi) -> Idle` with the assembled
byte `hi | (value >> 4)` handed to instruction decode / data access respectively). Switching
`interface_width` via `Function Set` does not reset either `NibbleState` — per spec §6, the new
width only governs whichever access comes next, and a `Function Set` write is itself always
processed as a complete instruction by the time it takes effect.

### 4. Instruction decode

A single `fn execute_instruction(&mut self, byte: u8)` matches `byte`'s high bits against spec
§4.2's table (checking from most-specific pattern — `Set DDRAM Address`'s single high bit — down to
`Clear Display`'s full match, the natural precedence order for this bit-pattern family) and applies
each instruction's effect directly (no intermediate command/argument-count state machine is needed
here, unlike `LedMatrix`'s `CMD_*` design — every HD44780 instruction is exactly one byte with its
arguments packed into that same byte's low bits, so there is nothing to "arm" across multiple data
writes the way `LedMatrix`'s multi-byte commands need). Sets `busy_cycles_remaining` per spec §5's
timing class table (`SHORT_CYCLES`/`LONG_CYCLES`, both derived once at construction the same way
`CharDisplay` derives `cycles_per_frame`, from `clock_hz` or the shared `NOMINAL_CLOCK_HZ` fallback
already public on `display::NOMINAL_CLOCK_HZ` — reused directly rather than a duplicate constant).

`fn write_instruction_register(&mut self, byte: u8)` and `fn write_data_register(&mut self, byte:
u8)` are the two nibble-pairing entry points (design §3); once a complete byte is assembled, they
call `execute_instruction`/a data-write handler respectively — but only if `!self.busy()` (spec
§4.2/§4.4: an access arriving while busy is discarded, logged at `Debug`, and does not disturb any
in-progress nibble pairing since the discard happens *after* nibble assembly completes, exactly
like a real 4-bit-mode host that dutifully sends both nibbles before ever finding out the byte was
ignored).

### 5. DDRAM/CGRAM storage, address counter, and CGROM

```rust
struct LcdDisplay {
    ddram: [u8; 80],
    cgram: [u8; 64],
    ac: AddressCounterTarget, // Ddram(u8) | Cgram(u8) -- current address + which RAM it targets
    entry_id: bool,           // ID: true = increment
    entry_shift: bool,        // S
    display_on: bool,
    cursor_on: bool,
    cursor_blink: bool,
    interface_width_8bit: bool, // DL
    font_5x10: bool,             // F
    line_shift: [u8; 2],         // one offset per 40-byte line; single-line geometries use [0]
                                  // only, modulo 80 instead of 40 (spec §7.4)
    busy_cycles_remaining: u64,
    geometry: &'static Geometry,
    cgrom: CgRom,
    background: Rgb24,
    foreground: Rgb24,
    // ... address_range, frame_sink, log_sender, etc., mirroring CharDisplay/LedMatrix
}
```

`AddressCounterTarget` bundles "which RAM" with "current address" in one enum specifically so an
increment/decrement can never be applied against the wrong RAM's modulus (spec §4.4.1) — `advance()`
matches on the variant, applies `entry_id`'s direction mod 80 or mod 64 respectively, and returns
the RAM targeted (used by the data-write/read handlers to index `ddram`/`cgram` directly). `Set
CGRAM Address`/`Set DDRAM Address` simply replace `ac` wholesale.

`cgrom.rs`'s `CgRom` type is a fixed `[[u8; 8]; 256]`-shaped table (5×8 rows; 5×10 glyphs store
their extra rows 8–10 the same way, just addressed differently per spec §8.2 — the table itself
doesn't need to know which font is active, only the caller resolving a DDRAM byte to a row does) for
the default built-in table (`cgrom::default_table()`, `include_bytes!`-embedded, populated from a
published HD44780 "A02" ROM character table per spec §8.1.1 — sourcing the actual glyph bitmaps is
this plan's Work Unit 2 deliverable, not fixed by the spec) plus a `CgRom::from_bytes` parser for
the optional `cgrom=` override file, in the same fixed binary-blob style as `display::font::Font`.

Resolving a DDRAM byte to a glyph's 8 (or 10, per §8.2) row-bytes: `0x00..=0x0F` index into `cgram`
per spec §8.2's `F`-dependent stride; everything else indexes `cgrom` directly (`0x10..=0x1F` and
`0x80..=0x9F` are simply blank entries baked into the default table's data, not special-cased in
code).

### 6. Timing

```rust
const SHORT_INSTRUCTION_US: u64 = 37;
const LONG_INSTRUCTION_US: u64 = 1_520;
```

`cycles_for(duration_us, clock_hz)` = `(effective_clock_hz * duration_us / 1_000_000).max(1)`,
computed once per instruction at execution time (not cached per-instruction-type at construction,
since it's cheap arithmetic run at most once per instruction and keeps the two duration constants
as the single source of truth rather than a precomputed table needing its own field). `tick(cycles)`
subtracts from `busy_cycles_remaining`, saturating at 0 — a plain countdown, not the accumulate-
and-fire cadence `CharDisplay`/`LedMatrix` use for periodic vsync/auto-refresh, since this device
has nothing periodic: it only ever waits out one instruction at a time.

### 7. Frame delivery: push on every register access, no cadence

Unlike `CharDisplay`/`LedMatrix`, this device has no vsync/auto-refresh concept at all (spec §2.1's
timing is about busy/instruction latency, not a periodic redraw) — a real LCD panel's visible state
changes are effectively immediate once an instruction/data access completes. So: `tick()` only ever
decrements `busy_cycles_remaining` (design §6); there is no `cycle_accumulator`/`cycles_per_frame`
pair here at all. Instead, every register write that could change what's rendered (spec §8.3: in
practice, every instruction and every data write) recomposites and pushes an `LcdDisplayFrame`
immediately after applying its effect, via the same never-blocks `mpsc::Sender::try_send` contract
`DisplayFrame`/`LedMatrixFrame` use. Given the device's total render cost (at most 80 glyph cells,
far smaller than `CharDisplay`'s default 1000 or `LedMatrix`'s 8×1,024), recompositing on every
write rather than batching is not a performance concern.

```rust
pub struct LcdDisplayFrame {
    pub pixels: Vec<u8>, // RGBA, geometry.columns * 5 by geometry.rows * (8 or 10) pixels
    pub columns: u8,
    pub rows: u8,
}
```

`InstantiationContext` gains `lcd_display_frame_sink: Option<LcdDisplayFrameSlot>` and
`lcd_display_geometry_sink: Option<LcdDisplayGeometrySlot>`, mirroring `DisplayFrameSlot`/
`DisplayGeometrySlot` exactly; `LcdDisplayGeometry { columns: u8, rows: u8 }` is all the panel
needs to size itself before any frame arrives (cell pixel dimensions are derived from the font
height, itself only known once `Function Set`'s `F` bit has been programmed — the panel doesn't
need to know this ahead of the first frame, unlike `LedMatrix`'s fixed 32×32).

### 8. Compositing

A pure function, no font/glyph state beyond what's passed in: `compositing::composite(ddram: &[u8;
80], cgram: &[u8; 64], geometry: &Geometry, line_shift: &[u8; 2], cursor: CursorState, display_on:
bool, font_5x10: bool, cgrom: &CgRom, background: Rgb24, foreground: Rgb24) -> Vec<u8>`. Walks each
of `geometry`'s rows, each row's segments (spec §7.1), applying `line_shift`'s modulo-40-or-80
offset (spec §7.4) to compute each visible cell's actual DDRAM address, resolves that byte to a
glyph's row bytes (design §5), and draws each set pixel bit as a filled square/circle (matching
`LedMatrix`'s `DrawRenderer`-style dot rendering for visual consistency across the project's LCD-
style panels) in `foreground` against `background` — or renders the whole grid blank in
`background` when `display_on` is false (spec §8.3). `CursorState { position: Option<(u8, u8)>,
visible: bool, blinking: bool }` is computed by the caller (`LcdDisplay::compositing_cursor()`,
translating the address counter's current DDRAM address back to a visible row/column via the
segment table, or `None` if off-screen/targeting CGRAM per spec §8.3) and drawn as the specified
under score or block over whichever cell it lands on.

### 9. Debugger panel: one dockable/detachable panel

Follows the Terminal/Display/LED-matrix panels' established pattern function-for-function:

- **Backend module** `debugger/src-tauri/src/lcd_display.rs`: bridge task forwarding
  `LcdDisplayFrame`s as an `"lcd-display-frame"` event, `detach_lcd_display`/`attach_lcd_display`/
  `get_lcd_display_geometry` commands, detached-window install/restore.
- **Statically declared detached window**: `lcd-display-detached` entry in `tauri.conf.json`.
- **Layout persistence**: `DockLayoutData` gains `lcd_display_detached: bool`.
- **Window menu**: a "Detach LCD Display…"/"Attach LCD Display" toggle in `menu.rs`.
- **Shutdown**: `LcdDisplay::shutdown()` drops its frame sink.

### 10. Frontend panel

New `debugger/frontend/src/LcdDisplayPanel.tsx`: on mount, calls `get_lcd_display_geometry`, sizes
a single `<canvas>` to the reported grid, listens for `"lcd-display-frame"` and blits `pixels` via
`putImageData` — no compositing logic in the frontend, matching `DisplayPanel.tsx`/
`LedMatrixPanel.tsx`. Registered in `layout/panelRegistry.tsx` (`MainPanelId` gains `"lcdDisplay"`),
with an `lcd-display-detached.tsx`/`.html` entry point mirroring the existing detached-window
pairs.

## Work Units

One branch and PR per unit; stop after each and await review before starting the next.

### 1. Library device core

`src/emulator/device/lcd_display/mod.rs`: `LcdDisplay` struct and `IoDevice` impl — register
access with per-register nibble pairing (design §3), instruction decode covering every instruction
in spec §4.2 (design §4), busy timing (design §6), DDRAM/CGRAM storage with the address-counter/RAM-
target bundling and wrap rules (design §5, spec §4.4.1), entry-mode-driven auto-increment/decrement,
and per-line shift offsets (spec §7.4) including the shared-shift-moves-every-line behavior. No
compositing, no CGROM, no transport, no frame sink yet — tests are direct `read`/`write`/`peek`
calls exercising register semantics only (e.g. a full 4-bit-mode nibble-pair write producing the
right assembled byte; busy correctly gating and then releasing a subsequent access; address-counter
wrap at both RAM boundaries; `Return Home` resetting shift offsets but not DDRAM contents; `Clear
Display` filling DDRAM with `0x20`). Also covers the geometry table (design §2) as a `const` lookup
consumed directly by the shift/segment logic, even though no config wiring exists yet (a
hand-constructed `Geometry` is passed into `LcdDisplay::new` by tests, the same way `CharDisplay`'s
tests hand-construct dimensions before its config module exists).

### 2. CGROM and compositing

`src/emulator/device/lcd_display/cgrom.rs`: the default built-in character table (spec §8.1.1 —
sourcing real glyph bitmap data from a published HD44780 ROM table is part of this unit's work, not
fixed by the spec) plus `CgRom::from_bytes` for the `cgrom=` override format (defined during this
unit, analogous to how `display::font::Font`'s file format was settled during `CharDisplay`'s own
compositing unit). `src/emulator/device/lcd_display/compositing.rs`: `composite()` (design §8),
covering both 5×8 and 5×10 font row resolution (spec §8.2, including the "only 4 usable characters"
5×10 CGRAM addressing), segment/shift-aware cell address resolution (spec §7.4), cursor rendering
(spec §8.3, underline vs. blinking block), and the `display_on`=false all-blank case. Golden-pixel
tests: known DDRAM/CGRAM/geometry/cursor combinations composited and asserted byte-for-byte, plus a
dedicated test asserting specific default-CGROM glyphs (e.g. `0x41` renders as 'A') against the
sourced bitmap data so a transcription error is caught immediately.

### 3. Config module and registry wiring

New `src/emulator/config/lcd_display.rs`: `LcdDisplayAttributes`/`LcdDisplayModule` per design §2
(geometry-string validation against the 9 supported values, `cgrom=`/`background=`/`foreground=`
parsing), non-IRQ device ID allocation, constructing the device with the resolved `Geometry` and
`CgRom`. Registered under `display/lcd` in `DeviceRegistry::with_builtins()`. No
`InstantiationContext` changes yet — `lcd_display_frame_sink`/`lcd_display_geometry_sink` stay
unset, same as the equivalent config-only units for `CharDisplay`/`LedMatrix`; the device is
configurable and instantiable, just has nothing to push frames to outside the debugger.

### 4. Debugger backend integration

`InstantiationContext::lcd_display_frame_sink`/`lcd_display_geometry_sink` (design §7), debugger
setup code creating the channel and stashing the sender before bus construction; `lcd_display.rs`
(design §9): bridge task, `detach_lcd_display`/`attach_lcd_display`/`get_lcd_display_geometry`
commands, detached-window install/restore. `tauri.conf.json` gains the `lcd-display-detached`
window declaration. `DockLayoutData::lcd_display_detached`. `menu.rs`'s Window menu item.

### 5. Frontend integration

`LcdDisplayPanel.tsx`, `lcd-display-detached.tsx`/`.html`, `panelRegistry.tsx` wiring,
`DockLayout.tsx` handling for detach/reattach events mirroring the existing
`display-detach-requested`/`display-reattached` handling.

### 6. Manual verification

`cargo tauri dev`; drive the device by hand through the Memory panel or a short hand-assembled
6502 routine (no demo program exists yet) exercising: 8-bit and 4-bit instruction/data writes
producing identical DDRAM contents; busy correctly blocking a too-early follow-up write (observable
as a dropped character if the test program doesn't poll busy); `Clear Display`/`Return Home`;
`Cursor or Display Shift` scrolling text into view from off-screen DDRAM; a custom CGRAM character
written and then displayed; each of the 9 geometries configured in turn, confirming row/segment
layout (especially `16x1`'s split row and `16x4`/`20x4`'s paired-row shift behavior) matches spec
§7.1; docked and detached windows both receive frames correctly after a detach/reattach cycle; a
profile switch or session reload tears down the old device's channel cleanly.

## Explicitly out of scope for this plan

- The external wire protocol (spec §9) and the `emma65-lcd-display` (or similarly named) SDL2
  companion-process binary — a separate follow-on plan once this device exists and is usable
  through the debugger, mirroring the SDL2 display peripheral plan and the LED matrix companion
  binary plan. The device's frame-on-every-write model (design §7) should translate directly into a
  tagged per-write message on that follow-on protocol, but the exact framing is left to it.
- The future VIA-protocol client transport mode (issue #554, spec §9).
- 4-bit-mode reads split across two nibble reads (spec §9).
- Contrast/backlight control (spec §9).
- Any demo 6502 program exercising the device.
