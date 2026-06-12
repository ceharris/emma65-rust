# Emma65

Emma65 is a software emulator for the 65C02-family of 8-bit microprocessors.
It provides a complete execution environment suitable for running and
debugging programs written for classic 65C02-based systems, with support for
flexible memory configuration, virtual I/O devices, and expression-based
watchpoints that make it a capable foundation for building retro-computing
tools, educational simulators, and hardware-in-the-loop test rigs.

## Features

### Instruction Set

Emma65 emulates two variants of the 65C02 processor family:

- **CMOS 65C02** — the standard CMOS variant, including all instructions added
  over the original NMOS 6502: `BRA`, `STZ`, `TSB`, `TRB`, `PHX`, `PHY`,
  `PLX`, `PLY`, accumulator-mode `INC` and `DEC`, zero-page indirect
  addressing, and `JMP (abs,X)`.

- **WDC 65C02** — the Western Design Center variant, which adds 34 opcodes to
  the CMOS baseline:
  `STP` (stop the processor), `WAI` (wait for interrupt), `BBR0`–`BBR7` and
  `BBS0`–`BBS7` (branch on bit clear/set), and `RMB0`–`RMB7` and
  `SMB0`–`SMB7` (reset/set memory bit).

All 16 addressing modes are supported, including the zero-page relative mode
used by the WDC bit-branch instructions. Invalid opcodes can be configured to
either silently act as NOPs (advancing PC by the correct byte length) or to
halt execution with an error.

### Memory and Bus Mapping

The memory bus is organized around named address regions mapped into the
16-bit address space. Regions can be configured as RAM, ROM (with
write-protection), or I/O device windows. The bus resolver supports
overlapping regions with unambiguous priority rules, and reports configuration
errors — including ROM size mismatches and duplicate device registrations — at
build time rather than at runtime. Bus errors (unmapped reads/writes, ROM
write violations) are surfaced through the step result so the host application
can decide how to respond.

### Virtual I/O Devices

I/O devices are registered on the bus by `DeviceId` and mapped to one or more
address ranges. The device interface handles byte-level reads and writes from
the CPU, making it straightforward to implement common 65C02 peripherals such
as UARTs, timers, and parallel I/O ports. Connectivity options allow virtual
devices to be backed by real host resources — such as TCP sockets, files, or
pipes — so that emulated peripherals can exchange data with external processes
and tools.

### Execution Model

Execution is step-based: each call to `Cpu::step()` executes one instruction
and returns a `StepResult` describing what happened. Results include normal
instruction execution (with the decoded opcode), breakpoint hits, watchpoint
triggers, `WAI`/`STP` processor states, and fatal bus or CPU errors. A
free-running mode drives the step loop automatically at a configurable clock
speed (specified in MHz or Hz, or left unlimited for maximum throughput).

### Watchpoint Expressions

Watchpoints are specified as expressions evaluated against live machine state
before each instruction. The expression language supports registers (`A`, `X`,
`Y`, `P`, `S`, `PC`), named CPU status flags (`` `N ``, `` `Z ``, etc.),
memory reads at byte, word, and doubleword widths (`B[addr]`, `W[addr]`,
`D[addr]`), hex literals (`$FF`), arithmetic and bitwise operators,
comparisons, and a walrus operator (`:=`) for snapshotting values across
steps. Expressions are compiled to bytecode once and evaluated efficiently on
every step, making it practical to run many watchpoints simultaneously.

## For Contributors

Emma65 is written in Rust (2024 edition) with minimal dependencies — only
`bitflags` for the processor status register and `thiserror` for structured
error types. There are no runtime allocations on the hot path.

The crate exposes both a library (`emma65`) and a binary (`emma65`). The
library contains two top-level modules:

- **`emulator`** — the CPU, memory bus, and device infrastructure, organized
  into submodules:
  `cpu` (opcode decode table, addressing modes, status register, variant
  selection), `bus`
  (address ranges and bus operations), `device` (device identity), `exec` (
  clock speed and step results), and `error` (typed errors for bus config, bus
  I/O, CPU construction, and execution).

- **`watch`** — a complete watchpoint expression pipeline: `Scanner` →
  `Vec<Token>` → `Parser`
  → `Expr` AST → `Compiler` → `Vec<OpCode>` → `Evaluator` → `Operand`. The
  scanner and parser use zero-copy techniques — token text slices borrow
  directly from the source string — so the pipeline produces no heap
  allocations until bytecode emission. The `WatchCompiler` and
  `WatchEvaluator` types are the primary public entry points; `WatchEvaluator`
  owns variable name-to-index mappings and persistent variable storage so that
  watchpoint variables survive across steps.

The binary (`src/main.rs`) wires the library together with a concrete
`WatchContext`
implementation for the WDC 65C02 (`src/wdc6502.rs`) and serves as both a usage
example and a manual exercise harness.

```
cargo build      # build
cargo test       # run all tests
cargo clippy     # lint
```