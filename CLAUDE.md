# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build              # build the emma65 and emma65-tracer binaries + library
cargo build --workspace  # also build the debugger crate (emma65-debugger)
cargo test                # run the library/binary test suite
cargo test --workspace    # also run the debugger crate's tests
cargo test <name>         # run a single test by name (partial match)
cargo clippy              # lint (covers the debugger crate too — it's a workspace member)
```

The debugger's frontend (`debugger/frontend/`) is a separate React/TypeScript/Vite project.
Tauri invokes `npm run build` there automatically as part of `cargo tauri build` /
`cargo tauri dev`; it is not built by plain `cargo build`.

## Architecture

`emma65` is a Cargo workspace (Rust 2024 edition) with two members:

- **`.`** (crate `emma65`) — the emulator library plus two binaries: `emma65` (the emulator)
  and `emma65-tracer` (decodes binary trace files)
- **`debugger/src-tauri`** (crate `emma65-debugger`) — a Tauri 2 desktop app that hosts the
  emulator and exposes a full-featured debugger UI

The `emma65` library exposes two top-level public modules:

- **`emulator`** — CPU, memory bus, devices, transport, execution/trace model, and config
- **`watch`** — watchpoint expression pipeline (scanner → parser → compiler → evaluator)

### Crate structure

```
src/
  lib.rs                  — exposes pub mod emulator, pub mod watch
  bin/
    emulator/
      main.rs              — emma65 binary entry point
      config.rs             — AppConfig, CliArgs, apply_default_if_unconfigured
      tty.rs                — terminal/PTY helpers for the CLI console
    tracer/
      main.rs               — emma65-tracer binary entry point (CLI args, trace decode loop)
      format.rs             — text rendering of decoded trace rows
  emulator/
    mod.rs                  — re-exports public API surface
    bus/                    — Bus, BusConfig, address regions, DeviceIdAllocator, symbol
                               table (VICE label loading), bus loader, Interrupt controller, IrqSource
    cpu/                    — Cpu, Registers, opcode decode, ALU, status register, variant,
                               bus-access trace recording (binary trace format)
    device/                 — IoDevice trait, built-in devices, device/protocol (VIA and PTM
                               peer-communication message encoders/decoders)
    disasm/                 — Disassembler, and disasm/trace (reconstructs disassembly lines
                               from a recorded trace stream)
    error.rs                — BusConfigError, BusError, CpuBuildError, ExecError
    exec/                   — ClockSpeed, StepResult, CpuLiveSnapshot, RunHandle/RunStopper,
                               run(), run_from(), step_into(), step_over_subroutine(),
                               step_over_breakpoint(), step_return()
    transport/               — Transport trait, ChannelRelay, TransportRelay, TransportReporter,
                               InternalPipeTransport, PipeTransport, TcpSocketTransport,
                               UnixSocketTransport, PtyTransport
    session.rs               — EmulatorSession (owns Cpu, ErrorReceiver, DeviceIdAllocator)
    config/                  — configuration loading and device module registry (see below)
  watch/                    — watchpoint expression pipeline (see below)
debugger/
  src-tauri/                — emma65-debugger: Tauri commands/state per UI panel (see below)
  frontend/                 — React/TypeScript/Vite UI (panels, styles, keybindings)
```

`util/via_sr_peripheral.py` is a standalone Python script that emulates a VIA shift-register
peripheral over a transport, useful for manually exercising `Via6522`'s shift-register modes.

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
InstantiationContext // clock_hz, error_sender, console_transport passed to DeviceModule::instantiate()
RamModule / RomModule
TransportSpec       // Tcp { port, address } | Unix { path } | Pty { path }
TransportSpecFormat // serde-untagged: Shorthand(String) | Structured(TransportSpec)
ExpandedPathBuf     // PathBuf that expands ~/ at construction; used for path attrs
```

