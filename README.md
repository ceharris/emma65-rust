# emma65

Emma65 is a software emulator for the 65C02-family of 8-bit microprocessors.
It provides a complete execution environment suitable for running and
debugging programs written for classic 65C02-based systems, with support for
flexible memory configuration, a rich set of virtual I/O devices, and
expression-based watchpoints. It is designed as a foundation for building
retro-computing tools, educational simulators, and hardware-in-the-loop test
rigs.

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

Bus errors (unmapped reads/writes, ROM write violations) are surfaced through
`StepResult::Error` so the host application can decide how to respond.

### Virtual I/O Devices

All four built-in devices implement the `IoDevice` trait and can be mapped
into any address range on the bus. Each integrates with the interrupt
controller via `irq_active()`
and `take_nmi()`, and with the transport system for byte-stream I/O.

#### 6522 VIA (`Via6522`)

A comprehensive implementation of the WDC 65C22 Versatile Interface Adapter:

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

The VIA uses a GPIO communication protocol over any attached `Transport` to
exchange port state and control signal transitions with real or emulated
peripherals. On connection the VIA performs a format-negotiation handshake and
sends a full state dump so the peripheral starts with an accurate picture of
all pins and control lines.

#### Rockwell 6551 ACIA (`Acia6551`)

An implementation of the Rockwell 6551 Asynchronous Communications Interface
Adapter:

- 4 addressable registers: RX data, TX data, status, and command/control
- RDRF (Receive Data Register Full) and TDRE (Transmit Data Register Empty)
  status bits
- Interrupt-driven I/O with separate RX and TX interrupt enables
- Baud rate selection from the control register; external-clock mode polls the
  transport on every CPU tick for maximum responsiveness
- Hardware bug–compatible mode (`Acia6551::with_tdre_bug()`) keeps TDRE
  permanently set, matching the behavior of the WDC 65C51 variant for software
  that uses timed delays rather than TDRE polling

#### MC6850 ACIA (`Mc6850`)

An implementation of the Motorola MC6850 Asynchronous Communications Interface
Adapter:

- 2 addressable registers: status/control and RX/TX data
- RDRF and TDRE status with IRQ support for both receive and transmit
- Master reset via control register bits
- TX is immediate: bytes are forwarded to the transport on write; TDRE is
  restored on the next CPU tick

#### Console (`Console`)

A simple polling console device for byte-stream I/O:

- Input buffering via a 128-byte ring buffer
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

### Transport Options

Devices that exchange byte streams attach a `Transport`. Four implementations
are provided:

| Transport             | Best for                                                              |
|-----------------------|-----------------------------------------------------------------------|
| `PipeTransport`       | In-process tests; inter-process stdin/stdout                          |
| `TcpTransport`        | Connecting a terminal emulator or remote process over the network     |
| `UnixSocketTransport` | Low-latency local IPC (lower overhead than TCP)                       |
| `PtyTransport`        | Any program that expects a real TTY — `screen`, `minicom`, `cu`, etc. |

All transports are non-blocking: `try_recv()` returns `None` immediately when
no data is available, so device `tick()` implementations never stall the CPU
thread.

### Extensibility

Custom devices implement the `IoDevice` trait. Only three methods are
required:

```rust
fn read(&mut self, offset: u16) -> u8;
fn write(&mut self, offset: u16, value: u8);
fn peek(&self, offset: u16) -> u8;  // Side-effect-free read (watchpoints, disassembler)
```

The remaining methods — `tick`, `irq_active`, `take_nmi`, and `name` — have
default no-op implementations and can be added as needed.

### Watchpoint Expressions

