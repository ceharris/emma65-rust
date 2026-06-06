# WDC65C02 Emulator — Ideation Phase Transcript

## Initial Request

**User**: I want you to help me create a design for an emulator for the WDC65C02 microprocessor using the Rust language. The emulator should be realized as a Rust module whose public API is to be used by other components of a development and debugging platform. The emulator does not need to be cycle accurate. It should support a configuration of emulated ROM, RAM, and IO devices. It should allow both single-step execution (fetch and execute a single instruction at the current program counter) and asynchronous free-running execution (start executing at the current program counter). The call to start free running execution should return a future that can be used to stop execution. It should allow configuration of a closure that will evaluate watch expressions before executing the next instruction. Ask me additional questions to complete your understanding of what's required.

---

## Clarifying Questions and Answers

### Memory & Bus

**Q1**: Should the address map (which ranges are ROM, RAM, or IO) be configurable at construction time, or do you want a fixed layout (e.g., zero page RAM, ROM at $FF00-$FFFF for vectors, etc.)?

**A1**: The address map should be configurable. I think memory should be a separate subsystem that is configured according to a configuration specified by the user of the emulator. The CPU should get the configured memory subsystem at the time of construction.

---

**Q2**: For IO devices, are you thinking a trait that users implement, with memory-mapped registration at specific address ranges? Or something more abstract?

**A2**: IO devices should be implementations of a trait. When configuring the memory subsystem, the user will map the desired devices into the address space. Emulations of some common IO devices (e.g. the 6522 VIA, the 6551 ACIA) will be included in the emulator module. The external interface to these IO devices will sockets (TCP or UNIX domain) or in some cases (e.g. a serial terminal) a master-slave PTY pair.

**Follow-up Q**: Should the socket/PTY plumbing live inside the device emulation (e.g., the ACIA implementation owns its PTY), or should it be a separate layer that the device delegates to?

**Follow-up A**: The socket/PTY plumbing should be in a separate layer. This will not only facilitate testing of the IO device emulation, but it will also allow a given IO device to be configured with the user's preferred backend plumbing.

---

**Q3**: Should memory reads/writes be observable from outside (e.g., a bus trace callback for the debugger)?

**A3**: Memory reads and writes should be traceable via a callback. An implementation of the callback could stream a timestamped log of memory reads and writes to disk using a compact and efficient representation, supporting long traces.

---

### Interrupts

**Q4**: How should IRQ, NMI, and RESET be triggered — method calls on the emulator from external code? Should they be safe to call from another thread during free-running execution?

**A4**: It should be possible to trigger interrupts from either an I/O device implementation or from a debugger if not in free-running mode. Since IRQ is level triggered, there should be methods for asserting and releasing IRQ. An emulated IO device would assert when an interrupt condition occurs and would release after the interrupt condition is cleared (e.g. by reading a status register).

**Follow-up Q**: NMI is edge-triggered on the real chip — should I assume standard edge-triggered behavior for NMI as well?

**Follow-up A**: Assume that NMI is edge-triggered, just like the real hardware.

---

### Watch Expressions & Breakpoints

**Q5**: When the watch closure fires before each instruction, what should it return? A simple "continue or stop" decision? Or richer control like "stop and report reason"?

**A5**: Assume a simple boolean "should break" return value.

---

**Q6**: Should there also be explicit breakpoint support (break on PC address), or is the watch closure the sole mechanism for that?

**A6**: The watch expression language is parsed and compiled into a byte code for execution in a simple stack based evaluator. An ordinary breakpoint can be achieved using an expression such as `PC == 0x40c`. The debugger can provide UI for creating such breakpoints so the user doesn't have to enter the expression itself. Would there be advantages to an additional breakpoint mechanism?

**Assistant response**: A dedicated breakpoint mechanism (e.g., a simple hashset of PC addresses checked before each instruction) would have one advantage: **performance during free-running execution**. Checking `pc_breakpoints.contains(&addr)` is O(1) and branch-predictor-friendly, whereas evaluating even a simple bytecode expression has more overhead.

**Follow-up Q**: Would simple breakpoints be specified for the emulator as something like a vector of addresses to check?