Built-in device modules (registered by `DeviceRegistry::with_builtins()`), by config `type` string:
`ram`, `rom`, `console`, `mem/finch`, `display/matrix`, `lfsr`, `acia/6551`, `ptm/6840`,
`acia/6850`, `mem/phoebe`, `via/6522`, `mem/vireo`.

`Config::build(&registry)` iterates `devices`, dispatches each to its `DeviceModule`,
builds the `BusConfig`, constructs `Cpu`, and returns `EmulatorSession`.

`DeviceSpec::from_str` format: `type@address[,key=value,...]`
- Address: decimal, `0x`/`0o`/`0b` prefix
- Size: bytes or `K`/`k` suffix
- Transport: `tcp:PORT`, `tcp:IP:PORT`, `unix:PATH`, `pty`, `pty:SYMLINK`

**`emulator::config::default` module (`src/emulator/config/default/`)** — bundles the default
device layout as a checked-in template (`emulator-template.toml`) plus the TaliForth ROM
(`program.bin`) and its VICE labels (`program.lbl`), embedded via `include_bytes!`/`include_str!`.
`materialize_default_config(dest)` writes all three (rendered, with `image=`/`labels=` paths
filled in) into `dest`, returning the path to the written `emulator.toml`. This is the single
source of truth for the default layout: 32K RAM at `0x0000`, the TaliForth ROM at `0x8000`, a VIA
and a PTM on Unix-socket transports, an ACIA on `~/.emma/dev/ttyS0`, a second ACIA on
`~/.emma/dev/ttyS1`, an LFSR, and a console — WDC65C02 at 1.8432 MHz. The `emma65` binary
materializes it into a tempdir when no devices are configured (`apply_default_if_unconfigured` in
`src/bin/emulator/config.rs`, loaded through the normal `Toml::file()` path); the debugger
materializes it into `~/.emma/debugger/profiles/default/` the first time that profile directory
is created (see `profile::ensure_profile_dir` below).

---

### `emulator::bus`

`BusConfig` is a builder. Regions are added with `.ram()`, `.rom()`, `.device()`, then
`.build()` produces a `Bus`. Most-specific-wins overlap: smaller regions shadow larger
ones at the same addresses. Ambiguous same-size overlaps are caught at build time.

`DeviceIdAllocator` hands out `DeviceId`s that don't collide with configured devices;
`EmulatorSession` exposes it post-configuration so hosts (e.g. the debugger UI) can
allocate additional IDs for UI-driven controls (like an IRQ toggle) safely.

`symbol::load_vice_labels` loads a VICE-format label file into a `SymbolTable`, used by
the disassembler, trace tooling, and the debugger to resolve addresses to names.

---

### `emulator::cpu`

`Cpu::builder(variant)` → `CpuBuilder` → `Cpu`. Two variants: `Cmos65C02` and
`Wdc65C02`. The builder accepts a `ClockSpeed` and a `Bus`.

`Cpu::step()` returns `StepResult`. `exec::run()` and friends (below) drive execution.

`cpu::trace` implements bus-access tracing: `TraceCallback`/`ChannelTraceCallback` receive
`TraceRecord`s (`TraceKind::Registers | Read | Write | Cycles`) as the CPU executes;
`BinaryTraceWriter`/`BinaryTraceReader` persist them to/from a versioned binary format
(magic `E65T`, currently format version 2, header carries the CPU variant); `spawn_trace_writer`
runs the writer on a background thread fed by a channel, with a configurable `OverflowPolicy`.
This is the backbone for both the `emma65-tracer` binary and the debugger's live trace window.

---

### `emulator::device`

All built-in devices implement `IoDevice`:

```rust
fn read(&mut self, offset: u16) -> u8;
fn write(&mut self, offset: u16, value: u8);
fn peek(&self, offset: u16) -> u8;   // side-effect-free (watchpoints, disassembler)
// optional: tick(), irq_active(), take_nmi(), take_reset(), name()
```

