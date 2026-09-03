# `LcdDisplay` External Protocol — Specification

## 1. Purpose and scope

This document specifies the wire protocol `LcdDisplay` (`src/emulator/device/lcd_display/`,
config type `display/lcd`) uses to stream its composited output to an external peripheral process
over a [`Transport`](../src/emulator/transport/mod.rs), for use when running the plain `emma65`
CLI standalone (no Tauri debugger). It is unrelated to the debugger's in-process
`LcdDisplayFrame`/`attach_frame_sink` push channel, which remains the mechanism the debugger uses
and needs no protocol at all (same address space, same process).

It is also unrelated to `CharDisplay`'s (`plan/char-display-external-protocol.md`) and
`LedMatrix`'s (`plan/led-matrix-external-protocol.md`) protocols — different devices with
different needs. Do not confuse the three.

See `plan/memory-mapped-lcd-display-device-spec.md` for `LcdDisplay`'s bus-facing register
behavior (the two-register HD44780 interface, instruction timing, DDRAM/CGRAM/CGROM) and
`plan/memory-mapped-lcd-display-device-plan.md` §7-§8 for its frame-push/compositing model — this
document only covers what crosses the transport.

## 2. Transport requirements

Exactly one connection, outbound only (device → peripheral) — like `LedMatrix`, and unlike
`CharDisplay`, this device has no input capability at all, so there is no inbound direction to
specify. `Transport::send_bytes` must be atomic: either the whole buffer is written or none of it
is (see the transport module's documentation). This rules out any transport whose `send_bytes` is
the default per-byte-loop fallback; in practice `PipeTransport` is the only supported
implementation, and the config module enforces this by rejecting any other transport spec at
instantiate time.

## 3. Message framing

The header (§4) is sent exactly once, immediately when the transport is attached
(`LcdDisplay::attach_external_transport`). Every subsequent message is a frame (§5), sent whenever
the device pushes a frame — i.e. after every register write that could change what's rendered
(design doc §7), with no periodic cadence at all, unlike `CharDisplay`'s per-vsync or `LedMatrix`'s
per-swap sends.

Unlike both of those protocols, a frame here is **not** a fixed size for the life of the
connection: `Function Set`'s `F` bit can switch the active font between 5×8 and 5×10 dots at any
time (spec §8.2), changing every subsequent frame's pixel height. So each frame message carries its
own `width`/`height` fields (§5) rather than relying on the header alone to fix a size — this is a
deliberate, narrow deviation from the fixed-frame-size convention `CharDisplay`/`LedMatrix`
established, made necessary by this device's variable font height, not a stylistic choice. There
are still no separate length prefixes or delimiters beyond those two fields: a frame's total size
is always exactly determined by `4 + width * height * 4` bytes, which the receiver can compute as
soon as it has read those two fields. This is only safe because of §2's atomicity requirement: a
transport that could deliver a partial frame would desync the stream permanently, with no way to
resynchronize.

## 4. Header (sent once, on attach)

| Field        | Type    | Size (bytes) | Notes                                                     |
|--------------|---------|--------------|------------------------------------------------------------|
| `magic`      | ASCII   | 4            | `"E65L"` — distinct from `"E65D"` (display) and `"E65M"` (LED matrix) |
| `version`    | `u8`    | 1            | `1`                                                        |
| `columns`    | `u8`    | 1            | configured character grid width (design doc §2)            |
| `rows`       | `u8`    | 1            | configured character grid height (design doc §2)           |
| `background` | RGB24   | 3            | `r`, `g`, `b` — configuration-time-fixed (spec §3)          |
| `foreground` | RGB24   | 3            | `r`, `g`, `b` — configuration-time-fixed (spec §3)          |

Total header size: 13 bytes. `columns`/`rows` are the character grid dimensions, not pixel
dimensions — a peripheral wanting pixel dimensions ahead of the first frame can compute an upper
bound (`columns * 5` by `rows * 10`) but must still read each frame's own `width`/`height` (§5) to
render it, since the actual font in use isn't known until the first frame arrives (mirroring how
`LcdDisplayGeometry`, the debugger's equivalent of this header, likewise omits pixel dimensions —
see `LcdDisplayPanel.tsx`'s `LcdDisplayGeometry` doc comment). `background`/`foreground` are
carried here, not derived from frame pixel data, so a peripheral can replicate the debugger panel's
dot-matrix cosmetics (§6) without having to reverse-engineer which composited pixels are "on" vs.
"off".

## 5. Frame (sent whenever the device pushes a frame)

| Field        | Type     | Size (bytes)      | Notes                                                  |
|--------------|----------|-------------------|----------------------------------------------------------|
| `width_px`   | `u16` LE | 2                 | `columns * 5` (spec §8.2's fixed glyph cell width)        |
| `height_px`  | `u16` LE | 2                 | `rows * 8` or `rows * 10`, depending on the active font   |
| `pixels`     | raw      | `width_px * height_px * 4` | RGBA, row-major, top row first, 4 bytes per pixel |

Total message size: `4 + width_px * height_px * 4` bytes. The pixel data sent is always exactly
what `compositing::composite` produces for the device's current DDRAM/CGRAM/CGROM/cursor/mode
state — the same bytes the debugger's in-process `LcdDisplayFrame::pixels` carries on the same
push, already fully composited (background/foreground baked into each pixel, cursor drawn, blank
when `display_on` is false). There is no palette to separately transmit or retain, unlike
`CharDisplay`/`LedMatrix`: this device's only two colors are the header's fixed
`background`/`foreground` (spec §3), so nothing about §5's payload changes if either were somehow
reconfigured mid-connection (they can't be — both are configuration-time-fixed).

## 6. Rendering cosmetics are the peripheral's responsibility

`compositing::composite` (device-side) produces a flat one-RGBA-pixel-per-dot buffer with no visual
polish — no gaps, no rounded corners, no dim "off" state. Per issue #569 and
`LcdDisplayPanel.tsx`'s own doc comment, that cosmetic dot-matrix rendering (rounded dots,
inter-dot and inter-cell gaps, a dimly-visible off state rather than flat background) deliberately
lives independently in each renderer rather than in the shared library — this protocol carries the
same undecorated raw buffer the debugger panel receives in-process, and a companion peripheral is
expected to apply its own equivalent cosmetic treatment using its own native drawing primitives
(the same split `LedMatrix`'s protocol and `emma65-led-matrix` already use for round-LED
rendering). A peripheral can distinguish an "on" dot from an "off" one the same way
`LcdDisplayPanel.tsx`'s `drawFrame` does: a pixel exactly equal to the header's `background` triple
is "off"; anything else is "on" (in practice, always exactly the header's `foreground` triple,
since `composite` draws only those two colors).

## 7. Startup state and reconnection

There is no reconnection support (the design assumes a single spawned child process tied to the
device's lifetime, mirroring `CharDisplay`'s and `LedMatrix`'s companion processes). A peripheral
that attaches sees no frame at all until the device's next render-affecting register write —
unlike the debugger panel, which can fetch a cached last-delivered frame on mount
(`get_lcd_display_frame`), there is no equivalent "replay the last frame" mechanism over this
protocol, so a freshly attached peripheral should render a blank grid (in `background`) until its
first frame arrives.

## 8. Non-goals

No protocol negotiation — a peripheral that doesn't recognize a header's `version` should refuse
to proceed rather than guess at a compatible framing. No inbound direction (this device has no
input capability at all — spec §2). No power/brightness/contrast messages (the HD44780 has no
such registers — spec §3, and unlike `LedMatrix`'s `CMD_SET_POWER`/`CMD_SET_BRIGHTNESS`, nothing in
this device's spec calls for them).
