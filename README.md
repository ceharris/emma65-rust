# emma65

[![CI](https://github.com/ceharris/emma65-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/ceharris/emma65-rust/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Emma65 is a software emulator for the 65C02-family of 8-bit microprocessors.
It provides a complete execution environment suitable for running and
debugging programs written for classic 65C02-based systems, with support for
flexible memory configuration, a rich set of virtual I/O devices, and
expression-based watchpoints. The project ships five tools built on the same
emulator core:

- **`emma65`** — a command-line emulator for running programs directly
- **`emma65-debugger`** — a graphical debugger (registers, disassembly,
  memory, stack, watchpoints, and a live execution trace, in a native desktop
  app) for interactively developing and troubleshooting programs
- **`emma65-tracer`** — a utility that decodes a recorded binary execution
  trace into a human-readable, symbol-annotated disassembly listing
- **`emma65-display`** — an SDL2 peripheral process that renders the
  character display device (`display`) in its own window when running
  `emma65` standalone (no debugger)
- **`emma65-led-matrix`** — an SDL2 peripheral process that renders the RGB
  LED matrix device (`display/matrix`) in its own window when running
  `emma65` standalone (no debugger)

Together they form a foundation for building retro-computing tools,
educational simulators, and hardware-in-the-loop test rigs.

## The Debugger

`emma65-debugger` is a native desktop application (built with
[Tauri](https://tauri.app)) that turns the emulator into a full interactive
development environment for 65C02 programs:

- **Load and run code in multiple execution modes** — free-run, single-step
  (step into), step over a subroutine call, and step out (step return) — all
  driven from a live, symbol-annotated Disassembly panel that tracks the
  program counter as it executes
- **Full breakpoint support** — set, enable, disable, and remove breakpoints
  directly against the disassembly listing
- **Full watchpoint support** — write expression-based watchpoints (see
  [Watchpoint Expressions](#watchpoint-expressions) below) in the Watchpoint
  panel; add, edit, remove, and toggle them with a click, and see at a
  glance which are currently triggered
- **View and modify memory and registers live** — browse and edit memory a
  page at a time, fill ranges, and load a program image from a file in the
  Memory panel; view and edit every CPU register in the Register panel
- **Trigger interrupts on demand** — manually assert or release IRQ and
  trigger NMI from the CPU/Bus panel, alongside a full CPU reset, to exercise
  interrupt handlers without needing real hardware events
- **Live execution trace** — a dedicated Trace window shows a scrolling,
  real-time view of recently executed instructions, recorded via the same
  facility described in [Execution Tracing](#execution-tracing)
- **Built-in terminal** — an Xterm/VT220-compatible terminal window wired
  directly to the configured, memory-mapped console device, so you can
  interact with a running program without any external terminal emulator or
  PTY setup

The debugger reads its emulator configuration from
`~/.emma/debugger/profiles/default/emulator.toml` (the same TOML format
described under [Running the Emulator](#running-the-emulator)); watchpoints
are stored alongside it as `watchpoints.emw`. Its own UI preferences —
including light/dark theme — are not specific to any profile, and are read
from `~/.emma/debugger/config/ui.toml` instead.

### Watchpoint Expressions

Watchpoints are boolean expressions evaluated against live machine state
before each instruction; each line of `watchpoints.emw` is one watchpoint,
and the Watchpoint panel shows whether it's currently triggered. The
expression language covers:

- **Registers** — `A`, `X`, `Y`, `P`, `S`, `PC`
  ```
  X > 10
  PC == $8010
  ```
- **CPU status flags**, prefixed with a backtick — `` `N ``, `` `V ``,
  `` `B ``, `` `D ``, `` `I ``, `` `Z ``, `` `C ``
  ```
  `C
  `N && `Z
  ```
- **Literals** — decimal, or hex with a `$` or `0x` prefix (`0o`/`0q` octal
  and `0b` binary are also recognized)
  ```
  A == 42
  A == $2A
  ```
- **Memory operands** — `B[addr]`, `W[addr]`, `D[addr]` read a byte, word, or
  doubleword from memory; a leading `+` or `-` interprets the value as signed
  (`-` also negates it)
  ```
  B[$0200] == $FF
  +B[$D010] < 0    // true when bit 7 (the sign bit) of the byte at $D010 is set
  W[$FE] != 0
  ```
- **Symbols** — a bare identifier resolves to the address of a label loaded
  from a VICE-format label file (the `labels` device attribute), so a
  watchpoint can reference a source-level name instead of a hardcoded address
  ```
  PC == reset_vector
  B[cursor_x] > 79
  ```
- **Arithmetic, bitwise, and comparison operators** — `+ - * / %`,
  `& | ^ ~`, `<< >>`, `== != < <= > >=`, `&& || !`
  ```
  (B[$D010] & $80) != 0
  ```
- **The walrus operator (`:=`)** snapshots a value into a named variable that
  persists across steps, so one watchpoint can be compared against a value
  captured on an earlier step
  ```
  A != x    // triggers once A differs from the value snapshotted below
  x := A    // snapshot this step's A for comparison on the next step
  ```

Expressions are compiled to bytecode once, at load time, and evaluated
efficiently on every step, making it practical to run many watchpoints
simultaneously.

Build and run the debugger from `debugger/src-tauri` with the
[Tauri CLI](https://tauri.app/develop/) (`cargo tauri dev` for development,
`cargo tauri build` for a packaged release); this drives an `npm run build`
of the `debugger/frontend` React/TypeScript UI automatically.

## The Emulator Core

At the heart of Emma65 is a CPU model that faithfully emulates the 65C02
instruction set and interrupt behavior, paired with a flexibly configurable
memory bus and a growing library of virtual I/O devices — everything the
`emma65` command-line emulator, the debugger, and the tracer are all built
on. Memory and devices are mapped into the 16-bit address space however a
program needs them, devices talk to real or emulated peripherals over
pluggable transports, and execution can be inspected and controlled through
expression-based watchpoints and a recorded instruction trace. The following
sections describe this core in detail, starting with how closely it matches
real 65C02 hardware.

### Correctness

Emma65 passes
the [Klaus Dormann 65C02 test suite](https://github.com/Klaus2m5/6502_65C02_functional_tests),
which exhaustively exercises every instruction, addressing mode, flag
computation, interrupt sequence, and decimal-mode operation defined by the
65C02 architecture. It also passes
the [Bruce Clark decimal mode test](http://www.6502.org/tutorials/decimal_mode.html),
which independently verifies all 256×256 ADC and SBC operand combinations in
BCD mode against predicted CMOS 65C02 results. Users can rely on Emma65's
instruction-level behavior matching real hardware.

### Features

#### Instruction Set

Emma65 emulates two variants of the 65C02 processor family:

- **CMOS 65C02** — the standard CMOS variant, including all instructions added
  over the original NMOS 6502: `BRA`, `STZ`, `TSB`, `TRB`, `PHX`, `PHY`,
  `PLX`, `PLY`, accumulator-mode `INC` and `DEC`, zero-page indirect
  addressing, and `JMP (abs,X)`.

- **WDC 65C02** — the Western Design Center variant, which adds 34 opcodes to
  the CMOS baseline: `STP` (stop the processor), `WAI` (wait for interrupt),
  `BBR0`–`BBR7` and
  `BBS0`–`BBS7` (branch on bit clear/set), and `RMB0`–`RMB7` and `SMB0`–`SMB7`
  (reset/set memory bit).

All 16 addressing modes are supported, including the zero-page relative mode
used by the WDC bit-branch instructions. Invalid opcodes can be configured to
either silently act as NOPs or to halt execution with an error.

#### Interrupt Support

Emma65 implements the full 65C02 interrupt model:

- **RESET** — restores the CPU to its power-on state: every device on the
  bus receives an `IoDevice::reset()` call, the stack pointer is set to
  `$FF`, the status register to `I` (interrupts disabled, every other flag
  clear), the cumulative cycle counter is zeroed, and any `STP`/`WAI`-halted
  state is cleared. The program counter is then loaded from the reset vector
  at `$FFFC`/`$FFFD`. Emma65 issues one automatically before running the
  first instruction of a session, and the debugger's CPU/Bus panel exposes
  it as an on-demand control.
- **NMI** — edge-triggered and latched: the first falling edge sets a pending
  flag that is consumed exactly once, with highest priority over simultaneous
  IRQ. Any device can signal an NMI by implementing `IoDevice::take_nmi()`.
- **IRQ** — level-triggered and multi-source: multiple devices can
  independently assert and release the IRQ line; the interrupt fires when any
  source is active and the I flag is clear. Each device's IRQ state is polled
  after every instruction.
- **BRK** — software interrupt; sets the B flag in the pushed status byte so
  interrupt handlers can distinguish a BRK from a hardware IRQ.

On interrupt entry the D flag is cleared, matching CMOS 65C02 hardware
behavior.

#### Clock Speed Simulation

Free-running execution throttles to a configurable target clock frequency by
comparing accumulated emulated cycles against elapsed wall time, sleeping as
needed to match the target rate. Throttling is batched over roughly 1,000
instructions at a time, keeping sleep-syscall overhead negligible while
maintaining sub-millisecond timing granularity. Tested and accurate up to
approximately 2 MHz on typical hardware, covering the clock speeds of all
historically common 6502-based systems.

```rust
ClockSpeed::mhz(1.0)       // 1 MHz — Apple II speed
ClockSpeed::mhz(1.8432)    // 1.8432 MHz — common UART baud-rate crystal
ClockSpeed::mhz(2.0)       // 2 MHz — BBC Micro speed
ClockSpeed::unlimited()    // Maximum throughput; no throttling
```

#### Memory and Bus Configuration

The memory bus is organized around named address regions mapped into the
16-bit address space. Regions can be RAM, ROM (write-protected), or I/O device
windows. The bus uses a most-specific-wins overlap policy: a smaller region
always shadows a larger one at the same addresses, which makes it easy to
place a device register window inside a ROM region. Ambiguous overlaps (
same-size regions at the same addresses) and ROM size mismatches are caught at
build time.

```rust
let bus = Bus::config()
    .ram(AddressRange::new(0x0000, 0x7FFF)) ?
    .rom(AddressRange::new(0xC000, 0xFFFF), rom_data) ?
    .device(AddressRange::new(0xDF00, 0xDF0F), DeviceId(1), Box::new(my_device)) ?
    .build();
```

`.build()` resolves every one of the 65,536 possible addresses to its
most-specific region exactly once, consulting `IoDevice::claims()` on each
overlapping device candidate along the way to settle any conditional
chip-select. That one-time resolution is cached in a flat lookup table, so
every read or write the CPU subsequently performs is a single array index —
no walking the configured regions and no re-consulting `claims()` at
runtime — keeping bus access overhead effectively constant regardless of how
many devices are configured, out of the way of maximum emulated CPU
throughput.

Bus errors (unmapped reads/writes, ROM write violations) are surfaced through
`StepResult::Error` so the host application can decide how to respond.

#### Memory-Mapped I/O Devices

Devices are mapped onto the bus with the same builder call used for RAM and
ROM regions — `BusConfig::device(AddressRange, DeviceId, Box<dyn IoDevice>)`
— so a device window is subject to the same build-time overlap checking. The
built-in `ram`, `rom`, `console`, and other `type`s configurable from
TOML/CLI (see [Running the Emulator](#running-the-emulator)) are themselves
just `DeviceModule` implementations that make this same call: each is
registered by name in a `DeviceRegistry`, and `Config::build()` instantiates
one per `[[devices]]` entry (or `--device` flag) as it walks the configured
device list at startup. A custom device plugs into this exact same
configuration surface — see
[Adding a Custom Device Module](#adding-a-custom-device-module) under For
Contributors.

Custom devices implement the `IoDevice` trait. Only three methods are
required:

```rust
/// Read and return a byte from the specified absolute `address`.
fn read(&mut self, address: u16) -> u8;
/// Write a byte to the specified absolute `address`.
fn write(&mut self, address: u16, value: u8);
/// Read and return a byte from the specified absolute `address` while
/// inhibiting side effects; used by the debugger.
fn peek(&self, address: u16) -> u8;
```

A handful of further methods, each with a no-op default, give a device the
rest of what it needs to behave like real hardware:

- **Timing** — `tick(cycles: u32)` is called once per instruction, right
  after it completes, with the number of clock cycles that instruction
  actually took. A device advances its own internal timers and counters by
  exactly that many cycles, keeping it in lock-step with CPU time without
  being invoked on every single cycle; `Via6522`'s two timers and
  `Mc6840`'s three are both built on this.
- **Interrupts** — `irq_active()` is polled after every instruction to
  report whether the device is currently asserting the shared IRQ line, and
  `take_nmi()` is called once per instruction to consume a pending NMI edge
  (an implementation sets an internal flag on the triggering event and
  clears it here). See [Interrupt Support](#interrupt-support) above for how
  the CPU combines these signals from every device on the bus.
- **Lifecycle and direct writes** — `reset()` restores hardware-reset state;
  `patch()` writes a value while bypassing a device's own read-only
  restrictions (used to load ROM images and by the debugger's Memory panel);
  `shutdown()` signals an owned transport to begin closing down.

#### Execution Tracing

The CPU can record every register snapshot and bus read/write to a compact
binary trace format (magic `E65T`) as it executes, via a pluggable
`TraceCallback` — writing is offloaded to a background thread so recording
does not slow down execution. Two tools consume these traces:

- The `emma65` binary writes a trace directly to a file with `--trace-file`
- The debugger's Trace window records and displays a scrolling, live view of
  recent execution without stopping the CPU
- The standalone `emma65-tracer` binary decodes a previously recorded trace
  file into a disassembly listing, optionally annotated with symbols from a
  VICE label file and per-instruction bus operation detail

## I/O Devices

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

### Console (`console`)

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

### 6522 Versatile Interface Adapter (`via/6522`)

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

The VIA uses a GPIO communication protocol over any attached
[`Transport`](#transport-options) to exchange port state and control signal
transitions with real or emulated peripherals. On connection the VIA sends a
full state dump so the peripheral starts with an accurate picture of all
pins and control lines.

### MC6840 Programmable Timer Module (`ptm/6840`)

A comprehensive implementation of the Motorola MC6840 Programmable Timer 
Module (PTM).

- Three independent timers supporting continuous or single-shot 
  generation modes as well as frequency/period or pulse width measurement 
  modes
- Connects to a virtual peripheral over any [Transport](#transport-options)
- Support for external gate and clock inputs and timer output

The PTM uses a communication protocol over any attached
[`Transport`](#transport-options) to exchange port state and control signal
transitions with real or emulated peripherals. On connection, the PTM sends
a full state dump so the peripheral starts with an accurate picture of all
pins and control lines.

### MC6850 Asynchronous Communications Adapter (`acia/6850`)

An comprehensive implementation of the Motorola MC6850 Asynchronous 
Communications Interface Adapter (ACIA):

- Two addressable registers: status/control and RX/TX data
- RDRF and TDRE status with IRQ support for both receive and transmit
- Master reset via control register bits
- TX is immediate: bytes are forwarded to the transport on write; TDRE is
  restored on the next CPU tick 

Connects to a virtual peripheral over any [Transport](#transport-options).

### R6551 Asynchronous Communication Adapter (`acia/6551`)

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

### RGB LED Matrix Display (`display/matrix`)

A memory-mapped RGB LED matrix display supporting 1, 2, 4, or 8 attached
32×32 matrices, fixed at configuration time:

- Pixel memory is mapped directly into the address space, one byte per
  pixel, row-major, per matrix — no per-pixel register bottleneck for bulk
  updates
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

See `doc/memory-mapped-led-matrix-device-spec.md` for the full register-level
specification. Like `display`, `display/matrix` has no in-process
console-style rendering when running the plain `emma65` CLI:

- **The debugger** — the LED Matrix panel renders each matrix as an
  independent, composited canvas in-process, no configuration needed.
- **Standalone `emma65`** — configure a `pipe:` transport pointing at the
  bundled `emma65-led-matrix` SDL2 peripheral binary (see
  [Running the LED Matrix Peripheral](#running-the-led-matrix-peripheral)
  below). A block message streams per matrix swap, and a palette message
  streams per palette write, over a wire protocol specified in
  `doc/led-matrix-external-protocol.md`.

```toml
[[devices]]
type = "display/matrix"
address = 0x9000
matrix-count = 4
register-address = 0x9400
transport = "pipe:/path/to/emma65-led-matrix"
```

### Character Display (`display`)

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

See `doc/memory-mapped-display-device-spec.md` for the full register-level
specification. Unlike the other register-window devices, `display` has
no in-process console-style rendering when running the plain `emma65` CLI:
it needs an external peripheral to actually put pixels on screen. Two ways
to view it:

- **The debugger** — the Display panel renders composited frames in-process,
  no configuration needed.
- **Standalone `emma65`** — configure a `pipe:` transport pointing at the
  bundled `emma65-display` SDL2 peripheral binary (see
  [Running the Display Peripheral](#running-the-display-peripheral) below).
  Composited frame data (char RAM, color RAM, palette, and the font) streams
  to the peripheral once per vsync over a wire protocol specified in
  `doc/char-display-external-protocol.md`.

```toml
[[devices]]
type = "display"
address = 0xF000
columns = 40
rows = 25
transport = "pipe:/path/to/emma65-display"
```

### 16-bit Galois LFSR (`lfsr`)

A memory-mapped pseudo-random number generator based on a 16-bit Galois
linear-feedback shift register (default tap mask `0xB400`, a maximal-length
65535-state sequence):

- 2 addressable registers exposing the current LFSR state
- **Continuous** mode advances the register automatically as part of normal
  execution; **step** mode advances only when explicitly clocked, for
  reproducible pseudo-random sequences under program control

### Bank-Switched Memory Modules

Finch, Phoebe, and Vireo are complete memory subsystems — RAM, ROM, and a
bank-switching MMU — rather than register-window devices. Each claims the
entire 64 KB address space when configured, so no separate `ram`/`rom`
entries are needed alongside them, and their `address` device-spec field is
unused. All three support an optional ROM `write-policy` (`ignore` or
`error`), an `image` loaded at an optional `offset`, and an optional VICE
`labels` file for symbol resolution.

#### Finch bank-switched MMU (`mem/finch`)

512 KB RAM and 512 KB ROM behind a simple MMU: the top four bits of the 6502
address bus (`A12..A15`) index into 16 one-byte bank registers, each
selecting which 4 KB segment of the module's 1024 KB memory space is mapped
into that 4 KB window of the 6502's address space. Two memory-mapped
registers (configurable addresses) control the bank registers and other MMU
functions.

#### Phoebe bank-switched memory (`mem/phoebe`)

56 KB RAM and 32 KB ROM. The ROM is split into four 8 KB banks; bank 3 is
permanently mapped into the upper half of a 16 KB switchable region at
`0xC000` (and must contain the 6502 machine vectors), while a single
memory-mapped control register selects which of banks 0–2 (or none, exposing
the underlying RAM instead) occupies the lower half.

#### Vireo bank-switched memory (`mem/vireo`)

128 KB RAM and 32 KB ROM behind an elegant bank-switching scheme supporting
four configurations — from a plain 32 KB RAM / 32 KB ROM split up to modes
that expose additional RAM banks beyond the 64 KB address space — selected
via a single memory-mapped control register.

### Transport Options

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
of the device's signals.

## Running the Emulator

### Default configuration

When launched with no devices configured, the emulator runs with a built-in
[TaliForth 2](https://github.com/SamCoVT/TaliForth2) ROM and a full set of
peripherals:

- 32 KB RAM at `0x0000`–`0x7FFF`
- TaliForth ROM at `0x8000`–`0xFFFF`
- VIA at `0xFF80` on a Unix-domain socket (`~/.emma/sock/via6522`)
- MC6840 PTM at `0xFF90` on a Unix-domain socket (`~/.emma/sock/mc6840`)
- R6551 ACIA at `0xFFF0` on a pseudo-terminal (`~/.emma/dev/ttyS0`)
- MC6850 ACIA at `0xFFF4` on a pseudo-terminal (`~/.emma/dev/ttyS1`)
- LFSR at `0xFFF6` in step mode
- Console device at `0xFFF8`–`0xFFF9`, connected to the process's own
  standard input and output
- WDC 65C02 variant at 1.8432 MHz

Interact with the Forth interpreter via standard input and output.

### TOML configuration file

Use `--config <file>` to load a TOML configuration file. Top-level keys map
directly to emulator fields — there is no `[emulator]` wrapper:

```toml
cpu-variant = "WDC65C02"   # or "65C02" (CMOS only, default)
clock-speed-hz = 1843200   # omit for unlimited throughput

[[devices]]
type = "ram"
address = 0x0000
size = 32768               # or "32K"

[[devices]]
type = "rom"
address = 0x8000
size = 32768
image = "~/roms/my.bin"    # .bin, .rom, .hex, .ihx, .ihex, .s19, .srec

[[devices]]
type = "console"
address = 0xFFF8
transport = { pty = { path = "~/.emma/dev/ttyS0" } }
```

### CLI flags

All config values can also be set from the command line. CLI takes precedence
over TOML, which takes precedence over environment variables.

```
emma65 --cpu-variant WDC65C02 \
       --clock-speed-hz 1843200 \
       --device ram@0x0000,size=32768,fill=0 \
       --device rom@0x8000,size=32768,image=~/roms/my.bin \
       --device console@0xFFF8,transport=pty:~/.emma/dev/ttyS0
```

Device shorthand format: `type@address[,key=value,...]`

- Address: decimal, `0x` hex, `0o` octal, or `0b` binary
- Size: bytes, or `K`/`k` suffix for kibibytes (e.g. `32K`)
- Paths support `~/` tilde expansion

### Environment variables

Any config key can be set with the `EMMA65_` prefix, using `_` in place of
`-`:

```
EMMA65_CPU_VARIANT=WDC65C02
EMMA65_CLOCK_SPEED_HZ=1843200
```

### Built-in device types

| Type            | Registers | Key attributes                                                                     |
|-----------------|:---------:|-------------------------------------------------------------------------------------|
| `ram`           |     —     | `size` (required), `fill` (optional byte), `image` (optional path)                  |
| `rom`           |     —     | `size` (required), `image` (required path), `fill` (optional byte)                  |
| `console`       |     2     | `transport` (optional), `break` (optional byte: break-key code)                     |
| `acia/6551`     |     4     | `transport` (optional), `with-tdre-bug` (bool), `with-overrun` (bool)               |
| `acia/6850`     |     2     | `transport` (optional)                                                              |
| `via/6522`      |    16     | `transport` (optional), `protocol` (`ascii` or `binary`, optional)                  |
| `ptm/6840`      |     8     | `transport` (optional), `protocol` (`ascii` or `binary`, optional)                  |
| `display/matrix`| variable  | `matrix-count` (required: 1, 2, 4, or 8), `register-address` (required), `frame_rate_hz`, `transport` (optional, `pipe:` only) |
| `display`  |  variable | `columns`, `rows` (optional, default 40×25), `palette`, `font` (optional paths), `double-buffered` (bool), `frame-rate-hz`, `transport` (optional, `pipe:` only) |
| `lfsr`          |     2     | `taps` (optional u16), `mode` (`continuous` or `step`, optional)                    |
| `mem/finch`     |     2     | `bank-registers`, `control-register` (required addresses), `image` (required path), `write-policy`, `fill`, `offset`, `labels` (all optional) |
| `mem/phoebe`    |     1     | `control-register` (required address), `image` (required path), `write-policy`, `fill`, `ram-fill`, `offset`, `labels` (all optional) |
| `mem/vireo`     |     1     | `control-register` (required address), `image` (required path), `write-policy`, `fill`, `ram-fill`, `offset`, `labels` (all optional) |

`mem/finch`, `mem/phoebe`, and `mem/vireo` each occupy the entire 64 KB
address space rather than a fixed-size register window; their register count
above is the count of dedicated MMU/bank-control registers, placed at the
configurable addresses shown, not a contiguous block.

`display`'s register window is `2 * columns * rows + 2` bytes (char RAM
+ color RAM + a control register + a status/data register), so it grows with
the configured grid size rather than being fixed.

`display/matrix`'s pixel memory is `matrix-count * 1024` bytes, based at
`address`; its command and data registers are a separate 2-byte range based
at `register-address` rather than immediately following pixel memory, so the
two can be placed independently on the bus (e.g. keeping pixel memory
aligned to a 1 KiB/N KiB boundary).

Transport shorthand values for CLI and TOML string form:
`pipe:/path/to/exe,arg1,arg2`, `tcp:PORT`, `tcp:IP:PORT`, `unix:PATH`, `pty`,
`pty:SYMLINK_PATH`

## Running the Tracer

`emma65-tracer` decodes a binary trace file — recorded via `emma65
--trace-file <path>` or the debugger's Trace window — into a human-readable,
disassembled instruction listing.

```
emma65-tracer [--output <path>] [--symbol-file <path>]... [--verbose] [<input>]
```

- `<input>` — path to the trace file; reads from stdin if omitted
- `--output <path>` — path to write decoded output; writes to stdout if omitted
- `--symbol-file <path>` — a VICE-format label file to resolve addresses to
  symbol names; may be repeated to load labels from multiple files
- `--verbose` — additionally print the bus reads and writes performed by each
  instruction

## Running the Display Peripheral

`emma65-display` is an SDL2 window that renders a `display` device's
composited output when running the plain `emma65` CLI standalone (the
debugger doesn't need it — its own Display panel renders in-process). It's
not run directly against a live emulator process; instead, the emulator
spawns it as a child and streams frame data to it over the pipe transport's
stdin, per the wire protocol in `doc/char-display-external-protocol.md`.

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

## Running the LED Matrix Peripheral

`emma65-led-matrix` is an SDL2 window that renders a `display/matrix`
device's per-matrix composited output when running the plain `emma65` CLI
standalone (the debugger doesn't need it — its own LED Matrix panel renders
in-process). Like `emma65-display`, it's spawned by the emulator as a child
process and streams data to it over the pipe transport's stdin, per the wire
protocol in `doc/led-matrix-external-protocol.md`.

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
matrix-count = 4
register-address = 0x9400
transport = "pipe:/path/to/target/release/emma65-led-matrix"
```

```
emma65 --device display/matrix@0x9000,matrix-count=4,register-address=0x9400,transport=pipe:/path/to/target/release/emma65-led-matrix
```

The window opens as soon as the emulator attaches the transport, showing
`matrix-count` matrices side by side as round LEDs on a PCB-colored
background, flush against each other. `--arrangement COLSxROWS` lays the
matrices out in a grid instead of a single row (must be a divisor pair of
`matrix-count`, e.g. `2x2` for 4 matrices); `--pitch` sets the initial
on-screen LED center-to-center spacing in pixels (default `12`). The window
remains resizable afterward and letterboxes/scales to fit. Closing the
window ends `emma65-led-matrix`; it also exits cleanly if the emulator
process exits or is killed first, since that closes its stdin.

## For Contributors

Emma65 is written in Rust (2024 edition), as a Cargo workspace. Key
dependencies of the root `emma65` crate:

| Crate               | Purpose                                                             |
|---------------------|---------------------------------------------------------------------|
| `bitflags`          | Processor status register flag sets                                 |
| `thiserror`         | Structured, typed error enums                                       |
| `rand`              | Random fill for uninitialized RAM                                   |
| `tokio`             | Async runtime backing TCP, Unix socket, and PTY transport tasks     |
| `crossbeam-channel` | Sync/async bridge between device `tick()` calls and transport tasks |
| `libc` / `nix`      | PTY and pipe setup on Unix                                          |
| `serde`             | Serialization framework for configuration structs                   |
| `clap`              | CLI argument parsing                                                |
| `figment`           | Multi-source configuration merging (TOML, env vars, CLI)            |
| `tempfile`          | Temporary file for the embedded default ROM at startup              |

The other three workspace members are thin binary crates. `debugger/src-tauri`
(crate `emma65-debugger`) adds Tauri 2, `tauri-plugin-dialog`/
`tauri-plugin-log`, and (on Linux) `gtk` on the Rust side, plus a
React/TypeScript/Vite frontend in `debugger/frontend` — see
[The Debugger](#the-debugger). `display` (crate `emma65-display`) and
`led-matrix` (crate `emma65-led-matrix`) each add the `sdl2` crate (requires
SDL2 development headers to build — see
[Running the Display Peripheral](#running-the-display-peripheral) and
[Running the LED Matrix Peripheral](#running-the-led-matrix-peripheral)) and
reuse the root crate's compositing code directly rather than duplicating it.

The root crate exposes a library (`emma65`) and two binaries, `emma65` and
`emma65-tracer`. The library has two top-level public modules:

- **`emulator`** — the CPU, memory bus, and device infrastructure. Submodules:
  `cpu` (opcode decode table, addressing modes, status register, variant
  selection, bus-access trace recording), `bus` (address regions, bus
  operations, IRQ/NMI controller, device ID allocation, VICE symbol loading),
  `device` (device trait, built-in devices, and the `protocol` submodule for
  VIA/PTM peer-communication framing), `exec` (clock speed, step results, live
  snapshots, free-running and single-step execution), `transport`
  (byte-stream abstraction, implementations, and the relay/reporter types
  that connect devices to them), `disasm` (instruction disassembler, and a
  `trace` submodule that reconstructs disassembly from a recorded trace), and
  `error` (typed errors for every failure category).

- **`watch`** — a self-contained watchpoint expression pipeline: `Scanner` →
  `Vec<Token>` → `Parser` → `Expr` AST → `Compiler` → `Vec<OpCode>` →
  `Evaluator` →
  `Operand`. The scanner and parser use zero-copy techniques — token text
  slices borrow directly from the source string — so the pipeline produces no
  heap allocations until bytecode emission. `WatchCompiler` and
  `WatchEvaluator` are the primary entry points;
  `WatchEvaluator` owns variable name-to-index mappings and persistent
  variable storage so that watchpoint variables survive across steps.

The `emma65` binary (`src/bin/emulator/`) uses the `emulator::config` module
to load configuration from all sources (TOML, environment, CLI), build an
`EmulatorSession`, and run the free loop. The `emulator::config` module is
the integration point for contributors adding new device types. The
`emma65-tracer` binary (`src/bin/tracer/`) and the `emma65-debugger` crate
(`debugger/src-tauri/`) are both thin front ends over the same library.

### Adding a Custom Device Module

A custom device is two pieces: an `IoDevice` implementation (the device
itself) and a `DeviceModule` implementation (the glue that lets it be
configured from TOML/CLI). Device modules are registered with
`DeviceRegistry` before `Config::build()` is called; once registered, a
module's `name()` can appear as the `type` field in a TOML `[[devices]]`
entry or in a CLI `--device` shorthand.

**Step 1** — Implement `IoDevice`. `read`/`write`/`peek` are always passed
the *absolute* bus address, not an offset into the device's own registers —
store the device's own base address (typically via a `with_address()`
builder, matching every built-in device) and compute `address - self.address`
yourself if you have more than one register. `peek()` must be side-effect-free
— it backs the debugger's Memory panel, watchpoints, and the disassembler, so
it must never change device state or interrupt lines the way a real access
might. Beyond those three required methods, override whichever optional ones
your device actually needs — `tick()` for cycle-accurate timing,
`irq_active()`/`take_nmi()` for interrupts, `reset()`/`shutdown()` for
lifecycle, `claims()` for conditional chip-select — all described in
[Memory-Mapped I/O Devices](#memory-mapped-io-devices) above. Note that both
interrupt hooks are polled by the CPU, not called by the device itself:
`irq_active()` is a live query of your own IRQ state, and `take_nmi()` is
called once per step to report and clear a pending edge that your own code
raised internally (e.g. via a `signal_nmi()`-style helper of your own that
sets a pending flag on the triggering write) — not something a device calls
on itself. Everything else defaults to a no-op.

```rust
use emma65::emulator::IoDevice;

struct BlinkerDevice {
    address: u16,
    on: bool,
}

impl BlinkerDevice {
    fn new() -> Self {
        Self { address: 0, on: false }
    }

    fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }
}

impl IoDevice for BlinkerDevice {
    fn read(&mut self, _address: u16) -> u8 {
        self.on as u8
    }

    fn write(&mut self, _address: u16, value: u8) {
        self.on = value != 0;
    }

    fn peek(&self, _address: u16) -> u8 {
        self.on as u8
    }

    fn name(&self) -> &str {
        "myvendor/blinker"
    }
}
```

**Step 2** — Implement `DeviceModule`. The trait requires `name()` and an
async
`instantiate()` that receives a `BusConfig` builder, the mapped address, a
`HashMap<String, figment::value::Value>` of configuration attributes, an
`InstantiationContext` (holds the configured clock speed, an error-event
sender, and — for the console only — a pre-built transport slot), and a
shared `DeviceIdAllocator` for obtaining a `DeviceId` that won't collide with
any other configured device. The implementing struct must also be
`Clone + Send + Sync + 'static`.

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use emma65::emulator::{AddressRange, BusConfig};
use emma65::emulator::bus::DeviceIdAllocator;
use emma65::emulator::config::{DeviceModule, DeviceModuleError, InstantiationContext};

#[derive(Clone)]
struct BlinkerModule;

impl DeviceModule for BlinkerModule {
    fn name(&self) -> &'static str { "myvendor/blinker" }

    async fn instantiate(
        &self,
        bus_config: BusConfig,
        address: u16,
        _attributes: &HashMap<String, figment::value::Value>,
        _context: &InstantiationContext,
        id_allocator: Arc<Mutex<DeviceIdAllocator>>,
    ) -> Result<BusConfig, DeviceModuleError> {
        let device_id = id_allocator.lock().unwrap().next(false);
        bus_config
            .device(
                AddressRange::new(address, address + 1),
                device_id,
                Box::new(BlinkerDevice::new().with_address(address)),
            )
            .map_err(DeviceModuleError::BusConfig)
    }
}
```

**Step 3** — Deserialize attributes from the `HashMap`. Follow the pattern
used by
`RamModule` and `RomModule` in `src/emulator/config/memory.rs`: define a serde
`Deserialize` struct, then extract it with `figment::Figment`:

```rust
use figment::providers::Serialized;
use figment::value::{Dict, Value};

#[derive(serde::Deserialize)]
struct BlinkerAttributes {
    color: String
}

let attrs = Dict::from_iter(attributes.clone());
let config: BlinkerAttributes = figment::Figment::new()
.merge(Serialized::defaults(attrs))
.extract()
.map_err( | e| DeviceModuleError::Config(e.to_string())) ?;
```

**Step 4** — Register the module and build:

```rust
let mut registry = emma65::emulator::DeviceRegistry::with_builtins();
registry.register(BlinkerModule);
let session = config.build( & registry).await?;
```

Once registered, the module is available by name in TOML and CLI
configuration:

```toml
[[devices]]
type = "myvendor/blinker"
address = 0xD000
color = "red"
```

#### A Device That Uses a Transport

`BlinkerDevice` never talks to anything outside the emulator, so its module
never touches [Transport Options](#transport-options). A device that does —
follow the pattern used by every built-in transport-attached device (see
`src/emulator/config/mc6850.rs` for the simplest real example): deserialize
an optional `transport` attribute, convert it to a `TransportSpec`, and hand
it to `TransportSpec::to_transport_with_reporter()` to get back a connected
`Transport` and its paired `TransportRelay`. `EchoDevice` below sends
whatever's written to it out over its transport, and buffers whatever the
transport delivers for the next read — draining the relay from `tick()`,
never from `read()`, so an idle transport never blocks CPU execution (see
[Transport Options](#transport-options) for why that's safe to rely on):

```rust
use std::collections::VecDeque;
use emma65::emulator::{IoDevice, Transport, TransportRelay};

struct EchoDevice {
    address: u16,
    transport: Option<Box<dyn Transport>>,
    relay: Option<TransportRelay>,
    rx_buffer: VecDeque<u8>,
}

impl EchoDevice {
    fn new() -> Self {
        Self { address: 0, transport: None, relay: None, rx_buffer: VecDeque::new() }
    }

    fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }

    fn attach_transport(&mut self, transport: Box<dyn Transport>, relay: TransportRelay) {
        self.transport = Some(transport);
        self.relay = Some(relay);
    }
}

impl IoDevice for EchoDevice {
    fn read(&mut self, _address: u16) -> u8 {
        self.rx_buffer.pop_front().unwrap_or(0)
    }

    fn write(&mut self, _address: u16, value: u8) {
        if let Some(transport) = self.transport.as_mut() {
            transport.send(value);
        }
    }

    fn peek(&self, _address: u16) -> u8 {
        self.rx_buffer.front().copied().unwrap_or(0)
    }

    fn tick(&mut self, _cycles: u32) {
        if let Some(relay) = self.relay.as_mut() {
            let rx_buffer = &mut self.rx_buffer;
            relay.drain_bytes_into(|b| rx_buffer.push_back(b));
        }
    }

    fn name(&self) -> &str {
        "myvendor/echo"
    }
}
```

```rust
use emma65::emulator::config::{TransportSpec, TransportSpecFormat};

#[derive(Clone)]
struct EchoModule;

#[derive(serde::Deserialize)]
struct EchoAttributes {
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for EchoModule {
    fn name(&self) -> &'static str { "myvendor/echo" }

    async fn instantiate(
        &self,
        bus_config: BusConfig,
        address: u16,
        attributes: &HashMap<String, figment::value::Value>,
        context: &InstantiationContext,
        id_allocator: Arc<Mutex<DeviceIdAllocator>>,
    ) -> Result<BusConfig, DeviceModuleError> {
        let attrs = figment::value::Dict::from_iter(attributes.clone());
        let config: EchoAttributes = figment::Figment::new()
            .merge(figment::providers::Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(e.to_string()))?;

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;

        let device_id = id_allocator.lock().unwrap().next(false);
        let mut device = EchoDevice::new().with_address(address);
        if let Some(transport_spec) = transport_spec {
            let (transport, relay) = transport_spec
                .to_transport_with_reporter(context.pipe_exit_reporter(device_id))
                .await
                .map_err(DeviceModuleError::Transport)?;
            device.attach_transport(transport, relay);
        }

        bus_config
            .device(AddressRange::new(address, address + 1), device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }
}
```

```toml
[[devices]]
type = "myvendor/echo"
address = 0xD100
transport = { pty = { path = "~/.emma/dev/ttyEcho" } }
```

```
cargo build                # build the emma65 and emma65-tracer binaries
cargo build --workspace    # also build the emma65-debugger, emma65-display, and emma65-led-matrix crates
cargo build -p emma65-display     # build just the SDL2 display peripheral (needs libsdl2-dev)
cargo build -p emma65-led-matrix  # build just the SDL2 LED matrix peripheral (needs libsdl2-dev)
cargo test                 # run all tests (includes Klaus Dormann and Bruce Clark suites)
cargo test --workspace     # also run the debugger, display, and led-matrix crates' tests
cargo clippy               # lint the whole workspace
```
