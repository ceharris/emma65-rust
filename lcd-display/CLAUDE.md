# CLAUDE.md — `emma65-lcd-display`

An SDL2 desktop app (`emma65-lcd-display`) that renders a `display/lcd` device's composited
output for the plain `emma65` CLI standalone (no Tauri debugger); see
`plan/lcd-display-external-protocol.md` for the wire format it consumes. `emma65` spawns it as a
child process via a `display/lcd` device's `transport = "pipe:..."` attribute — its own stdin is
the pipe, so it is never run standalone against a live `emma65` process any other way.
`src/protocol.rs` is a decode-only mirror of `emma65::emulator::device::lcd_display`'s (private)
encode side — same split as `display/src/protocol.rs`/`led-matrix/src/protocol.rs` mirror their
devices' encode sides, but each frame here carries its own `width_px`/`height_px` (spec §5) since
`Function Set`'s `F` bit can change a frame's pixel height at runtime, so decoding a frame is a
two-step read (dimension prefix, then the now-known-length pixel payload) rather than one
fixed-size read. Unlike `emma65-display`/`emma65-led-matrix`, `src/main.rs` does no compositing at
all — `LcdDisplay`'s frame sink already produces a fully composited flat RGBA buffer, reused
as-is — but it does port `LcdDisplayPanel.tsx`'s dot-matrix cosmetics (rounded dots, inter-dot and
inter-cell gaps, a dimly-blended "off" state; issue #569) to SDL2 with its own native primitives,
since that cosmetic layer deliberately isn't shared in the library (spec §6). Dots are drawn with
SDL2_gfx's `DrawRenderer::rounded_box`, routed through the same `gfx_color` native-byte-order fix
`emma65-led-matrix` needs (see that crate's `CLAUDE.md` and
`plan/led-matrix-companion-binary-plan.md` for the underlying bug). The window resizes when a
frame's dimensions change (a font switch); before the first frame arrives it shows a blank
`background` grid at an assumed 5x8 font (spec §7 — there is no cached-last-frame replay over this
protocol). Building it requires SDL2 development headers (`libsdl2-dev` on Debian/Ubuntu); it is
not built by plain `cargo build` — use `cargo build -p emma65-lcd-display` or
`cargo build --workspace`.
