# LCD Display External Protocol

Wire protocol [`LcdDisplay`](io-devices.md#character-lcd-display-displaylcd)
(config type `display/lcd`) uses to stream its composited frame data to an
external peripheral process — the bundled `emma65-lcd-display` binary, or a
replacement for it — over an attached [`Transport`](io-devices.md#transport-options),
when running the plain `emma65` CLI standalone. It's unrelated to the
debugger's in-process `LcdDisplayFrame`/`attach_frame_sink` push channel,
which needs no wire protocol at all (same address space, same process), and
unrelated to the [Character Display External Protocol](appendix-display-protocol.md)
and [LED Matrix External Protocol](appendix-led-matrix-protocol.md) — different
devices with different needs. Implemented in
`src/emulator/device/lcd_display/protocol.rs`; full historical design
rationale in `plan/lcd-display-external-protocol.md`. See
[Running the LCD Display Peripheral](running-the-lcd-display-peripheral.md)
for how to configure and launch `emma65-lcd-display` itself; see
`plan/memory-mapped-lcd-display-device-spec.md` for `LcdDisplay`'s bus-facing
register behavior — this page covers only what crosses the transport.

## Transport requirements

Exactly one connection, outbound only (device → peripheral) — like
`LedMatrix`, and unlike `CharDisplay`, this device has no input capability at
all. `Transport::send_bytes` must be atomic — either the whole buffer is
written or none of it is — which in practice means only `pipe:` is
supported; the config module rejects any other transport spec for a
`display/lcd` device at instantiation time.

## Message framing

The header is sent exactly once, immediately when the transport is attached.
Every subsequent message is a frame, sent whenever the device pushes a frame
— i.e. after every register write that could change what's rendered, with
no periodic cadence at all, unlike `CharDisplay`'s per-vsync or `LedMatrix`'s
per-swap sends.

Unlike both of those protocols, a frame here is **not** a fixed size for the
life of the connection: `Function Set`'s `F` bit can switch the active font
between 5×8 and 5×10 dots at any time, changing every subsequent frame's
pixel height. So each frame message carries its own `width_px`/`height_px`
fields rather than relying on the header alone to fix a size. There are
still no separate length prefixes or delimiters beyond those two fields: a
frame's total size is always exactly `4 + width_px * height_px * 4` bytes,
which the receiver can compute as soon as it has read those two fields. This
is only safe because of the transport atomicity requirement above: a
transport that could deliver a partial frame would desync the stream
permanently, with no way to resynchronize.

## Header (sent once, on attach)

| Field        | Type    | Size (bytes) | Notes                                                     |
|--------------|---------|--------------|------------------------------------------------------------|
| `magic`      | ASCII   | 4            | `"E65L"` — distinct from `"E65D"` (display) and `"E65M"` (LED matrix) |
| `version`    | `u8`    | 1            | `1`                                                        |
| `columns`    | `u8`    | 1            | configured character grid width                            |
| `rows`       | `u8`    | 1            | configured character grid height                           |
| `background` | RGB24   | 3            | `r`, `g`, `b` — configuration-time-fixed                    |
| `foreground` | RGB24   | 3            | `r`, `g`, `b` — configuration-time-fixed                    |

Total header size: 13 bytes. `columns`/`rows` are the character grid
dimensions, not pixel dimensions — a peripheral wanting pixel dimensions
ahead of the first frame can compute an upper bound (`columns * 5` by
`rows * 10`) but must still read each frame's own `width_px`/`height_px` to
render it, since the actual font in use isn't known until the first frame
arrives. `background`/`foreground` are carried here, not derived from frame
pixel data, so a peripheral can replicate the debugger panel's dot-matrix
cosmetics (see below) without having to reverse-engineer which composited
pixels are "on" vs. "off".

## Frame (sent whenever the device pushes a frame)

| Field       | Type     | Size (bytes)               | Notes                                                  |
|-------------|----------|-----------------------------|----------------------------------------------------------|
| `width_px`  | `u16` LE | 2                           | `columns * 5` (fixed glyph cell width)                    |
| `height_px` | `u16` LE | 2                           | `rows * 8` or `rows * 10`, depending on the active font   |
| `pixels`    | raw      | `width_px * height_px * 4` | RGBA, row-major, top row first, 4 bytes per pixel          |

Total message size: `4 + width_px * height_px * 4` bytes. The pixel data
sent is always exactly what the device's compositing produces for its
current DDRAM/CGRAM/CGROM/cursor/mode state — already fully composited
(background/foreground baked into each pixel, cursor drawn, blank when
`display_on` is false). There is no palette to separately transmit or
retain, unlike `CharDisplay`/`LedMatrix`: this device's only two colors are
the header's fixed `background`/`foreground`.

## Rendering cosmetics are the peripheral's responsibility

The device-side compositing produces a flat one-RGBA-pixel-per-dot buffer
with no visual polish — no gaps, no rounded corners, no dim "off" state.
Per issue #569, that cosmetic dot-matrix rendering (rounded dots, inter-dot
and inter-cell gaps, a dimly-visible off state rather than flat background)
deliberately lives independently in each renderer rather than in the shared
library — this protocol carries the same undecorated raw buffer the
debugger panel receives in-process, and a companion peripheral is expected
to apply its own equivalent cosmetic treatment using its own native drawing
primitives (the same split `LedMatrix`'s protocol and `emma65-led-matrix`
already use for round-LED rendering). A peripheral can distinguish an "on"
dot from an "off" one the same way the debugger panel does: a pixel exactly
equal to the header's `background` triple is "off"; anything else is "on"
(in practice, always exactly the header's `foreground` triple).

## Startup state and reconnection

There is no reconnection support — the design assumes a single spawned
child process tied to the device's lifetime, mirroring `CharDisplay`'s and
`LedMatrix`'s companion processes. A peripheral that attaches sees no frame
at all until the device's next render-affecting register write — unlike the
debugger panel, which can fetch a cached last-delivered frame on mount,
there is no equivalent "replay the last frame" mechanism over this
protocol, so a freshly attached peripheral should render a blank grid (in
`background`) until its first frame arrives.

## Non-goals

No protocol negotiation — a peripheral that doesn't recognize a header's
`version` should refuse to proceed rather than guess at a compatible
framing. No inbound direction (this device has no input capability at
all). No power/brightness/contrast messages (the HD44780 has no such
registers, and unlike `LedMatrix`'s power/brightness messages, nothing in
this device's spec calls for them).
