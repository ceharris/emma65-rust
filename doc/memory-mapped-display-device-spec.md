# Memory-Mapped Display Device — Specification

## 1. Purpose and scope

This document specifies a memory-mapped, character/color-cell display 
device. It captures the *behavior and interface contract* of the
device: its configuration surface, its address-space layout, and the semantics of
reads, writes, and buffer swapping. It deliberately does not specify:

- The shape of Emma65's bus device trait(s).
- How the device is instantiated, registered, or wired into bus configuration.
- Timing/scheduling mechanics beyond the single `on_vsync_tick`-equivalent hook
  the device needs to be driven from.

Those are left to be resolved against Emma65's existing conventions.

## 2. Conceptual model

The device is a character-cell display, structurally similar to the VIC-II in the
Commodore 64 (separate character RAM and color RAM over a fixed grid), but:

- Grid dimensions are configurable rather than fixed at 40×25 (though 40×25 is
  the default).
- Color RAM is a full 8-bit palette index per cell (not 4-bit / 16-color as on
  the C64).
- The color palette is fixed RGB24 data supplied at configuration time — it is
  **not** part of the device's bus-addressable memory.
- The device optionally supports double buffering. When enabled, this is
  implemented as a backend-side snapshot copy on swap, not a bank-switch: the
  CPU-addressable memory has a single, unchanging identity for the life of the
  device. See §5.

A future C64-compatibility mode (packed 4-bit color, two indices per byte) is
anticipated but out of scope for this revision. See §7.

## 3. Configuration (locked at bus configuration time)

The following are fixed for the lifetime of a device instance. They are not
writable through the device's memory-mapped registers; they are supplied when
the device is configured/instantiated and cannot change afterward.

| Field | Type | Default | Notes |
|---|---|---|---|
| `columns` | integer | 40 | Grid width in cells |
| `rows` | integer | 25 | Grid height in cells |
| `palette` | list of RGB24 triples | 16-entry default palette | 1–256 entries. Length need not be a power of two. |
| `double_buffered` | bool | `true` | See §5 |
| `base_address` | integer | — | Where the device's memory-mapped region begins on the bus |

Derived, not separately configurable:

- `cells = columns * rows` (1000 for the 40×25 default). All RAM regions below
  are sized from this.

**Validation at configuration time** (before the device becomes live on the
bus):
- `palette` must be non-empty and must contain no more than 256 entries (color
  RAM indices are a full byte).
- `columns`, `rows` must be positive.

## 4. Bus-addressable memory map

Only **one** buffer's worth of character/color RAM is ever visible in the bus
address space, regardless of whether double buffering is enabled. The device
does not expose bank selection to the CPU in any form — there is exactly one
char RAM region and one color RAM region at fixed offsets, always.

| Region | Offset | Size | Access | Notes |
|---|---|---|---|---|
| Character RAM | `0x0000` | `cells` bytes | R/W | Glyph index per cell |
| Color RAM | `0x03E8`* | `cells` bytes | R/W | 8-bit palette index per cell (0..=255, wrapped/clamped into actual palette length — see §4.1) |
| Control register | `0x07D0`* | 1 byte | R/W | See §4.2 |
| Status register | `0x07D1`* | 1 byte | R (writes ignored) | See §4.3 |

*Offsets shown are for the 40×25 default (`cells = 1000`); they scale with
`cells` for other configured dimensions.

Total mapped size is `2 * cells + 2` bytes, unaffected by whether double
buffering is on — double buffering is purely an internal detail of the
device's backend and costs no additional address space.

### 4.1 Palette index resolution

Color RAM cells are always a full byte (0–255), independent of `palette.len()`.
When compositing a frame, an index that falls outside the configured palette's
range must resolve to *some* defined color rather than panicking or reading
out of bounds. Recommended rule, to be confirmed during implementation:

- If `palette.len()` is a power of two, mask the index to `palette.len() - 1`
  (matches hardware-style bank-masking intuition, e.g. a 16-entry palette masks
  with `0x0F`).
- Otherwise, reduce the index modulo `palette.len()`.

This resolution happens only in the backend's frame-compositing path — it does
not affect what byte value is stored in or read back from color RAM. Reads of
color RAM must return exactly what was last written, unmodified.

### 4.2 Control register (offset `0x07D0`)

| Bit | Meaning | R/W |
|---|---|---|
| 0 | Swap request (write 1 to request a swap; self-clearing, always reads 0) | W |
| 1 | Swap-on-vsync enable (0 = swap immediately on request, 1 = defer swap to next vsync) | R/W |
| 7 | Swap pending (1 = a vsync-deferred swap has been requested and not yet performed) | R |

Writing bit 0 = 0 has no effect. Writing bit 0 = 1 triggers `request_swap`
(§5.2). In single-buffered mode (`double_buffered == false`), writes to bit 0
are accepted but have no observable effect, since there is no separate scanout
buffer to update.

