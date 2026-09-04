# I/O Devices

Eleven built-in devices implement the `IoDevice` trait. Eight — Console,
R6551, Mc6850, Via6522, Mc6840, LedMatrix, CharDisplay, and Lfsr16 — are
register-window devices that can be mapped into any address range on the
bus; each integrates with the interrupt controller via `irq_active()` and
`take_nmi()` (CharDisplay is the one exception — see below), and most of
them exchange data with the outside world over a configurable
[Transport](#transport-options). The other three — Finch, Phoebe, and Vireo —
are complete bank-switched memory subsystems that occupy the entire 64 KB
address space in place of separate RAM/ROM regions; see
[Bank-Switched Memory Modules](#bank-switched-memory-modules) below.

## Console (`console`)

A simple polling console device for byte-stream I/O over a configurable
[Transport](#transport-options):

- Input buffering via a 64 kilobyte ring buffer
- Two addressable registers: data input/output (offset 0) and data
  latch (offset 1)
- The data latch register latches an incoming byte in a single read, providing
  a non-blocking one-byte look-ahead and making it easy to write polling loops
  without separate status and data registers
- Support for configuring a break key code (e.g. ASCII Ctrl+C) which, when
  recognized in input from the transport, drains the input buffer, latches
  the break key code, and asserts the CPU's IRQ signal
- Reading the data or latch register clears interrupt status. Writing the
  break key code simulates break key input under program control. Writing any
  other value to the latch clears interrupt status, drains the input
  buffer, and stores the value in the latch register for subsequent read
  (useful for simulating input under program control).
- Designed as the backend for the debugger's built-in terminal emulator

## 6522 Versatile Interface Adapter (`via/6522`)

A comprehensive implementation of the WDC 65C22 Versatile Interface 
Adapter (VIA):

- All 16 addressable registers (offsets `$0`–`$F`)
- Two independent 8-bit I/O ports (A and B), each with a data direction
  register
- All handshaking and latching modes fully supported
- CA1, CA2, CB1, CB2 control lines with configurable edge and level triggering
  via PCR
- Timer 1 (one-shot or free-run, with optional PB7 square-wave output) and
  Timer 2
  (one-shot or pulse counting)
- Shift register with seven configurable modes (input or output; T2, PHI2, or
  external clock)
- Full IFR/IER interrupt flag and enable registers with independent masking
  per source

The VIA uses a GPIO communication protocol (see the
[VIA Peer Protocol](appendix-via-protocol.md) appendix) over any attached
[`Transport`](#transport-options) to exchange port state and control signal
transitions with real or emulated peripherals. On connection the VIA sends a
full state dump so the peripheral starts with an accurate picture of all
pins and control lines.

## MC6840 Programmable Timer Module (`ptm/6840`)

A comprehensive implementation of the Motorola MC6840 Programmable Timer 
Module (PTM).

- Three independent timers supporting continuous or single-shot 
  generation modes as well as frequency/period or pulse width measurement 
  modes
- Connects to a virtual peripheral over any [Transport](#transport-options)
- Support for external gate and clock inputs and timer output

The PTM uses a communication protocol (see the
[PTM Peer Protocol](appendix-ptm-protocol.md) appendix) over any attached
[`Transport`](#transport-options) to exchange port state and control signal
transitions with real or emulated peripherals. On connection, the PTM sends
a full state dump so the peripheral starts with an accurate picture of all
pins and control lines.

## MC6850 Asynchronous Communications Adapter (`acia/6850`)

An comprehensive implementation of the Motorola MC6850 Asynchronous 
Communications Interface Adapter (ACIA):

- Two addressable registers: status/control and RX/TX data
- RDRF and TDRE status with IRQ support for both receive and transmit
- Master reset via control register bits
- TX is immediate: bytes are forwarded to the transport on write; TDRE is
  restored on the next CPU tick 

Connects to a virtual peripheral over any [Transport](#transport-options).

## R6551 Asynchronous Communication Adapter (`acia/6551`)

An implementation of the Rockwell 6551 Asynchronous Communications Interface
Adapter (ACIA):

- Four addressable registers: RX data, TX data, status, and command/control
- RDRF (Receive Data Register Full) and TDRE (Transmit Data Register Empty)
  status bits
- Interrupt-driven I/O with separate RX and TX interrupt enables
- Baud rate selection from the control register; external-clock mode polls the
  transport on every CPU tick for maximum responsiveness
- Hardware bug–compatible mode (`R6551::with_tdre_bug()`) keeps TDRE
  permanently set, matching the behavior of the WDC 65C51 variant for software
  that uses timed delays rather than TDRE polling

Connects to a virtual peripheral over any [Transport](#transport-options).

## RGB LED Matrix Display (`display/matrix`)

A memory-mapped RGB LED matrix display supporting 1, 2, 4, or 8 attached
32×32 matrices, fixed at configuration time:

- Pixel memory is mapped directly into the address space, one byte per
  pixel, as a single row-major raster of the composed canvas (see
  `arrangement` below) — no per-pixel register bottleneck for bulk updates
- Each pixel byte indexes a single, shared 256-entry color palette; palette
  entries store 16-bit RGB565 colors (matching real LED matrix driver
  hardware's color depth), organized like the Xterm 256-color palette (16
  named colors, a 6×6×6 color cube, and a 24-level grayscale ramp) by
  default
- Double-buffered per matrix: pixel writes target an off-screen buffer,
  exchanged with the visible one via a command/data register pair — either
  explicitly (a swap command) or automatically via a per-matrix,
  dirty-gated auto-refresh cadence
- A single command/data register pair drives every operation (swap,
  auto-refresh mask, power, brightness, palette read/write) — there is no
  per-drawing-primitive register; not IRQ-capable, since swaps are always
  synchronous

See `plan/memory-mapped-led-matrix-device-spec.md` in the repository for the
full register-level specification. Like `display`, `display/matrix` has no
in-process console-style rendering when running the plain `emma65` CLI:

- **The debugger** — the LED Matrix panel renders each matrix as an
  independent, composited canvas in-process, no configuration needed.
- **Standalone `emma65`** — configure a `pipe:` transport pointing at the
  bundled `emma65-led-matrix` SDL2 peripheral binary (see
  [Running the LED Matrix Peripheral](running-the-led-matrix-peripheral.md)
  below). A block message streams per matrix swap, and a palette message
  streams per palette write, over the
  [LED Matrix External Protocol](appendix-led-matrix-protocol.md).

```toml
[[devices]]
type = "display/matrix"
address = 0x9000
register-address = 0x9400
arrangement = "2x2"
transport = "pipe:/path/to/emma65-led-matrix"
```

`arrangement` (required, `COLSxROWS`, e.g. `2x2`) describes how the matrices
are physically daisy-chained: the matrix count (`columns * rows`) and how bus
addresses map onto them. The composed canvas is `columns * 32` pixels wide by
`rows * 32` pixels tall, addressed like a real framebuffer (byte
`row * width + col`), with matrix *n* occupying the `32x32` sub-rectangle at
`((n / columns) * 32, (n % columns) * 32)`. There is no separate
`matrix-count` attribute — a bare count doesn't say how the matrices are
wired, and having both invited them to silently disagree. A `1xN` (single
column) arrangement reproduces the original one-matrix-per-1024-contiguous-
bytes layout.

## Character LCD Display (`display/lcd`)

A memory-mapped character LCD module emulating a Hitachi HD44780-compatible
controller/driver, faithfully reproducing its real two-register bus
interface rather than mapping display memory directly:

- A 2-byte register pair (instruction/status and data), regardless of
  configured geometry — exactly like a real HD44780, all display state
  (DDRAM, CGRAM, address counter) is reached only indirectly through these
  two registers
- Command execution takes simulated time, reported via a busy flag on the
  instruction register, matching real HD44780 timing so programs written
  against real hardware assumptions behave the same way here
- Supports both the 8-bit and "software enabled" 4-bit interface widths,
  selected at runtime via `Function Set`, including the classic 5×8/5×10
  font height switch
- Geometry (rows × columns) is fixed at configuration time from a set of
  real-world HD44780 module layouts, quirks (like 16x1's split-segment
  addressing) included
- Not IRQ-capable — the HD44780 interface has no interrupt output

See `plan/memory-mapped-lcd-display-device-spec.md` in the repository for
the full register-level specification. Like `display` and `display/matrix`,
`display/lcd` has no in-process console-style rendering when running the
plain `emma65` CLI:

- **The debugger** — the LCD Display panel renders composited frames
  in-process, no configuration needed.
- **Standalone `emma65`** — configure a `pipe:` transport pointing at the
  bundled `emma65-lcd-display` SDL2 peripheral binary (see
  [Running the LCD Display Peripheral](running-the-lcd-display-peripheral.md)
  below). A frame streams on every register write that could change what's
  rendered, over the
  [LCD Display External Protocol](appendix-lcd-display-protocol.md).

```toml
[[devices]]
type = "display/lcd"
address = 0xD000
geometry = "16x2"
transport = "pipe:/path/to/emma65-lcd-display"
```

`geometry` (optional, default `16x2`) selects one of the nine supported
real-world module layouts: `8x1`, `8x2`, `16x1`, `16x2`, `16x4`, `20x2`,
`20x4`, `40x1`, `40x2`. `cgrom` (optional) selects the bundled character
generator ROM by name -- `a00` (the default, and the ROM code most HD44780
clones ship with) or `a02` (the European-font variant), case-insensitive --
or overrides it with a file of the same format.

`polarity` (optional, default `positive`) and `backlight` (optional, default
`yellow`) together select one of 8 color-scheme presets modeling
commonly available real LCD modules, rather than requiring hand-picked RGB24
values: `positive` polarity renders dark pixels over a backlight-colored
background; `negative` polarity renders backlight-colored pixels over a dark
"opaque near-black" background. Not every `backlight` value is valid for
every `polarity` — only the combinations below are:

| `polarity`  | `backlight` |
|-------------|-------------|
| `positive`  | `yellow`    |
| `positive`  | `white`     |
| `positive`  | `amber`     |
| `positive`  | `blue`      |
| `negative`  | `blue`      |
| `negative`  | `white`     |
| `negative`  | `amber`     |
| `negative`  | `red`       |

`background`/`foreground` (optional, hex RGB24) remain available for fully
custom colors — each, if given, overrides the corresponding channel of the
`polarity`/`backlight` preset. None of these are part of the HD44780's own
behavior, and none are bus-addressable.

## Character Display (`display`)

A memory-mapped character/color-cell text display, with a configurable grid
size (default 40×25 cells), an 8×8 glyph font, and a runtime-writable color
palette:

- Character RAM and color RAM, one byte per cell each — a glyph index and a
  palette index, respectively
- Double-buffered like `display/matrix`: writes target an off-screen buffer,
  swapped to visible either on request or automatically on every vsync
- Runtime palette updates via a small armed write sequence to the
  status/data register (index, red, green, blue)
- Not IRQ-capable — nothing in its register map asserts an interrupt

See `plan/memory-mapped-display-device-spec.md` in the repository for the
full register-level specification. Unlike the other register-window devices,
`display` has no in-process console-style rendering when running the plain
`emma65` CLI: it needs an external peripheral to actually put pixels on
screen. Two ways to view it:

- **The debugger** — the Display panel renders composited frames in-process,
  no configuration needed.
- **Standalone `emma65`** — configure a `pipe:` transport pointing at the
  bundled `emma65-display` SDL2 peripheral binary (see
  [Running the Display Peripheral](running-the-display-peripheral.md) below).
  Composited frame data (char RAM, color RAM, palette, and the font) streams
  to the peripheral once per vsync over the
  [Character Display External Protocol](appendix-display-protocol.md).

```toml
[[devices]]
type = "display"
address = 0xF000
columns = 40
rows = 25
transport = "pipe:/path/to/emma65-display"
```

## 16-bit Galois LFSR (`lfsr`)

A memory-mapped pseudo-random number generator based on a 16-bit Galois
linear-feedback shift register (default tap mask `0xB400`, a maximal-length
65535-state sequence):

- 2 addressable registers exposing the current LFSR state
- **Continuous** mode advances the register automatically as part of normal
  execution; **step** mode advances only when explicitly clocked, for
  reproducible pseudo-random sequences under program control

## Bank-Switched Memory Modules

Finch, Phoebe, and Vireo are complete memory subsystems — RAM, ROM, and a
bank-switching MMU — rather than register-window devices. Each claims the
entire 64 KB address space when configured, so no separate `ram`/`rom`
entries are needed alongside them, and their `address` device-spec field is
unused. All three support an optional ROM `write-policy` (`ignore` or
`error`), an `image` loaded at an optional `offset`, and an optional VICE
`labels` file for symbol resolution.

### Finch bank-switched MMU (`mem/finch`)

512 KB RAM and 512 KB ROM behind a simple MMU: the top four bits of the 6502
address bus (`A12..A15`) index into 16 one-byte bank registers, each
selecting which 4 KB segment of the module's 1024 KB memory space is mapped
into that 4 KB window of the 6502's address space. Two memory-mapped
registers (configurable addresses) control the bank registers and other MMU
functions.

### Phoebe bank-switched memory (`mem/phoebe`)

56 KB RAM and 32 KB ROM. The ROM is split into four 8 KB banks; bank 3 is
permanently mapped into the upper half of a 16 KB switchable region at
`0xC000` (and must contain the 6502 machine vectors), while a single
memory-mapped control register selects which of banks 0–2 (or none, exposing
the underlying RAM instead) occupies the lower half.

### Vireo bank-switched memory (`mem/vireo`)

128 KB RAM and 32 KB ROM behind an elegant bank-switching scheme supporting
four configurations — from a plain 32 KB RAM / 32 KB ROM split up to modes
that expose additional RAM banks beyond the 64 KB address space — selected
via a single memory-mapped control register.

## Transport Options

Devices that exchange byte streams attach a `Transport`. Configurable via TOML/CLI:

| Transport             | Shorthand                       | Best for                                                              |
|------------------------|---------------------------------|-------------------------------------------------------------------------|
| `PipeTransport`        | `pipe:/path/to/exe,arg1,arg2`   | Spawning a child process and bridging its stdin/stdout to the device    |
| `TcpSocketTransport`   | `tcp:PORT` or `tcp:IP:PORT`     | Connecting a terminal emulator or remote process over the network       |
| `UnixSocketTransport`  | `unix:PATH`                     | Low-latency local IPC (lower overhead than TCP)                         |
| `PtyTransport`         | `pty` or `pty:SYMLINK_PATH`     | Any program that expects a real TTY — `screen`, `minicom`, `cu`, etc.   |

A fifth implementation, `InternalPipeTransport`, isn't configured via
TOML/CLI — the `emma65` binary and the debugger UI use it internally to wire
a console device directly to the host process's own stdin/stdout (CLI) or
terminal window (debugger) when no `transport` attribute is given.

Every transport's actual I/O runs on its own thread or async task, decoupled
from device `tick()` by a lock-free `rtrb` ring buffer (`ChannelRelay`): the
transport side pushes into the ring as bytes arrive, and `tick()` drains
whatever is currently available and returns immediately, whether that's
nothing, one byte, or a burst. Neither side ever blocks the other. Because
the CPU thread is practically unburdened by communication with external
peripherals, it can easily sustain common effective clock speeds — and much
higher ones with clock throttling disabled (`ClockSpeed::unlimited()`).

The VIA and MC6840 additionally support framing their transport traffic with
a structured peer-communication protocol (`protocol = "ascii"` or `"binary"`)
that exchanges full port/pin state on connection and incremental updates
thereafter, so a real or emulated peripheral always has an accurate picture
of the device's signals — see the [VIA Peer Protocol](appendix-via-protocol.md)
and [PTM Peer Protocol](appendix-ptm-protocol.md) appendices.
