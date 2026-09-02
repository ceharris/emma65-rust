# `LedMatrix` External Protocol — Specification

## 1. Purpose and scope

This document specifies the wire protocol `LedMatrix` (`src/emulator/device/led_matrix/`,
config type `display/matrix`) uses to stream its per-matrix pixel and palette data to an external
peripheral process over a [`Transport`](../src/emulator/transport/mod.rs), for use when running
the plain `emma65` CLI standalone (no Tauri debugger). It is unrelated to the debugger's
in-process `LedMatrixFrame`/`attach_frame_sink` push channel, which remains the mechanism the
debugger uses and needs no protocol at all (same address space, same process).

It is also unrelated to `CharDisplay`'s external protocol (`plan/char-display-external-protocol.md`)
— a different device with different needs, most notably that `LedMatrix` swaps happen per-matrix
rather than in lockstep across the whole device on a single vsync. Do not confuse the two.

See `plan/memory-mapped-led-matrix-device-spec.md` for `LedMatrix`'s bus-facing register behavior
(command/data registers, matrix swapping, runtime palette updates) — this document only covers
what crosses the transport.

## 2. Transport requirements

Exactly one connection, outbound only (device → peripheral) — unlike `CharDisplay`, `LedMatrix`
has no input capability, so there is no inbound direction to specify. `Transport::send_bytes`
must be atomic: either the whole buffer is written or none of it is (see the transport module's
documentation). This rules out any transport whose `send_bytes` is the default per-byte-loop
fallback; in practice `PipeTransport` is the only supported implementation, and the config module
(Unit 2 of the LED matrix companion binary plan) enforces this by rejecting any other transport
spec at instantiate time.

## 3. Message framing

Unlike `CharDisplay`'s protocol, messages here are tagged rather than shaped as one fixed-size
frame — swaps happen per-matrix, at unpredictable intervals relative to each other, so there is no
single per-tick "frame" to send as a unit. The header (§4) is sent exactly once, immediately when
the transport is attached (`LedMatrix::attach_external_transport`). Every subsequent message
begins with a one-byte tag (§5) that determines its fixed total length; there are no length
prefixes anywhere in this protocol. This is only safe because of §2's atomicity requirement: a
transport that could deliver a partial message would desync the stream permanently, with no way
to resynchronize.

## 4. Header (sent once, on attach)

| Field           | Type      | Size (bytes) | Notes                                                |
|-----------------|-----------|--------------|-------------------------------------------------------|
| `magic`         | ASCII     | 4            | `"E65M"` — distinct from the trace format's `"E65T"` and the display protocol's `"E65D"` |
| `version`       | `u8`      | 1            | `2`                                                   |
| `matrix_count`  | `u8`      | 1            | number of matrices configured (spec §2), `1..=8`      |
| `columns`       | `u8`      | 1            | the device's configured arrangement's column count (spec §2.2); the peripheral derives row count as `matrix_count / columns`, which always divides evenly |
| `frame_rate_hz` | `u32` LE  | 4            | auto-refresh cadence; informational only — a peripheral is not required to sync its own redraw to it |

Total header size: 11 bytes. Unlike `CharDisplay`'s header, there is no palette or per-matrix
dimension field: matrix dimensions are a fixed 32×32 constant (spec §2) that both sides already
know, and the palette is never transferred at connection time (see §7).

`columns` was added in version 2, replacing the peripheral's own `--arrangement` command-line
flag: the peripheral's on-screen layout now always mirrors the device's actual bus-addressing
arrangement (design doc §2.2) rather than an independently-chosen value that could disagree with
it.

## 5. Messages (sent as they occur)

Every message after the header begins with a one-byte tag identifying its type and fixed length.

### 5.1 Block (`MSG_BLOCK = 1`, sent once per matrix swap)

| Field          | Type    | Size (bytes) | Notes                                                    |
|----------------|---------|--------------|-----------------------------------------------------------|
| `tag`          | `u8`    | 1            | `1`                                                        |
| `matrix_index` | `u8`    | 1            | which matrix this block belongs to, `0..matrix_count`      |
| `pixels`       | raw     | 1024         | one palette-index byte per pixel, row-major, top row first (spec §2, §5.1) |

