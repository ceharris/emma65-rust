Debugger UI/Functional Design Specification
============================================

Overview
--------

This document specifies the visual layout, interaction patterns, and
functional architecture for the emma65 debugger — an IDE-like debugging
interface for the CMOS 65C02 emulator. The debugger is built on Tauri 2
(Rust backend) with a React frontend rendered in the platform web view.

Architecture
------------

### Application Windows

The debugger consists of two OS-level windows:

1. **Debugger window** — houses all inspection and control panels in a
   three-column layout.
2. **Terminal window** — a separate OS window providing a VT220/Xterm-
   compatible virtual terminal for the emulated console port.

Separating the terminal into its own window ensures:
- Fixed geometry (configurable, default 80×24) matching period software
  assumptions.
- Unambiguous focus semantics — keystrokes route to the emulated system
  when the terminal has focus, and to debugger controls when the debugger
  has focus.
- The debugger panels are not competing with a large terminal pane for
  screen real estate.

### IPC Model

Communication between the Tauri/Rust backend and the React frontend
follows a hybrid event/command pattern:

- **Events (backend → frontend):** Lightweight notifications signaling
  state transitions — step completed, breakpoint hit, CPU halted,
  execution started. These carry no large payload.
- **Commands (frontend → backend):** The frontend issues Tauri commands
  to fetch the specific data it needs to render: register values (via
  `Cpu` accessors), memory contents (via `Bus::peek` / `peek_range`),
  disassembly for an address range, watchpoint evaluation results.

During free-run execution, the terminal window remains fully live while
debugger panels refresh periodically for informational purposes. Exact
point-in-time consistency across panels is not required during free-run.
When the CPU halts (step, breakpoint, watchpoint), state is naturally
consistent because execution has stopped.

### Layout Philosophy

The initial layout uses a **hybrid** approach: a well-designed default
panel arrangement with resizable and collapsible panels, leaving the door
open for a fully dockable panel system in a future phase.

Debugger Window Layout
----------------------

Modern displays favor width over height. The three-column layout takes
advantage of this:

```
┌─────────────────┬───────────────────┬──────────────┐
│                 │                   │  Registers   │
│                 │                   │              │
│   Memory View   │  Disassembly View ├──────────────┤
│                 │                   │    Stack     │
│                 │                   │              │
│                 │                   ├──────────────┤
│                 │                   │ Watchpoints  │
└─────────────────┴───────────────────┴──────────────┘
```

- **Left column:** Memory view (widest — approximately 80 character
  columns to accommodate 16 bytes/row with hex and ASCII).
- **Center column:** Disassembly view with integrated execution controls.
- **Right column:** Registers (top), Stack (middle), Watchpoints (bottom).

Panels are resizable at their borders. Each panel may be collapsed to
give more space to adjacent panels.

Memory View
-----------

### Display Format

Each row displays 16 bytes of memory:

```
0400: 4C 00 06 00 00 00 00 00  00 00 00 00 00 00 00 00  L.......  ........
```

Fields: address (4 hex digits + colon), 16 hex byte pairs (grouped 8+8
with an extra space between groups), ASCII decode (two 8-character
segments with a space between, congruent with the hex grouping;
printable characters shown, non-printable as `.`).

The view displays one 256-byte page (16 rows) at a time.

### Navigation

- **Address input field:** Accepts hex (with `$` or `0x` prefix) or
  decimal addresses. Pressing Enter navigates to the page containing
  that address.
- **Scroll:** Line-by-line or page-by-page scrolling via scroll wheel,
  keyboard (arrow keys, Page Up/Down), or scroll bar.

### Follow Mode

The memory view can be locked to automatically follow:
- **Program Counter (PC):** Displays the page containing the current PC.
- **Zero-page pointer:** Displays the page addressed by a 16-bit word
  stored at a configurable zero-page offset. This is particularly useful
  for watching data accessed via indirect addressing.

Follow mode is disengaged by manual navigation and re-engaged via a
toggle control.

### Implementation

