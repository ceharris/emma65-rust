# The Debugger

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
  facility described in [Execution Tracing](the-emulator-core.md#execution-tracing)
- **Built-in terminal** — an Xterm/VT220-compatible terminal window wired
  directly to the configured, memory-mapped console device, so you can
  interact with a running program without any external terminal emulator or
  PTY setup

The debugger reads its emulator configuration from
`~/.emma/debugger/profiles/default/emulator.toml` (the same TOML format
described under [Running the Emulator](running-the-emulator.md)); watchpoints
are stored alongside it as `watchpoints.emw`. Its own UI preferences —
including light/dark theme — are not specific to any profile, and are read
from `~/.emma/debugger/config/ui.toml` instead.

## Watchpoint Expressions

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