Total message size: 1026 bytes. Sent whenever `LedMatrix::swap_matrix` runs for a given matrix —
whether triggered by `CMD_SWAP` or by auto-refresh (spec §5.2, §6) — carrying that matrix's
scanout buffer exactly as swapped. The peripheral is expected to composite these raw indices
against its own copy of the current palette (see §7), the same way the debugger's in-process path
does via `compositing::composite_matrix`.

### 5.2 Palette (`MSG_PALETTE = 2`, sent only on an actual `CMD_PALETTE_WRITE`)

| Field   | Type      | Size (bytes) | Notes                                                        |
|---------|-----------|--------------|----------------------------------------------------------------|
| `tag`   | `u8`      | 1            | `2`                                                             |
| `index` | `u8`      | 1            | palette entry updated, `0..256`                                |
| `color` | `u16` LE  | 2            | packed RGB565 (`rrrrrggggggbbbbb`), the entry's new value       |

Total message size: 4 bytes. Sent whenever `CMD_PALETTE_WRITE`'s effect is applied (spec §4.2),
carrying the already-quantized `Rgb565` value stored in the device's palette table — the same
value a subsequent `CMD_PALETTE_READ` of that entry would report (scaled back up to 8-bit
components), not the original pre-quantization write bytes.

### 5.3 Power (`MSG_POWER = 3`, sent only on an actual `CMD_SET_POWER`)

| Field  | Type | Size (bytes) | Notes                                                    |
|--------|------|--------------|-----------------------------------------------------------|
| `tag`  | `u8` | 1            | `3`                                                        |
| `mask` | `u8` | 1            | new power-state bitmask, one bit per matrix (spec §4.2)    |

Total message size: 2 bytes. Sent whenever `CMD_SET_POWER`'s effect is applied. The peripheral
must retain this mask and reapply it (via `compositing::composite_matrix`'s `power_on` parameter)
to every future composite of each affected matrix, the same way it already retains the palette
(§7) — a powered-off matrix composites to fully black regardless of palette content.

### 5.4 Brightness (`MSG_BRIGHTNESS = 4`, sent only on an actual `CMD_SET_BRIGHTNESS`)

| Field       | Type | Size (bytes) | Notes                                          |
|-------------|------|--------------|--------------------------------------------------|
| `tag`       | `u8` | 1            | `4`                                               |
| `level`     | `u8` | 1            | new global brightness level, `0..=255` (spec §4.2) |

Total message size: 2 bytes. Sent whenever `CMD_SET_BRIGHTNESS`'s effect is applied. The
peripheral must retain this value and reapply it (via `compositing::composite_matrix`'s
`brightness` parameter) to every future composite of every matrix, the same way it already retains
the palette (§7).

Both §5.3 and §5.4 are a pure addition to this tagged scheme, requiring no change to any existing
message's framing.

## 6. Startup state (device → peripheral)

`LedMatrix` never re-sends the full contents of every matrix or the whole palette at connection
time. A peripheral that attaches after the device has already been running sees only messages for
matrices swapped, palette entries written, and power/brightness changes made from that point
forward; anything unset renders using the peripheral's own reconstruction of
`compositing::default_palette()`, all-zero (index 0) pixel data, and full power/brightness
(`power_mask = 0xFF`, `brightness = 0xFF`), matching the device's own construction-time defaults
(spec §2, §2.1, §4.2).

## 7. Runtime palette updates

Unlike `CharDisplay`, which resends its entire palette with every frame (`plan/char-display-
external-protocol.md` §7), `LedMatrix`'s palette is comparatively large (256 entries, RGB565) and
changes independently of any single matrix's swap cadence, so each write is sent as its own small
message (§5.2) instead. A peripheral must therefore retain every matrix's most recently received
raw pixel indices (§5.1) as well as its own copy of the palette, and recomposite every matrix's
stored pixels whenever a palette message arrives — palette changes are not re-sent per matrix.
