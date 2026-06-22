# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build          # build
cargo test           # run all tests
cargo test <name>    # run a single test by name (partial match)
cargo clippy         # lint
```

## Architecture

`emma65` is a Rust crate (2024 edition) with a library and a binary. The library exposes
two top-level public modules:

- **`emulator`** — CPU, memory bus, devices, transport, config, and execution model
- **`watch`** — watchpoint expression pipeline (scanner → parser → compiler → evaluator)

The binary is in `src/bin/emulator/`. It uses `emulator::config` to load configuration
from TOML, environment variables, and CLI arguments, then builds and runs an
`EmulatorSession`.

### Crate structure

```
src/
  lib.rs                  — exposes pub mod emulator, pub mod watch
  bin/emulator/
    main.rs               — binary entry point; embeds default.bin ROM
    config.rs             — AppConfig, CliArgs, apply_default_if_unconfigured
  emulator/
    mod.rs                — re-exports public API surface
    bus/                  — Bus, BusConfig, address regions, bus tracing
    cpu/                  — Cpu, Registers, opcode decode, status register, variant
    device/               — IoDevice trait, built-in devices (Console, Acia6551, Mc6850, Via6522)
    disasm/               — Disassembler
    error.rs              — BusConfigError, BusError, CpuBuildError, ExecError
    exec/                 — ClockSpeed, StepResult, RunHandle, run()
    interrupt.rs          — InterruptController, IrqSource
    transport/            — Transport trait, PipeTransport, TcpTransport, UnixSocketTransport, PtyTransport
    session.rs            — EmulatorSession (owns Cpu + ErrorReceiver)
    config/               — configuration loading and device module registry (see below)
  watch/                  — watchpoint expression pipeline (see below)
```

---

### `emulator::config` module (`src/emulator/config/`)

Multi-source configuration (TOML < `EMMA65_*` env vars < CLI args) via `figment` + `clap`.

Key types re-exported from `emulator`:

```rust
Config              // emulator config: cpu_variant_spec, clock_speed_hz, devices
BuildError          // errors from Config::build()
CpuVariantSpec      // "65C02" | "WDC65C02"
DeviceSpec          // parsed device entry: type@address,key=val,...
DeviceModule        // trait for pluggable device modules
DeviceModuleError   // BusConfig | Transport | Config | Load | Io
DeviceRegistry      // maps module names to InstantiateFn closures
InstantiationContext // clock_hz, error_sender passed to DeviceModule::instantiate()
RamModule / RomModule
TransportSpec       // Tcp { port, address } | Unix { path } | Pty { path }
TransportSpecFormat // serde-untagged: Shorthand(String) | Structured(TransportSpec)
ExpandedPathBuf     // PathBuf that expands ~/ at construction; used for path attrs
```

Built-in device modules (registered by `DeviceRegistry::with_builtins()`):
`ram`, `rom`, `console`, `acia/6551`, `acia/6850`, `via/6522`

`Config::build(&registry)` iterates `devices`, dispatches each to its `DeviceModule`,
builds the `BusConfig`, constructs `Cpu`, and returns `EmulatorSession`.

`DeviceSpec::from_str` format: `type@address[,key=value,...]`
- Address: decimal, `0x`/`0o`/`0b` prefix
- Size: bytes or `K`/`k` suffix
- Transport: `tcp:PORT`, `tcp:IP:PORT`, `unix:PATH`, `pty`, `pty:SYMLINK`

The binary applies a built-in default (TaliForth ROM + RAM + console PTY at
`~/.emma/dev/ttyS0`, WDC65C02, 1.8432 MHz) when no devices are configured.

---

### `emulator::bus`

`BusConfig` is a builder. Regions are added with `.ram()`, `.rom()`, `.device()`, then
`.build()` produces a `Bus`. Most-specific-wins overlap: smaller regions shadow larger
ones at the same addresses. Ambiguous same-size overlaps are caught at build time.

---

### `emulator::cpu`

`Cpu::builder(variant)` → `CpuBuilder` → `Cpu`. Two variants: `Cmos65C02` and
`Wdc65C02`. The builder accepts a `ClockSpeed` and a `Bus`.

`Cpu::step()` returns `StepResult`. `exec::run()` drives a free-running loop with
optional clock throttling.

---

### `emulator::device`

All built-in devices implement `IoDevice`:

```rust
fn read(&mut self, offset: u16) -> u8;
fn write(&mut self, offset: u16, value: u8);
fn peek(&self, offset: u16) -> u8;   // side-effect-free (watchpoints, disassembler)
// optional: tick(), irq_active(), take_nmi(), name()
```

Devices that need byte-stream I/O hold an `Option<Box<dyn Transport>>`. TCP and Unix
socket transports listen for incoming connections; PTY creates a pseudoterminal.

---

### `watch` module (`src/watch/`)

A self-contained pipeline for evaluating watchpoint expressions against live machine state.

```
source &str → Scanner → Vec<Token> → Parser → Expr tree → Compiler → Vec<OpCode> → Evaluator → Operand (u32)
```

Public API (re-exported from `emma65::watch`):

```rust
pub use self::context::WatchContext;
pub use self::error::{Error, WatchError};
pub use self::expr::Operand;
pub use self::parser::Mapper;
pub use self::session::{WatchCompiler, WatchEvaluator, Watchpoint};
```

`WatchCompiler::new(map_register, map_flag, map_symbol)` — owns a `Parser`.
`compiler.compile(source, evaluator)` → `Watchpoint` (stores `Vec<OpCode>`).
`WatchEvaluator::new()` — owns watchpoints, `Variables`, and variable runtime storage.
`evaluator.evaluate_all(context)` → `Ok(Some(index))` | `Ok(None)` | `Err((index, err))`.

#### Submodules

- **`text`** — zero-copy cursor over `&str`; `consume()` returns `[start..current]`
- **`scanner`** — tokenizes source; handles `0x`/`$`/`0o`/`0q`/`0b`/decimal literals
- **`token`** — `Token<'a>` with `TokenType`, `&'a str` text slice, and `Location`
- **`expr`** — `Expr<'a>` AST: leaf nodes (Number, Register, Flag, Variable), Assign (walrus), UnaryOperator (includes Fetch), BinaryOperator; `signed: bool` field
- **`variables`** — `Variables` maps names to stable `Operand` IDs via `get_or_create`
- **`parser`** — recursive descent; precedence: `:=` → `||` → `&&` → `|` → `^` → `&` → `==` → relational → shift → `+/-` → `*/` → unary → primary
- **`compiler`** — depth-first `Expr` traversal → flat `Vec<OpCode>`; signedness selects opcode variant
- **`evaluator`** — stack VM over `&[OpCode]` against `&dyn WatchContext` and `&mut [Operand]`
- **`context`** — `WatchContext` trait: `read_register_u32/i32`, `read_flag`, `read_mem_u32/i32`
- **`session`** — high-level `WatchCompiler` + `WatchEvaluator` API

#### Domain-specific operators

- `B[addr]`, `W[addr]`, `D[addr]` — byte/word/dword memory fetch; leading `+`/`-` controls signedness
- `` `flagname `` — reads a named CPU status flag
- `:=` — walrus: assigns RHS to a named variable and yields its value; variables persist across `evaluate_all` calls
- `$hex` — hexadecimal literal shorthand

#### Lifetime threading

`Token<'a>` and `Expr<'a>` borrow from the source `&'a str`. After `compiler::compile`
consumes the tree, the resulting `Vec<OpCode>` (stored in `Watchpoint`) has no lifetime
parameters and can be stored freely.