Watchpoints are expressions evaluated against live machine state before each
instruction. The expression language supports registers (`A`, `X`, `Y`, `P`,
`S`, `PC`), named CPU status flags (`` `N ``, `` `Z ``, `` `C ``, etc.),
memory reads at byte, word, and doubleword widths (`B[addr]`, `W[addr]`,
`D[addr]`), hex literals (`$FF`), arithmetic and bitwise operators,
comparisons, and a walrus operator (`:=`) for snapshotting values across
steps. Expressions are compiled to bytecode once and evaluated efficiently on
every step, making it practical to run many watchpoints simultaneously.

## Running the Emulator

### Default configuration

When launched with no arguments, the emulator runs with a built-in
[TaliForth 2](https://github.com/SamCoVT/TaliForth2) ROM:

- 32 KB RAM at `0x0000`–`0x7FFF`
- TaliForth ROM at `0x8000`–`0xFFFF`
- Console device at `0xFFF8`–`0xFFF9` with a PTY transport symlinked at
  `~/.emma/dev/ttyS0`
- WDC 65C02 variant at 1.8432 MHz

Connect any TTY program to the PTY symlink to reach the Forth REPL:

```
screen ~/.emma/dev/ttyS0
```

A notice is printed to stderr confirming the default is in use:

```
notice: using default configuration; connect terminal to ~/.emma/dev/ttyS0
```

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

| Type        | Registers | Key attributes                                                        |
|-------------|:---------:|-----------------------------------------------------------------------|
| `ram`       |     —     | `size` (required), `fill` (optional byte), `image` (optional path)    |
| `rom`       |     —     | `size` (required), `image` (required path), `fill` (optional byte)    |
| `console`   |     2     | `transport` (optional), `break` (bool)                                |
| `acia/6551` |     4     | `transport` (optional), `with_tdre_bug` (bool), `with_overrun` (bool) |
| `acia/6850` |     2     | `transport` (optional)                                                |
| `via/6522`  |    16     | `transport` (optional)                                                |

Transport shorthand values for CLI and TOML string form:
`tcp:PORT`, `tcp:IP:PORT`, `unix:PATH`, `pty`, `pty:SYMLINK_PATH`

## For Contributors

Emma65 is written in Rust (2024 edition). Key dependencies:

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

The crate exposes both a library (`emma65`) and a binary (`emma65`). The
library has two top-level public modules:

- **`emulator`** — the CPU, memory bus, and device infrastructure. Submodules:
  `cpu`
  (opcode decode table, addressing modes, status register, variant selection),
  `bus`
  (address regions, bus operations, tracing), `device` (device trait and
  built-in devices), `exec` (clock speed, step results, free-running handle),
  `interrupt` (IRQ/NMI controller), `transport` (byte-stream abstraction and
  implementations), `disasm`
  (instruction disassembler), and `error` (typed errors for every failure
  category).

- **`watch`** — a self-contained watchpoint expression pipeline: `Scanner` →
  `Vec<Token>` → `Parser` → `Expr` AST → `Compiler` → `Vec<OpCode>` →
  `Evaluator` →
  `Operand`. The scanner and parser use zero-copy techniques — token text
  slices borrow directly from the source string — so the pipeline produces no
  heap allocations until bytecode emission. `WatchCompiler` and
  `WatchEvaluator` are the primary entry points;
  `WatchEvaluator` owns variable name-to-index mappings and persistent
  variable storage so that watchpoint variables survive across steps.

The binary (`src/bin/emulator/`) uses the `emulator::config` module to load
configuration from all sources (TOML, environment, CLI), build an
`EmulatorSession`, and run the free loop. The `emulator::config` module is the
integration point for contributors adding new device types.

### Adding a Custom Device Module

Device modules are registered with `DeviceRegistry` before `Config::build()`
is called. Once registered, a module's `name()` can appear as the `type` field
in a TOML
`[[devices]]` entry or in a CLI `--device` shorthand.

**Step 1** — Implement `DeviceModule`. The trait requires `name()` and an
async
`instantiate()` that receives a `BusConfig` builder, the mapped address, a
`HashMap<String, figment::value::Value>` of configuration attributes, and an
`InstantiationContext` (holds the configured clock speed and an error-event
sender). The implementing struct must also be `Clone + Send + Sync + 'static`.

```rust
use std::collections::HashMap;
use emma65::emulator::{AddressRange, BusConfig, DeviceId};
use emma65::emulator::config::{DeviceModule, DeviceModuleError, InstantiationContext};

#[derive(Clone)]
struct LedModule;

impl DeviceModule for LedModule {
    fn name(&self) -> &'static str { "myvendor/led" }

    async fn instantiate(
        &self,
        bus_config: BusConfig,
        address: u16,
        _attributes: &HashMap<String, figment::value::Value>,
        _context: &InstantiationContext,
    ) -> Result<BusConfig, DeviceModuleError> {
        bus_config
            .device(
                AddressRange::new(address, address + 1),
                DeviceId(address as u32),
                Box::new(LedDevice::new()),
            )
            .map_err(DeviceModuleError::BusConfig)
    }
}
```

**Step 2** — Deserialize attributes from the `HashMap`. Follow the pattern
used by
`RamModule` and `RomModule` in `src/emulator/config/memory.rs`: define a serde
`Deserialize` struct, then extract it with `figment::Figment`:

```rust
use figment::providers::Serialized;
use figment::value::{Dict, Value};

#[derive(serde::Deserialize)]
struct LedAttributes {
    color: String
}

let attrs = Dict::from_iter(attributes.clone());
let config: LedAttributes = figment::Figment::new()
.merge(Serialized::defaults(attrs))
.extract()
.map_err( | e| DeviceModuleError::Config(e.to_string())) ?;
```

**Step 3** — Register the module and build:

```rust
let mut registry = emma65::emulator::DeviceRegistry::with_builtins();
registry.register(LedModule);
let session = config.build( & registry).await?;
```

Once registered, the module is available by name in TOML and CLI
configuration:

```toml
[[devices]]
type = "myvendor/led"
address = 0xD000
color = "red"
```

```
cargo build      # build
cargo test       # run all tests (includes Klaus Dormann and Bruce Clark suites)
cargo clippy     # lint
```
