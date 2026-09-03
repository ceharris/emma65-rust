# CLAUDE.md — `emma65-display`

An SDL2 desktop app (`emma65-display`) that renders a `display` device's composited
output for the plain `emma65` CLI standalone (no Tauri debugger); see
`plan/char-display-external-protocol.md` for the wire format it consumes. `emma65` spawns it as
a child process via a `display` device's `transport = "pipe:..."` attribute — its own
stdin is the pipe, so it is never run standalone against a live `emma65` process any other way.
`src/protocol.rs` is a decode-only mirror of `emma65::emulator::device::display::protocol`
(private to the `emma65` crate); `src/main.rs` reads the one-time header, then loops reading
fixed-size frames on a background thread (so SDL window-close events keep pumping even if
frames stall), compositing each with the `emma65` crate's own
`emulator::device::display::compositing::composite` (already `pub`, reused directly — no
rendering logic is duplicated) and blitting it via `SDL_UpdateTexture`/`SDL_RenderCopy`/
`SDL_RenderPresent`. Building it requires SDL2 development headers (`libsdl2-dev` on
Debian/Ubuntu); it is not built by plain `cargo build` — use `cargo build -p emma65-display`
or `cargo build --workspace`.
