# The Emulator Core

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

## Correctness

Emma65 passes
the [Klaus Dormann 65C02 test suite](https://github.com/Klaus2m5/6502_65C02_functional_tests),
which exhaustively exercises every instruction, addressing mode, flag
computation, interrupt sequence, and decimal-mode operation defined by the
65C02 architecture. It also passes
the [Bruce Clark decimal mode test](http://www.6502.org/tutorials/decimal_mode.html),
which independently verifies all 256×256 ADC and SBC operand combinations in
BCD mode against predicted CMOS 65C02 results. Users can rely on Emma65's
instruction-level behavior matching real hardware.

## Features

### Instruction Set

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

### Interrupt Support

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

### Clock Speed Simulation

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

### Memory and Bus Configuration

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

### Memory-Mapped I/O Devices

Devices are mapped onto the bus with the same builder call used for RAM and
ROM regions — `BusConfig::device(AddressRange, DeviceId, Box<dyn IoDevice>)`
— so a device window is subject to the same build-time overlap checking. The
built-in `ram`, `rom`, `console`, and other `type`s configurable from
TOML/CLI (see [Running the Emulator](running-the-emulator.md)) are themselves
just `DeviceModule` implementations that make this same call: each is
registered by name in a `DeviceRegistry`, and `Config::build()` instantiates
one per `[[devices]]` entry (or `--device` flag) as it walks the configured
device list at startup. A custom device plugs into this exact same
configuration surface — see
[Adding a Custom Device Module](for-contributors.md#adding-a-custom-device-module) under For
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

### Execution Tracing

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
