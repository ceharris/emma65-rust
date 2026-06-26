Debugger Implementation Plan
============================

This plan implements the emma65 debugger as a sequence of user stories,
ordered by priority and dependency. Each story is scoped for a single
pull request with clear acceptance criteria.

Project Structure
-----------------

- **Workspace layout:** A `debugger/` directory at the repo root,
  added to a Cargo workspace. Contains `src-tauri/` (Rust backend
  binary depending on `emma65` as a path dep) and `frontend/`
  (TypeScript + React + Vite + Sass + Xterm.js).
- **CPU thread model:** A dedicated OS thread runs the CPU loop;
  Tokio handles IPC, terminal bridge, and event emission. Channels
  connect the two.
- **Frontend stack:** TypeScript (strict), React, Vite, Sass, Xterm.js.
  No heavy state management library or UI component framework.

---

Stories
-------

### 1. Tauri Project Scaffold

Create the workspace structure with an empty dark-themed debugger window.

**Scope:**
- Add `debugger/` as a Cargo workspace member.
- Initialize Tauri 2 project (`src-tauri/` with `Cargo.toml`,
  `tauri.conf.json`).
- Initialize frontend (`frontend/` with `package.json`, Vite config,
  TypeScript, React, Sass).
- Main window renders a dark background with placeholder text.
- `cargo tauri dev` launches successfully.

**Acceptance criteria:**
- Running `cargo tauri dev` from `debugger/` opens a window with a
  dark background.
- The project builds cleanly (`cargo build` at workspace root, `npm
  run build` in `frontend/`).

---

### 2. Config Loading and Emulator Session Construction

The debugger binary loads emulator configuration and constructs a
session, confirming successful startup in the UI.

**Scope:**
- The debugger binary reads
  `~/.emma/debugger/default/emulator.toml` using the existing
  `emulator::config` infrastructure.
- A `PipeTransport::pair()` is created; the local end is injected
  into `InstantiationContext.console_transport`.
- `Config::build()` produces an `EmulatorSession`.
- On success, a status message ("Emulator session ready") is
  displayed in the main window.
- On failure (missing config, build error), the error is displayed
  in the main window and the process exits with a non-zero code.

**Acceptance criteria:**
- With a valid `emulator.toml` in place, launching the debugger shows
  "Emulator session ready" (or similar) in the main window.
- With an invalid or missing config, the error is reported and the
  process exits cleanly.

---

### 3. Terminal Window and Free-Run Execution

The user can run a program on the emulated CPU and interact with it
via the terminal. The process exits cleanly on STP.

**Scope:**
- Open a second OS window (Tauri multi-window) with Xterm.js
  configured at 80×24.
- Implement the Terminal Bridge (Tokio task polling the remote end of
  the PipeTransport, emitting `"terminal-output"` events).
- Implement the `write_terminal` Tauri command for keyboard input.
- Launch the CPU on a dedicated thread in free-run mode using the
  existing `exec::run()` infrastructure.
- When the CPU executes STP, the emulator signals the backend, which
  closes both windows and exits with code 0.

**Acceptance criteria:**
- With a program that produces terminal output (e.g. TaliForth or a
  simple echo loop), launching the debugger opens the terminal window
  and displays the program's output.
- Typing in the terminal window sends input to the emulated program.
- When the program executes STP, the debugger exits cleanly.

---

### 4. Disassembly View and Register View (Step Into)

The user can single-step the CPU and observe instruction-level state
changes.

**Scope:**
- Debugger window shows a three-column layout (initially only center
  and right columns populated).
- Center column: disassembly view showing instructions around the
  current PC, with the current instruction highlighted.
- Right column (top): register view displaying A, X, Y, PC, S, P
  with hex radix and the 8-character flag display.
- A "Step Into" button (and F11 shortcut) executes one instruction
  and refreshes both views.
- The CPU starts halted (not free-running). The user steps manually.

**Acceptance criteria:**
- On launch, the disassembly view shows instructions starting at the
  reset vector address with PC highlighted.
- Pressing Step Into advances PC by one instruction; disassembly and
  registers update to reflect the new state.
- The flag display shows changed flags in a distinct color after each
  step.

---

### 5. Memory View

The user can inspect memory contents at any address.

**Scope:**
- Left column: memory view displaying 16 rows × 16 bytes (one
  256-byte page) in the format specified (address, hex bytes grouped
  8+8, ASCII decode grouped 8+8).
- Address input field accepting hex (`$` or `0x` prefix) or decimal.
  Enter navigates to the page containing that address. The specified 
  address is AND'ed with 0xfff0 to keep the display paragraph-aligned.
- Scrolling: line-by-line and page-by-page via scroll wheel, arrow
  keys, Page Up/Down.
