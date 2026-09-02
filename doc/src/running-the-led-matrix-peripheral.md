# Running the LED Matrix Peripheral

`emma65-led-matrix` is an SDL2 window that renders a `display/matrix`
device's per-matrix composited output when running the plain `emma65` CLI
standalone (the debugger doesn't need it — its own LED Matrix panel renders
in-process). Like `emma65-display`, it's spawned by the emulator as a child
process and streams data to it over the pipe transport's stdin, per the wire
protocol in `plan/led-matrix-external-protocol.md` in the repository.

Building it requires SDL2 development headers (`libsdl2-dev` on
Debian/Ubuntu, `sdl2` on Homebrew), the same as `emma65-display`:

```bash
cargo build --release -p emma65-led-matrix
```

Configure a `display/matrix` device with a `pipe:` transport pointing at the
built binary:

```toml
[[devices]]
type = "display/matrix"
address = 0x9000
arrangement = "1x4"
register-address = 0x9400
transport = "pipe:/path/to/target/release/emma65-led-matrix"
```

```
emma65 --device display/matrix@0x9000,arrangement=1x4,register-address=0x9400,transport=pipe:/path/to/target/release/emma65-led-matrix
```

The window opens as soon as the emulator attaches the transport, showing the
configured matrices side by side as round LEDs on a PCB-colored background,
flush against each other. `--arrangement COLSxROWS` lays the matrices out in
a grid instead of a single row (must be a divisor pair of the configured
matrix count, e.g. `2x2` for 4 matrices); `--pitch` sets the initial
on-screen LED center-to-center spacing in pixels (default `12`). The window
remains resizable afterward and letterboxes/scales to fit. Closing the
window ends `emma65-led-matrix`; it also exits cleanly if the emulator
process exits or is killed first, since that closes its stdin.

This `--arrangement` flag only controls on-screen layout and is independent
of the device's own `arrangement` config attribute, which controls bus
addressing (see [RGB LED Matrix Display](io-devices.md#rgb-led-matrix-display-displaymatrix)
in I/O Devices) — matrix *n*'s content is correct either way, but the two should
normally be set to the same `COLSxROWS` value so the picture on screen
matches the physical layout the program was written for.
