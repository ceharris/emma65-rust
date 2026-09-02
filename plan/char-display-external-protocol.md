# `CharDisplay` External Protocol — Specification

## 1. Purpose and scope

This document specifies the wire protocol `CharDisplay` (`src/emulator/device/display/`,
config type `display`) uses to stream its composited frame data to an external peripheral
process over a [`Transport`](../src/emulator/transport/mod.rs), for use when running the plain
`emma65` CLI standalone (no Tauri debugger). It is unrelated to the debugger's in-process
`DisplayFrame`/`attach_frame_sink` push channel, which remains the mechanism the debugger uses
and needs no protocol at all (same address space, same process).

It is also unrelated to `LedMatrix`'s "Virtual Display Communication Protocol"
(`device::protocol::via`/`ptm`-adjacent, register-level, multipoint) — a different device with
different needs. Do not confuse the two.

See `plan/memory-mapped-display-device-spec.md` for `CharDisplay`'s bus-facing register
behavior (control/status registers, buffer swapping, runtime palette updates) — this document
only covers what crosses the transport.

## 2. Transport requirements

Exactly one connection, used in both directions: outbound (device → peripheral) for the header
and frames (§4, §5), and inbound (peripheral → device) for keystrokes (§6) — see
`plan/display-keyboard-integration-plan.md` for why the inbound direction exists at all. The
outbound direction's [`Transport::send_bytes`] must be atomic: either the whole buffer is written
or none of it is (see the transport module's documentation and the SDL2 display peripheral
plan's Unit 1). This rules out any transport whose `send_bytes` is the default per-byte-loop
fallback; in practice `PipeTransport` is the only supported implementation, and the config module
(Unit 3 of the SDL2 display peripheral plan) enforces this by rejecting any other transport spec
at instantiate time. The inbound direction carries no such requirement — §6's messages are a
single byte each, so partial delivery isn't a concern.

## 3. Message framing

There are no length prefixes or delimiters anywhere in this protocol. The header (§4) is sent
exactly once, immediately when the transport is attached (`CharDisplay::attach_external_transport`).
Every subsequent message is a frame (§5), sent once per vsync, and is always exactly
`2 * cells + 3 * palette_len` bytes — a size fully determined by the header, which a receiver
reads exactly once at the start of the stream. This is only safe because of §2's atomicity
requirement: a transport that could deliver a partial frame would desync the stream permanently,
with no way to resynchronize.

## 4. Header (sent once, on attach)

| Field           | Type      | Size (bytes) | Notes                                              |
|-----------------|-----------|--------------|-----------------------------------------------------|
| `magic`         | ASCII     | 4            | `"E65D"` — distinct from the trace format's `"E65T"` |
| `version`       | `u8`      | 1            | `1`                                                 |
| `columns`       | `u32` LE  | 4            | grid width in cells                                 |
| `rows`          | `u32` LE  | 4            | grid height in cells                                |
| `frame_rate_hz` | `u32` LE  | 4            | vsync cadence; informational only — a peripheral is not required to sync its own redraw to it |
| `palette_len`   | `u16` LE  | 2            | fixed for the connection's lifetime (see §7)        |
| `font`          | raw bytes | 2048         | 256 glyphs × 8 bytes/row; same layout as `font::Font` (see `src/emulator/device/display/font.rs`) |

Total header size: 2067 bytes.

## 5. Frame (sent once per vsync)

| Field       | Size (bytes)        | Notes                                             |
|-------------|---------------------|----------------------------------------------------|
| char RAM    | `cells`             | one glyph index per cell, row-major, top row first |
| color RAM   | `cells`             | one palette index per cell, row-major, top row first |
| palette     | `palette_len * 3`   | RGB24 triples (`r`, `g`, `b`), in palette order    |

`cells = columns * rows`, from the header. Total frame size, constant for the life of the
connection: `2 * cells + 3 * palette_len` bytes.

The char/color RAM sent is always whatever `CharDisplay::frame_source()` currently returns —
the scanout buffers in double-buffered mode, the CPU-addressable buffers directly otherwise —
i.e. exactly the same data the debugger's in-process `DisplayFrame` compositing path reads on
the same vsync.

## 6. Inbound keystroke stream (peripheral → device)

Unlike §3–§5's outbound stream, this direction has no length prefix or framing at all: one byte
per keystroke, sent whenever the peripheral captures a key press, with no relationship to vsync
cadence or frame boundaries. `CharDisplay` forwards each byte into its keyboard sub-range's
`InputBuffer` when a `keyboard-address=` range is configured for the device, and silently
discards it otherwise (see `plan/display-keyboard-integration-plan.md`'s Context section for why
discarding rather than erroring matters here).

Encoding mirrors the scheme `debugger/src-tauri/src/keyboard.rs` already forwards from
`DisplayPanel.tsx`'s `keyboardByteForEvent` table: ordinary printable characters send their ASCII
character code; `Enter`, `Backspace`, `Tab`, and `Escape` send the standard ASCII control codes
(`0x0d`, `0x08`, `0x09`, `0x1b`); `Ctrl+<letter>` sends `charCode(letter) - 64`. Non-ASCII input
(e.g. from an IME) is silently dropped rather than encoded — there is no multi-byte encoding in
this stream.

## 7. Runtime palette updates

`CharDisplay` supports writing individual palette entries at runtime (memory-mapped display
device spec, control/status register section). Rather than a separate update message, the
*entire* palette is resent as part of every frame (§5): simpler for both sides, and cheap — even
the maximum 256-entry palette is 768 bytes, small next to a default 40×25 grid's 2000 bytes of
char+color RAM. No special-casing is needed on the sending side for an update to take effect:
each frame just reflects the palette's current in-memory state, whatever it happens to be.
`palette_len` itself, unlike the entries, cannot change after the header is sent — palette
length is fixed at configuration time (memory-mapped display device spec §3) and is never a
subject of a runtime update.

## 8. Non-goals

No reconnection support (the design assumes a single spawned child process tied to the device's
lifetime — see Unit 4 of the SDL2 display peripheral plan), no protocol negotiation. A version
mismatch has no defined recovery — a peripheral that doesn't recognize a header's `version`
should refuse to proceed rather than guess at a compatible framing.