**Follow-up A**: Yes, a `Vec<u16>` or `HashSet<u16>` of breakpoint addresses that the CPU checks against the PC before each instruction fetch.

**Decision**: Include both the simple breakpoint mechanism (fast PC-address check) and the watch expression evaluator for complex conditions. Either one triggering a break stops execution.

---

### State & Debugging

**Q7**: What CPU state needs to be inspectable — just registers/flags/PC/SP, or also things like cycle count, last instruction disassembly, etc.?

**A7**: The inspectable CPU state needs to include the registers and flags, but not cycle counts. I was thinking that instruction disassembly could be handled by a separate disassembler module since I'll also want to be able to display disassembly over ranges of addresses in the debugger.

**Observation**: The disassembler module would also need to know which instruction set it is using, but that doesn't seem like a significant complication either.

**Response**: Agreed — the disassembler just needs the same variant enum to decode those opcodes as valid mnemonics or show them as undefined/NOP. One shared enum, two consumers.

---

**Q8**: Should single-step return information about what just executed (opcode, affected addresses, etc.)?

**A8**: I was thinking that the debugger could support two similar modes for running (as opposed to single step). One of these modes would be the true free-running asynchronous mode. The other mode would repeatedly call the method to execute the next instruction. The latter would be much slower, and the delay could be configurable in the UI to allow the user to control how fast execution occurs. In this case, it would be helpful if the method used to execute one instruction could return a value to indicate that the instruction wasn't executed due to a watch or breakpoint.

**Follow-up Q**: Should the executed case also include what was executed (e.g., the PC before execution, the opcode), or should the debugger read CPU state separately after each step?

**Follow-up A**: The debugger can examine CPU state separately after each step. If there's a need to know what was executed, the trace facility previously mentioned could fill that need.

**Decision**: The step return value is a simple enum: Executed / Breakpoint / WatchTriggered / WatchError. The debugger queries state and uses the trace callback for details when needed.

---

### Async & Concurrency

**Q9**: What about the async runtime? Use a framework such as Tokio? Build our own on top of Rust's Async/Await and fundamental concurrency support? 

**A9**: How should I evaluate the choices for async runtime? In a C implementation, I might handle all of the asynchronous aspects with POSIX threads, locks, conditions, etc. Help me understand my options in Rust, since I'm less familiar with it.

**Assistant response**: Three main approaches:

1. **OS threads only (`std::thread` + `std::sync`)** — Closest to POSIX. Rust's `Mutex`, `RwLock`, `Condvar`, and channels work like their POSIX equivalents, but the type system enforces correct usage. Downside: managing many concurrent IO connections with raw threads is cumbersome.

2. **Fully async (tokio or similar)** — Great for IO-bound work (sockets, PTYs, debugger commands). Tokio is the dominant runtime. Downside: the CPU execution loop is CPU-bound, and long-running CPU-bound work on an async executor starves other tasks.

3. **Hybrid (recommended)** — Use tokio for the IO layer (device transports, debugger connections). Run the CPU execution loop on a dedicated OS thread, communicating with the async world through channels. Benefits:
   - Tight, efficient execution loop with no yielding overhead
   - Natural async IO for sockets and PTYs
   - Clean control flow: stop signal via channel, CPU thread halts, future resolves
   - `tokio::sync::oneshot` or `watch` channels for the stop signal
   - CPU thread checks stop channel at the same point it checks breakpoints/watches
   - Public API can be async even though CPU loop is synchronous internally
   - Tokio already needed for socket/PTY transports, not an additional dependency

Key concept from C: Rust's async is "colored" — async functions can only be called from async contexts. The hybrid approach keeps that boundary clean: CPU and memory subsystem are plain synchronous Rust, only the outer control API and IO transports are async.

**Decision**: Use the hybrid approach.

---

### Scope — Instruction Set

**Q10**: Do you need the full WDC65C02 instruction set (including the bit manipulation instructions like BBR/BBS/RMB/SMB), or just the base CMOS 65C02 additions over the NMOS 6502?

**A10**: I do want support for the full WDC65C02 instruction set, but perhaps this should be a configurable option (selecting either CMOS 65C02 or WDC65C02). What are the implications of making the instruction set selectable? Do the advantages outweigh the disadvantages?

