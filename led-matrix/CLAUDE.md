# CLAUDE.md — `emma65-led-matrix`

An SDL2 desktop app (`emma65-led-matrix`) that renders a `display/matrix` device's per-matrix
composited output for the plain `emma65` CLI standalone (no Tauri debugger); see
`plan/led-matrix-external-protocol.md` for the wire format it consumes. `emma65` spawns it as a
child process via a `display/matrix` device's `transport = "pipe:..."` attribute — its own
stdin is the pipe, so it is never run standalone against a live `emma65` process any other way.
`src/protocol.rs` is a decode-only mirror of `emma65::emulator::device::led_matrix`'s (private)
encode side — same split as `display/src/protocol.rs` mirrors `display::protocol`, but with
tagged, variable-arrival messages (a block message per matrix swap, a palette message per
`CMD_PALETTE_WRITE`) rather than one fixed-size frame per vsync, since `LedMatrix` swaps happen
per matrix rather than in lockstep. `src/main.rs` reads the one-time header, then loops decoding
the tagged message stream on a background thread; each matrix's most recently received raw pixel
indices are retained and recomposited against the current palette on every redraw via the
`emma65` crate's own `emulator::device::led_matrix::compositing::composite_matrix` (already
`pub`, reused directly), drawn as filled circles on a PCB-colored background via SDL2_gfx's
`DrawRenderer`, flush per `--arrangement COLSxROWS`. Building it requires SDL2 development
headers (`libsdl2-dev` on Debian/Ubuntu); it is not built by plain `cargo build` — use
`cargo build -p emma65-led-matrix` or `cargo build --workspace`.