- All reads use `Bus::peek_range`.

**Acceptance criteria:**
- Entering an address in the input field displays the 256-byte page
  containing that address. The display remains paragraph-aligned when an 
  address such as 0x4005 is entered (i.e. the displayed row containing that 
  address starts at 0x4000).
- Scrolling moves the view by lines or pages as expected.
- The ASCII column shows printable characters and `.` for
  non-printable bytes.

---

### 6. Stack View

The user can observe the current stack state.

**Scope:**
- Right column (middle): stack view showing word pairs aligned to the
  current stack pointer, as specified (chevron marker, `--`
  placeholder, ~8 visible word pairs) -- see `doc/debugger-ui-spec.md` for 
  the detailed design example.
- Alignment toggle (even/odd).
- View updates on each step.

**Acceptance criteria:**
- After pushing values onto the stack (via stepping through PHA/JSR
  instructions), the stack view shows the pushed values with correct
  alignment and the chevron tracking the stack pointer.
- When popping values from the stack, the chevron pointer remains 
  stationary the stack of values is scrolled up such that the chevron is 
  pointing at the new top of stack, a placeholder is introduced if needed.
- The `--` placeholder correctly indicates the next write position.

---

### 7. Auto-Step

The user can watch execution proceed at a controlled pace.

**Scope:**
- An "Auto-Step" toggle button (Ctrl+Shift+F5) starts/stops timed
  single-stepping.
- A speed control (slider or numeric input) sets the interval in milliseconds 
  between steps. The allowed range for the interval is 50 ms to 5000 ms 
  (inclusive). The default value is 500 ms. The slider uses tiered step sizes:
  25 ms (50–500 ms), 50 ms (500–1000 ms), 100 ms (1000–2000 ms), 250 ms
  (2000–5000 ms).
- All views (disassembly, registers, memory, stack) update after each
  step.
- Execution halts on STP.

**Acceptance criteria:**
- Pressing Auto-Step begins stepping at the configured interval; the
  user can observe the program executing in slow motion.
- Changing the interval immediately affects step timing. The control allows 
  selection of an interval between 50 ms and 5000 ms, and the slider 
  proceeds smoothly in each tier of step sizes.
- Pressing Auto-Step again (or Stop) halts execution.

---

### 8. Simple Breakpoints

The user can set breakpoints by clicking in the disassembly gutter.
Auto-step halts when a breakpoint is reached.

**Scope:**
- Gutter column in the disassembly view; clicking toggles a
  breakpoint at that address.
- Active breakpoints display a filled circle (●).
- The backend checks breakpoints after each auto-step tick.
  Execution halts when a breakpoint address is reached.
- Minimal UI: toggle only (no disable, no context menu yet).

**Acceptance criteria:**
- Clicking the gutter at an instruction address shows the ● marker.
- Clicking again removes it.
- During auto-step, execution halts at the breakpoint address and
  all views update.

---

### 9. Technical: Step Over and Step Return in Emulator

Add Step Over and Step Return logic to the emulator library with
tests.

**Scope:**
- Implement Step Over: when the current instruction is JSR, set a
  temporary breakpoint at PC+3 and run until it is reached (or
  another breakpoint/STP intervenes).
- Implement Step Return: run until the stack pointer rises above its
  current value (indicating the current subroutine has returned).
- Unit tests validating both behaviors, including edge cases (nested
  calls, BRK, interrupts).

**Implementation notes:**
- Both operations fit naturally as free functions (or methods) alongside
  `run()` in `exec/mod.rs`; no changes to `Cpu::step()` are needed.
- Step Return uses the S-threshold approach: record `initial_s =
  cpu.registers().s` before the loop, then step until `S > initial_s`
  or a non-`Executed` result occurs. Reading the return address from
  the stack is not reliable because the subroutine may have pushed
  registers above it, making the return address hard to locate without
  simulating the call frame.

**Acceptance criteria:**
- `cargo test` passes with new tests covering Step Over (JSR treated
  as atomic, non-JSR behaves like Step Into) and Step Return
  (execution halts after RTS/RTI unwinds the frame).

---

### 10. Free-Run Execution Controls

The user can run the program at full speed and stop it.

**Scope:**
- Execution control buttons in the disassembly panel header:
  Run/Continue (F5), Stop (Shift+F5), Step Over (F10), Step Return
  (Shift+F11).
- Run/Continue launches free-run on the CPU thread. The terminal
  stays live; debugger panels refresh periodically.
- Stop halts execution; all views update to the halted state.
- Step Over and Step Return use the emulator support from story 9.
- Execution also halts on breakpoint hit (if breakpoints are
  available).

**Acceptance criteria:**
- Pressing Run starts free-run; terminal I/O works; pressing Stop
  halts execution and views show current state.