**Assistant analysis**:

The WDC65C02 adds 34 opcodes over base CMOS 65C02: STP, WAI, BBR0-7, BBS0-7, RMB0-7, SMB0-7.

Advantages of making it selectable:
- Users targeting non-WDC 65C02 systems (Rockwell, Synertek) get validation that their code is portable
- Cheap to implement — a single config flag checked during decode of only 34 opcodes, or two lookup tables
- The kind of thing a development platform should offer

Disadvantages:
- Slightly more configuration surface
- Minor historical muddiness (bit manipulation instructions originated with Rockwell)

**Decision**: Include selectable instruction set via `CpuVariant` enum (`Cmos65C02` / `Wdc65C02`).

---

### BCD Mode

**Q12**: Should BCD (decimal) mode of ADC/SBC be fully implemented?

**A12**: Absolutely. Many 6502 programs make fairly extensive use of BCD mode, so we'll want to handle it correctly for all applicable instructions.

---

## Additional Topics Discussed

### Invalid Opcodes on CMOS 65C02

**User**: Are there invalid opcodes for the CMOS 65C02 that have predictable behavior that should be implemented despite being invalid?

**Assistant**: Yes — on the CMOS 65C02, all undefined opcodes behave as NOPs, but with varying instruction lengths (1, 2, or 3 bytes). The PC must advance by the correct number of bytes for each, otherwise the instruction stream would be misaligned. This also affects the disassembler.

---

### Error Handling

**Discussion**: Watch expressions can result in evaluation errors (e.g., divide-by-zero). When that happens, the error should stop execution and propagate back to the UI. The watch closure should return a `Result<bool, WatchError>` where the error contains details needed to let the UI determine how to flag the expression.

This means the single-step result enum needs four variants:
1. **Executed** — instruction ran
2. **Breakpoint** — PC matched, instruction not executed
3. **WatchTriggered** — expression evaluated true, instruction not executed
4. **WatchError** — expression evaluation failed, instruction not executed, error details included

---

### Memory Subsystem Error Policies

**Discussion**: The memory subsystem should include configuration for:

- **Unmapped address access**: Option A — return a default value on read, ignore writes (silent). Option B — signal an error (stops execution, propagates to UI).
- **ROM writes**: Option A — silently ignore. Option B — signal an error.
- **Invalid opcode execution**: Same configurable pattern (NOP silently or signal an error).

These are set at memory subsystem/CPU construction time.

### IO Device/Transport Errors

**Discussion**: Device/transport errors (e.g., socket disconnect) should be reported via a separate async event channel — a callback or async channel — that the UI subscribes to. This keeps device errors decoupled from the CPU execution path.

**Decision**: Two error reporting paths:
1. **Synchronous, execution-stopping** — memory policy violations, invalid opcodes, watch errors, returned through step/run result
2. **Asynchronous, informational** — device/transport events, delivered via callback or channel to UI independently of CPU execution

---

### Watch Expression Evaluation — Side-Effect-Free Reads

**Discussion**: Some memory-mapped IO devices have side effects on read operations. The watch evaluator needs an object implementing a trait that provides methods performing idempotent reads of any specified address.

Methods needed:
- Memory reads: signed and unsigned variants, for widths of 1, 2, or 4 bytes
- Register reads: signed and unsigned variants, identified by `u32` ID from compiled expression
- Flag reads: identified by `u32` ID

All return values are `u32`. The evaluator instructions handle signed/unsigned interpretation. Signed variants expect the return value to be sign-extended when coerced to `u32`.

Final trait shape:
- `read_mem_u32(addr, width) -> u32` — zero-extended
- `read_mem_i32(addr, width) -> u32` — sign-extended from source width
- `read_register_u32(id) -> u32` — zero-extended
- `read_register_i32(id) -> u32` — sign-extended from register's natural width
- `read_flag(id) -> u32` — always 0 or 1

Watch expression evaluation is purely read-only with respect to machine state. Local variables defined via assignment in the expression are stored outside of the emulated machine (in the evaluator's own storage).