All memory reads use `Bus::peek` and `Bus::peek_range` for idempotent,
side-effect-free access to both memory and device registers.

Disassembly View
----------------

### Display Format

Each row displays one disassembled instruction:

```
 ● 0600: 4C 00 06   JMP  $0600
   0603: A9 FF      LDA  #$FF
   0605: 8D 00 D0   STA  $D000
```

Columns: breakpoint gutter, address, object code bytes (up to 3),
mnemonic, operands.

### Scrolling Behavior

The view uses **scroll-on-edge** behavior:
- During sequential execution, the PC marker advances downward without
  scrolling.
- When the PC approaches the bottom edge of the visible range, the view
  scrolls to keep context below.
- On a far jump (address outside the visible range), the view re-centers
  on the new PC.

### Execution Controls

Controls are integrated into the disassembly panel header:

| Control          | Default Shortcut | Behavior                                  |
|------------------|------------------|-------------------------------------------|
| Run / Continue   | F5               | Start or continue free-running execution   |
| Stop             | Shift+F5         | Halt free-running execution                |
| Step Over        | F10              | Execute one instruction; treat JSR as atomic |
| Step Into        | F11              | Execute one instruction; enter subroutines |
| Step Return      | Shift+F11        | Run until current subroutine returns       |
| Auto-Step        | Ctrl+Shift+F5    | Toggle timed single-step at configurable interval |

**Auto-step** provides a speed control (slider or numeric input) that
sets the interval between steps in milliseconds. All views update after
each step, creating a slow-motion execution replay.

**Free-run** executes at the configured clock speed. The terminal stays
live; debugger panels refresh periodically. Execution halts on breakpoint
or enabled watchpoint evaluating to true.

### Breakpoints

- **Toggle:** Click the gutter column at any instruction row to set or
  clear a breakpoint. Active breakpoints display a filled circle (●).
- **Context menu:** Right-click a row for additional options: disable
  (without removing), remove, or set breakpoint at a typed address.
- **Scope:** Breakpoints are simple address matches. Conditional halting
  is handled by the watchpoint system.

Register View
-------------

### Register Groups

**Accumulator and Index Registers (A, X, Y):**
- Displayed as a group sharing a common radix.
- Radix cycle (via a toggle control): hexadecimal → unsigned decimal →
  signed decimal → binary → octal.
- Default radix: hexadecimal.

**Address and Status Registers (PC, S, P):**
- Displayed as a group sharing a common radix (independent from A/X/Y).
- Radix cycle: hexadecimal → unsigned decimal → octal.
- Default radix: hexadecimal.

**Processor Status (P) — Flag Display:**

In addition to its numeric value, the P register displays a compact
8-character flag sequence. Each flag bit that is set shows its name
letter; unset flags show `-`. The unused bit is always `-`.

```
------ZC
```

In this example, only the Zero and Carry flags are set. Flags whose
values changed as a result of the last instruction executed (in step or
auto-step mode) are displayed in a distinct color for emphasis.

### Editing Registers

Double-click a register value to open an inline edit field. The default
input radix matches the register group's current display radix, so the
user can type a bare value without a prefix and it will be interpreted in
the displayed radix. An explicit prefix overrides: `$` or `0x` for hex,
`0o` for octal, `0b` for binary, `.` or `0d` for decimal. Press Enter
to commit, Escape to cancel.

Stack View
----------

### Display Format

The stack view shows word pairs aligned to the current pointer:

```
> 0100 -- 00
  0102 01 02
  0104 03 04
  0106 05 06
  0108 07 08
```

- **Addresses** ascend from top to bottom (matching memory layout).
- **Chevron (`>`)** marks the row containing the current stack pointer.
- **`--`** placeholder indicates the next cell to be written.
- **Alignment toggle:** Switch between even-aligned and odd-aligned word
  pairing.
- **Display radix:** Always hexadecimal.

### Push Behavior

A push fills the `--` slot first (the chevron stays on the same row).
When both bytes of a pair are filled, a new row appears at the top with
the chevron following the new SP position.

### Visible Range

