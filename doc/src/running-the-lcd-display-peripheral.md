# Running the LCD Display Peripheral

`emma65-lcd-display` is an SDL2 window that renders a `display/lcd` device's
composited dot-matrix output when running the plain `emma65` CLI standalone
(the debugger doesn't need it — its own LCD Display panel renders
in-process). Like `emma65-display` and `emma65-led-matrix`, it's spawned by
the emulator as a child process and streams data to it over the pipe
transport's stdin, per the
[LCD Display External Protocol](appendix-lcd-display-protocol.md).

Building it requires SDL2 development headers (`libsdl2-dev` on
Debian/Ubuntu, `sdl2` on Homebrew), the same as `emma65-display` and
`emma65-led-matrix`:

```bash
cargo build --release -p emma65-lcd-display
```

Configure a `display/lcd` device with a `pipe:` transport pointing at the
built binary:

```toml
[[devices]]
type = "display/lcd"
address = 0xD000
geometry = "16x2"
transport = "pipe:/path/to/target/release/emma65-lcd-display"
```

```
emma65 --device display/lcd@0xD000,geometry=16x2,transport=pipe:/path/to/target/release/emma65-lcd-display
```

The window opens as soon as the emulator attaches the transport, showing a
blank dot-matrix grid in the device's configured background color before
any writes occur. `--pitch` sets the initial on-screen dot center-to-center
spacing in pixels (default `12`); the window remains resizable afterward and
letterboxes/scales to fit. Closing the window ends `emma65-lcd-display`; it
also exits cleanly if the emulator process exits or is killed first, since
that closes its stdin.

The window resizes on the fly when `Function Set`'s `F` bit switches the
active font between 5×8 and 5×10 dots, since that changes every subsequent
frame's pixel height — no configuration is needed for this, it follows
automatically from each frame message's own dimensions.