Bit 1 is stored and readable independent of bit 0 — it configures how future
swap requests are handled, not just the current write.

### 4.3 Status register (offset `0x07D1`)

| Bit | Meaning |
|---|---|
| 0 | Vsync flag — set by `on_vsync_tick`, semantics of clearing (e.g. read-to-clear vs. cleared at start of next frame) to be decided during implementation against Emma65's existing timing conventions |

Writes to this register are ignored.

## 5. Double buffering semantics

### 5.1 Model

When `double_buffered == true`, the device maintains:

- **CPU-addressable buffers** (`char_ram`, `color_ram`): the memory backing
  §4's address map. These have a single, fixed identity for the life of the
  device — the CPU always reads and writes the same underlying storage,
  regardless of swap state.
- **Scanout buffers** (not bus-addressable): a copy of the CPU-addressable
  buffers, taken at swap time. The renderer/frontend reads only from the
  scanout buffers.

A "swap" is implemented as **copying the CPU-addressable buffers into the
scanout buffers**, not as a pointer/bank exchange. This is the key semantic
decision of this revision: it guarantees that immediately after a swap, the
CPU-addressable buffers still contain exactly what the CPU last wrote to them.
Animation logic (e.g. a per-cell cellular-automaton-style rain routine) can
therefore rely on reading back, on any given tick, whatever it wrote on a
previous tick, with no bank-parity bookkeeping and no risk of reading stale
data from a buffer it doesn't currently "own."

When `double_buffered == false`, there are no separate scanout buffers; the
renderer reads directly from the same buffers the CPU writes, and swap
requests are no-ops.

### 5.2 Swap triggering

- `request_swap` is invoked when the CPU writes bit 0 = 1 to the control
  register (§4.2).
- If swap-on-vsync (bit 1) is **not** set: the copy (§5.1) happens
  synchronously, immediately.
- If swap-on-vsync **is** set: the swap is deferred. The device records that a
  swap is pending (status bit 7 reads 1 until performed) and performs the copy
  the next time it is driven by the vsync-equivalent tick (§5.3).
- A second swap request arriving while one is already pending is idempotent —
  it does not queue a second swap or change status beyond what's already
  reflected. Whether this should instead be treated as an error/dropped-frame
  condition is an open question (§8).

### 5.3 Vsync-equivalent tick

The device needs to be driven by a single, well-defined call once per frame
(naming and exact mechanism to be determined against Emma65's existing
scheduler/timing conventions). On this call, the device:

1. Sets the vsync status flag.
2. If a swap is pending, performs the copy (§5.1) and clears the pending flag.

## 6. Frame source for rendering

Regardless of buffering mode, the renderer/frontend obtains the frame to
display through a single accessor exposing the pair of (character, color)
buffers currently intended for scanout:

- Double-buffered: the scanout buffers.
- Single-buffered: the CPU-addressable buffers directly.

The renderer never reads the CPU-addressable buffers directly when double
buffering is active, and never needs to know which mode is in effect beyond
calling this one accessor.

Color-to-RGB24 resolution (§4.1) happens in this rendering path, using the
palette fixed at configuration time.

This device does not specify how the resulting pixel data reaches the
Tauri-based dockable panel (event emission, shared buffer, polling, etc.) —
that is a presentation-layer concern layered on top of `frame_source`.

## 7. Explicitly out of scope for this revision

- **C64-compatible packed color mode**: a future mode where each color RAM
  byte holds two 4-bit palette indices (halving effective color RAM size or
  addressing by nibble). If pursued later, this should be exposed via a
  reserved control register bit (bit 2 suggested) and will require deciding
  whether color RAM is always allocated at full `cells` bytes (simpler
  addressing, wastes space in packed mode) or shrinks to `cells / 2` bytes in
  packed mode (saves space, offset table becomes mode-dependent). No address
  space has been reserved for this beyond leaving bit 2 of the control
  register unused.
- Bus device trait shape, instantiation, and registration mechanics.
- Exact timing/scheduling integration beyond the single vsync-equivalent hook
  described in §5.3.

## 8. Open questions for implementation

1. **Palette index resolution rule** (§4.1): confirm power-of-two masking vs.
   modulo, or choose a single uniform rule (e.g. always modulo) for
   simplicity.
2. **Double swap-request behavior** (§5.2): confirm idempotent-ignore is
   acceptable, or specify a dropped-frame/error indication.
3. **Status register vsync-flag clear semantics** (§4.3): read-to-clear vs.
   cleared automatically at the start of the next tick — should match
   whatever convention Emma65 already uses elsewhere for similar status bits.
4. **Unmapped-address behavior**: whether reads/writes to addresses within the
   device's configured range but outside all defined regions (there are none
   in the current map, but this matters if `cells` calculations ever leave
   gaps) should return 0, be treated as an error, or follow whatever
   convention Emma65 uses elsewhere.
