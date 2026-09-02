# Running the Display Peripheral

`emma65-display` is an SDL2 window that renders a `display` device's
composited output when running the plain `emma65` CLI standalone (the
debugger doesn't need it — its own Display panel renders in-process). It's
not run directly against a live emulator process; instead, the emulator
spawns it as a child and streams frame data to it over the pipe transport's
stdin, per the [Character Display External Protocol](appendix-display-protocol.md).

Building it requires SDL2 development headers (`libsdl2-dev` on
Debian/Ubuntu, `sdl2` on Homebrew), the same way building the debugger
requires `gtk` on Linux:

```bash
cargo build --release -p emma65-display
```

Configure a `display` device with a `pipe:` transport pointing at the
built binary:

```toml
[[devices]]
type = "display"
address = 0xF000
transport = "pipe:/path/to/target/release/emma65-display"
```

```
emma65 --device display@0xF000,transport=pipe:/path/to/target/release/emma65-display
```

The window opens as soon as the emulator attaches the transport (immediately
on startup for a TOML/CLI-configured device), sized to the device's
configured grid at an initial `--scale` (default `3`, an integer multiple of
the native `columns*8` by `rows*8` pixel size); it remains resizable
afterward and letterboxes/scales to fit. Closing the window ends
`emma65-display`; it also exits cleanly if the emulator process exits or is
killed first, since that closes its stdin.