Approximately 8 word pairs (16 bytes) are visible, centered on the
current stack pointer.

Watchpoint View
---------------

### Display Format

Each watchpoint row shows:

```
[✓] W[$10] == $0400 && B[...   $0001   ●
```

Fields: enable/disable checkbox, expression text (truncated with tooltip
for full text), current evaluated value (displayed in a selectable radix),
status indicator.

**Value radix:** A control cycles through hexadecimal, unsigned decimal,
signed decimal, octal, and binary display for the evaluated values. The
selected radix applies to all watchpoint values uniformly.

**Status indicators:**
- Filled circle (●) or highlight: expression evaluates to true (non-zero).
- Empty/dim: expression evaluates to false (zero).
- Error icon: evaluation error (invalid address, etc.).

### Interaction

- **Add:** Click a "+" button or an empty slot to open a popover with a
  full-width text input for entering the expression.
- **Edit:** Double-click an existing expression to open the same popover
  for editing.
- **Remove:** Delete key or X button on the row.
- **Enable/Disable:** Checkbox toggle. Disabled watchpoints are grayed
  and not evaluated during execution.

### Watch Variables

The watchpoint view provides a means to inspect and edit watch variables
(those assigned via the `:=` walrus operator). Variables are scoped to
the entire set of watchpoint expressions — a variable assigned in one
watchpoint is visible to all others. A collapsible section or sub-panel
lists current variable names and values, with the ability to manually
change a variable's value.

### Implementation

The watchpoint system leverages the existing `watch` module:
- `WatchCompiler` compiles expression source into `Watchpoint` opcodes.
- `WatchEvaluator` evaluates all enabled watchpoints against current
  CPU/memory state via the `WatchContext` trait.
- Variables (`:=` walrus operator) persist across evaluation cycles.

Terminal Window
--------------

### Characteristics

- Separate OS-level window (Tauri multi-window support).
- VT220/Xterm terminal emulation with full escape sequence support.
- Configurable geometry: rows and columns set in session configuration,
  defaulting to 80 columns × 24 rows.
- Font: monospace, sized to fill the window at the configured geometry.

### Behavior

- The terminal is always live during free-run execution, displaying
  output from the emulated console device as it occurs.
- Keyboard input to the terminal window is delivered to the emulated
  console device's receive buffer.
- During single-step modes, the terminal still reflects any output
  produced by the most recent step.
- Terminal I/O is exclusively connected to the emulated console device
  via a `PipeTransport`. Emulated ACIA devices use external applications
  (e.g. Minicom in a GNOME Terminal) connected via PTY or Unix socket
  transports — they are not rendered in the debugger terminal window.

Keyboard Shortcuts
------------------

All keyboard shortcuts are customizable via application settings. The
default mapping follows VS Code conventions:

| Action           | Default Binding  |
|------------------|------------------|
| Run / Continue   | F5               |
| Stop             | Shift+F5         |
| Auto-Step        | Ctrl+Shift+F5    |
| Step Over        | F10              |
| Step Into        | F11              |
| Step Return      | Shift+F11        |

Additional shortcuts for panel navigation, radix cycling, and memory
navigation are TBD during implementation.

Theme
-----

- **Auto** (default): follows the system theme (dark or light) as
  reported by the OS.
- **Dark:** dark background, high-contrast monospace text for all data
  views.
- **Light:** light background, for high-ambient-light environments or
  personal preference.

Theme selection is available via application settings with three choices:
Auto, Dark, Light.

All data views (memory, disassembly, registers, stack, watchpoints)
use a monospace typeface.

Deferred Items
--------------

The following are explicitly out of scope for this specification and will
be addressed in future phases:

- **Bus/device configuration UI:** Visual builder for assembling memory
  and devices. Configuration continues to use TOML files.
- **Session persistence:** Saving/restoring window positions, breakpoint
  sets, watchpoint lists, and last-used configuration.
- **Full dockable panels:** The hybrid layout (resizable, collapsible)
  serves as the starting point; full drag-and-dock flexibility is a
  future enhancement.