- Step Over on a JSR instruction advances past the subroutine call.
- Step Return inside a subroutine runs until the subroutine returns.

---

### 11. Register Editing

The user can modify register values during a halt.

**Scope:**
- Double-click a register value to open an inline edit field.
- Input is interpreted in the current display radix by default;
  explicit prefixes (`$`, `0x`, `0o`, `0b`, `.`, `0d`) override.
- Enter commits; Escape cancels. Invalid input is rejected (field
  stays open with visual feedback).
- All dependent views (disassembly for PC change, stack for S
  change) refresh after commit.

**Acceptance criteria:**
- Double-clicking A and typing `FF` (in hex mode) sets A to 0xFF;
  the register view updates.
- Changing PC causes the disassembly view to re-center on the new
  address.
- Changing S updates the stack view.

---

### 12. Initial Watchpoints (Display Only)

The user can view watchpoint expressions and their evaluated values.

**Scope:**
- On startup, load watchpoint expressions from
  `~/.emma/debugger/default/watchpoints.emw`.
- Compile all expressions via `WatchCompiler`. On error, print
  diagnostics to stderr and exit with non-zero code.
- Left column (bottom, below Memory view): watchpoint view showing each
  expression (truncated with ellipsis control for long expressions), its
  current value, and a status indicator (true/false/error).
- Values update after each step, auto-step tick, or halt from
  free-run.
- Value radix cycle control (hex, unsigned decimal, signed decimal,
  octal, binary).
- Display-only: no add/edit/remove/enable/disable controls.

**Acceptance criteria:**
- With a `watchpoints.emw` file containing valid expressions,
  launching the debugger shows watchpoint values that update as
  execution proceeds.
- A long expression is truncated with an ellipsis control that
  reveals the full text on click.
- If the file has syntax errors, the debugger prints diagnostics and
  exits.

---

### 13. Theme Selection

The user can choose between Auto, Dark, and Light modes.

**Scope:**
- A theme selector control (dropdown or segmented control) in the
  application settings or toolbar.
- Auto follows the OS system theme.
- All panels, views, and controls update colors accordingly.
- Theme preference persists (written to a UI config file in
  `~/.emma/debugger/default/`).

**Acceptance criteria:**
- Selecting Dark applies the dark theme across all panels.
- Selecting Light applies the light theme.
- Selecting Auto follows the current OS setting and reacts to
  runtime changes.
- The choice persists across debugger restarts.

---

### 14. Memory View Follow Modes

The memory view can automatically track PC or a zero-page pointer.

**Scope:**
- Follow mode toggle/selector: Off, Follow PC, Follow Zero-Page
  Pointer.
- Follow PC: after each step/halt, the memory view navigates to the
  page containing the current PC.
- Follow Zero-Page Pointer: a configurable zero-page offset (user
  input); the view tracks the 16-bit address stored at that location.
- When auto-navigating, the memory view remains paragraph-aligned; i.e. the 
  address at the start of each line is evenly divisible by 16.
- Manual navigation disengages follow mode; a toggle re-engages it.

**Acceptance criteria:**
- With Follow PC enabled, stepping through code keeps the memory view
  centered on the current instruction area.
- With Follow ZP enabled and a pointer at 0x10 pointing to 0x0400,
  the memory view shows the page at 0x0400. When the pointer changes,
  the view follows.
- After any change in the followed pointer, the memory view remains 
  paragraph-aligned.
- Scrolling manually disengages follow; clicking the follow toggle
  re-engages.

---

### 15. Advanced Breakpoint UI

The user has richer control over breakpoints.

**Scope:**
- Right-click context menu on disassembly rows: set breakpoint,
  remove breakpoint, disable breakpoint, set breakpoint at typed
  address.
- Disabled breakpoints show a dimmed or hollow marker in the gutter.
- Disabled breakpoints do not halt execution.

**Acceptance criteria:**
- Right-click → Disable grays the marker; execution no longer stops
  at that address.
- Right-click → Remove clears the marker entirely.
- "Set breakpoint at address..." opens an input; entering a valid
  address sets a breakpoint there.

---

### 16. Watchpoints: Add and Remove

The user can manage watchpoints from within the debugger.

**Scope:**
- A "+" button adds a new watchpoint (opens an input popover).
- A delete button (or Delete key) removes a selected watchpoint.
- Changes are written back to `watchpoints.emw` on commit.

**Acceptance criteria:**
- Adding a valid expression appends it to the watchpoint view and
  persists to the file.
- Removing a watchpoint removes it from view and from the file.
- Adding an invalid expression shows a compilation error in the
  popover; the watchpoint is not added until corrected or cancelled.

---

### 17. Watchpoints: Edit