Built-in devices: `Console`, `R6551`, `Mc6850`, `Via6522`, `Mc6840`, `Finch`, `Phoebe`, `Vireo`,
`LedMatrix`, `Lfsr16`. Devices that need byte-stream I/O hold a `TransportRelay` connected to
an `Option<Box<dyn Transport>>`; the VIA and MC6840 additionally support a structured
peer-communication protocol (ASCII or binary framing) implemented in `device::protocol`
(`device::protocol::via`, `device::protocol::ptm`) for exchanging port/pin state with real or
emulated peripherals over that transport.

---

### `emulator::exec`

`run()`/`run_from()` drive a free-running loop (optionally clock-throttled) on a background
thread, returning a `RunHandle`/`RunStopper` pair for stopping it and reading a live
`CpuLiveSnapshot` (registers, stack page, cycle counts, IRQ/NMI/STP/WAI status, a memory page)
without pausing the CPU. `step_into()`, `step_over_subroutine()`, `step_over_breakpoint()`, and
`step_return()` implement the debugger's single-step, step-over, and step-out commands.

---

### `emulator::transport`

`Transport` is the byte-stream abstraction; implementations: `PipeTransport`,
`InternalPipeTransport`, `TcpSocketTransport`, `UnixSocketTransport`, `PtyTransport`. TCP and
Unix socket transports listen for incoming connections; PTY creates a pseudoterminal.
`TransportRelay`/`ChannelRelay` decouple a device's synchronous `tick()` from the transport's
async I/O task; `TransportReporter` surfaces connect/disconnect/error events as `DeviceEvent`s
(consumed via `device_event_channel()`).

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
- **`location`** — source position tracking used by tokens and diagnostics
- **`scanner`** — tokenizes source; handles `0x`/`$`/`0o`/`0q`/`0b`/decimal literals
- **`token`** — `Token<'a>` with `TokenType` and an `&'a str` text slice
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

---

### `debugger` crate (`debugger/src-tauri/`)

A Tauri 2 desktop app (`emma65-debugger`) that loads config from
`~/.emma/debugger/profiles/default/emulator.toml`, builds an `EmulatorSession` with an
injected `InternalPipeTransport` wired to its terminal window, and exposes the emulator to a
React/TypeScript frontend (`debugger/frontend/`) via `#[tauri::command]`s. UI preferences
(theme, exit-confirmation skip) are not profile-scoped and live in
`~/.emma/debugger/config/ui.toml` instead. One module per UI panel:

- **`registers`** — register snapshot/edit
- **`cpu_bus`** — reset, IRQ assert/release, NMI trigger, cached bus-signal snapshot
- **`disassembly`** — run/stop/step-into/step-over/step-return, breakpoint CRUD, disassembly listing
- **`memory`** — paged reads/writes/fills/file loads
- **`stack`** — stack pointer and stack page snapshot
- **`terminal`** — console byte-stream bridge and window visibility (toggleable window)
- **`trace`** — live-recorded execution trace, windowed reads
- **`watchpoints`** — loads/compiles `watchpoints.emw`, evaluates on demand, add/remove/edit/toggle with persistence
- **`theme`** — light/dark theme preference; also owns `UiConfig`/`ui.toml` persistence used by
  the exit-confirmation "Don't ask again" preference (set from `lib.rs`'s `confirm_exit`)
- **`menu`** — native File/Edit/Window/Help menu bar and Window-menu checkbox sync
- **`recent`** — recently-used profile list (`~/.emma/debugger/config/recent.toml`), recorded on every
  profile activation and shown in the File > Open Recent submenu
- **`profile`** — `--profile` CLI flag, profile directory resolution, `ensure_profile_dir` (seeds a
  new `default` profile from the bundled `emulator::config::default` template; seeds any other new
  profile by copying files from `default`), New/Open Profile commands, window-title sync

Devices requiring a byte-stream peer (VIA, MC6840, ACIAs) still use their configured
`Transport` independent of the debugger UI; only the console is special-cased to route
through the debugger's own terminal window.
