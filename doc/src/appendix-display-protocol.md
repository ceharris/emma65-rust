# Character Display External Protocol

Wire protocol [`CharDisplay`](io-devices.md#character-display-display)
(config type `display`) uses to stream its composited frame data to an
external peripheral process — the bundled `emma65-display` binary, or a
replacement for it — over an attached [`Transport`](io-devices.md#transport-options),
when running the plain `emma65` CLI standalone. It's unrelated to the
debugger's in-process `DisplayFrame`/`attach_frame_sink` push channel, which
needs no wire protocol at all (same address space, same process), and
unrelated to the [LED Matrix External Protocol](appendix-led-matrix-protocol.md)
— a different device with different needs. Implemented in
`src/emulator/device/display/protocol.rs`; full historical design rationale
in `plan/char-display-external-protocol.md`. See
[Running the Display Peripheral](running-the-display-peripheral.md) for how
to configure and launch `emma65-display` itself; see
`plan/memory-mapped-display-device-spec.md` for `CharDisplay`'s bus-facing
register behavior — this page covers only what crosses the transport.

## Transport requirements

Exactly one connection, used in both directions: outbound (device →
peripheral) for the header and frames below, and inbound (peripheral →
device) for keystrokes. The outbound direction's `Transport::send_bytes`
must be atomic — either the whole buffer is written or none of it is — which
in practice means only `pipe:` is supported; the config module rejects any
other transport spec for a `display` device at instantiation time. The
inbound direction carries no such requirement, since each of its messages is
a single byte.

## Message framing

No length prefixes or delimiters anywhere. The header is sent exactly once,
immediately when the transport is attached. Every subsequent outbound
message is a frame, sent once per vsync, always exactly `2 * cells + 3 *
palette_len` bytes — a size fully determined by the header, which a receiver
reads exactly once at the start of the stream. This is only safe because of
the outbound direction's atomicity requirement above: a transport that could
deliver a partial frame would desync the stream permanently, with no way to
resynchronize.

## Header (sent once, on attach)

| Field           | Type      | Size (bytes) | Notes                                              |
|-----------------|-----------|--------------|-----------------------------------------------------|
| `magic`         | ASCII     | 4            | `"E65D"` — distinct from the trace format's `"E65T"` |
| `version`       | `u8`      | 1            | `1`                                                 |
| `columns`       | `u32` LE  | 4            | grid width in cells                                 |
| `rows`          | `u32` LE  | 4            | grid height in cells                                |
| `frame_rate_hz` | `u32` LE  | 4            | vsync cadence; informational only — a peripheral is not required to sync its own redraw to it |
| `palette_len`   | `u16` LE  | 2            | fixed for the connection's lifetime (see [Runtime palette updates](#runtime-palette-updates)) |
| `font`          | raw bytes | 2048         | 256 glyphs × 8 bytes/row; same layout as `font::Font` (`src/emulator/device/display/font.rs`) |

Total header size: 2067 bytes.

## Frame (sent once per vsync)

| Field       | Size (bytes)        | Notes                                             |
|-------------|---------------------|----------------------------------------------------|
| char RAM    | `cells`             | one glyph index per cell, row-major, top row first |
| color RAM   | `cells`             | one palette index per cell, row-major, top row first |
| palette     | `palette_len * 3`   | RGB24 triples (`r`, `g`, `b`), in palette order    |

`cells = columns * rows`, from the header. Total frame size, constant for
the life of the connection: `2 * cells + 3 * palette_len` bytes.

The char/color RAM sent is always whatever `CharDisplay::frame_source()`
currently returns — the scanout buffers in double-buffered mode, the
CPU-addressable buffers directly otherwise — i.e. exactly the same data the
debugger's in-process `DisplayFrame` compositing path reads on the same
vsync.

## Inbound keystroke stream (peripheral → device)

Unlike the outbound stream above, this direction has no length prefix or
framing at all: one byte per keystroke, sent whenever the peripheral
captures a key press, with no relationship to vsync cadence or frame
boundaries. `CharDisplay` forwards each byte into its keyboard sub-range's
`InputBuffer` when a `keyboard-address=` range is configured for the device,
and silently discards it otherwise.

Encoding mirrors the scheme `debugger/src-tauri/src/keyboard.rs` forwards
from `DisplayPanel.tsx`'s `keyboardByteForEvent` table: ordinary printable
characters send their ASCII character code; `Enter`, `Backspace`, `Tab`, and
`Escape` send the standard ASCII control codes (`0x0D`, `0x08`, `0x09`,
`0x1B`); `Ctrl+<letter>` sends `charCode(letter) - 64`. Non-ASCII input
(e.g. from an IME) is silently dropped rather than encoded — there is no
multi-byte encoding in this stream.

## Runtime palette updates

`CharDisplay` supports writing individual palette entries at runtime (see
the memory-mapped display device spec's control/status register section).
Rather than a separate update message, the *entire* palette is resent as
part of every frame: simpler for both sides, and cheap — even the maximum
256-entry palette is 768 bytes, small next to a default 40×25 grid's 2000
bytes of char+color RAM. No special-casing is needed on the sending side for
an update to take effect: each frame just reflects the palette's current
in-memory state, whatever it happens to be. `palette_len` itself, unlike the
entries, cannot change after the header is sent — palette length is fixed
at configuration time and is never the subject of a runtime update.

## Non-goals

No reconnection support — the design assumes a single spawned child process
tied to the device's lifetime — and no protocol negotiation. A version
mismatch has no defined recovery: a peripheral that doesn't recognize a
header's `version` should refuse to proceed rather than guess at a
compatible framing.