The user can modify existing watchpoint expressions with compiler
feedback.

**Scope:**
- Double-click an expression to open the edit popover with the
  current expression text.
- Real-time or on-submit compilation feedback: errors are displayed
  inline and must be resolved before the watchpoint can be saved.
- On save, the watchpoints file is updated.

**Acceptance criteria:**
- Editing an expression and saving updates the displayed value.
- Introducing a syntax error shows the error message; the save
  button is disabled until the error is corrected.
- Pressing Escape cancels the edit, reverting to the original
  expression.

---

### 18. Watchpoints: Enable/Disable

The user can disable watchpoints without removing them.

**Scope:**
- Checkbox toggle on each watchpoint row.
- Disabled watchpoints are grayed and not evaluated during execution
  (they do not trigger halts, and their value column shows the last
  known value or a dash).
- Enable/disable state persists in the watchpoints file (or a
  companion metadata file).

**Acceptance criteria:**
- Unchecking a watchpoint grays it out; it no longer halts execution
  even if its expression evaluates to true.
- Re-enabling resumes evaluation.
- The state survives a debugger restart.

---

### 19. Watchpoints: View Variables

The user can inspect watch variables (those assigned via `:=`).

**Scope:**
- A collapsible section in the watchpoint panel listing all current
  variable names and their values.
- Values update after each evaluation cycle.
- Value radix follows the watchpoint panel's radix setting.

**Acceptance criteria:**
- After stepping through code where a watchpoint assigns `x := PC`,
  the variable `x` appears in the variables section with the correct
  value.
- The variable list updates as new variables are created by walrus
  expressions.

---

### 20. Watchpoints: Edit Variable Values

The user can manually change a watch variable's value.

**Scope:**
- Double-click a variable value in the variables section to edit it.
- Input interpretation follows the same radix/prefix rules as
  register editing.
- The new value takes effect on the next evaluation cycle.

**Acceptance criteria:**
- Setting a variable to a new value is reflected in subsequent
  watchpoint evaluations that reference it.
- The change persists until overwritten by a `:=` expression or
  another manual edit.

### 21. Stack View — Value Radix Cycling

The user can view stack cell values in their preferred numeric base.

**Scope:**
- Add a radix-cycling control to the stack view header (similar to the
  register radix controls).
- Clicking the control cycles through: hex → unsigned decimal → signed
  decimal → octal → hex.
- The default radix is hexadecimal.
- The address column always displays in hexadecimal regardless of the
  selected radix.
- The `--` placeholder is unaffected by radix selection.

**Acceptance criteria:**
- Clicking the radix control cycles through all four radices in order
  and wraps back to hex.
- All byte values in the stack view render correctly in each radix.
- The address column always shows hex addresses.
- The `--` placeholder always displays as `--`.

---

Story Dependency Notes
----------------------

The stories are ordered by priority and natural dependency. Key
dependencies:

- Stories 1 → 2 → 3 form the scaffolding sequence.
- Story 4 introduces the halted-start mode and stepping; stories 5,
  6, and 12 build on this state.
- Story 7 (auto-step) provides the execution mechanism used to
  validate story 8 (breakpoints).
- Story 9 (emulator support) is a prerequisite for story 10's Step
  Over and Step Return controls.
- Stories 16–20 build on story 12's watchpoint infrastructure.

---

GitHub Issues
-------------

| Story | Title                                              | Issue |
|-------|----------------------------------------------------|-------|
| 1     | Tauri Project Scaffold                             | #56   |
| 2     | Config Loading and Emulator Session Construction   | #57   |
| 3     | Terminal Window and Free-Run Execution             | #58   |
| 4     | Disassembly View and Register View (Step Into)     | #59   |
| 5     | Memory View                                        | #60   |
| 6     | Stack View                                         | #61   |
| 7     | Auto-Step                                          | #62   |
| 8     | Simple Breakpoints                                 | #63   |
| 9     | Technical: Step Over and Step Return in Emulator   | #64   |
| 10    | Free-Run Execution Controls                        | #65   |
| 11    | Register Editing                                   | #66   |
| 12    | Initial Watchpoints (Display Only)                 | #67   |
| 13    | Theme Selection                                    | #68   |
| 14    | Memory View Follow Modes                           | #69   |
| 15    | Advanced Breakpoint UI                             | #70   |
| 16    | Watchpoints — Add and Remove                       | #71   |
| 17    | Watchpoints — Edit                                 | #72   |
| 18    | Watchpoints — Enable/Disable                       | #73   |
| 19    | Watchpoints — View Variables                       | #74   |
| 20    | Watchpoints — Edit Variable Values                 | #75   |
| 21    | Stack View — Value Radix Cycling                   | #82   |
