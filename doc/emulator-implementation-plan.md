# Emma65 Emulator Module — Implementation Plan

## Context

Emma65 is a Tauri-based desktop application for running and debugging programs on an emulated 65C02 microcomputer. The `emulator` module is the core engine of the application, consumed by a Tauri UI that provides disassembly views, memory dumps, register display, watch expressions, execution controls, a VT220 terminal, and machine configuration.

The emulator architecture is defined in `emulator-specification.md`. The `watch` module (scanner, parser, compiler, evaluator) already exists as a sibling module in `emma65`. The Tauri UI has not been developed yet — the emulator API is the foundation everything else builds on.

This plan breaks the emulator into 14 merge requests, ordered bottom-up by dependency. Each MR is scoped to one subsystem with unit tests included. Integration and functional tests are introduced in later MRs as subsystems compose.

**Constraints:** Rust 2024 edition, Tokio for async, pragmatic use of crates (`bitflags`, `crossbeam`, `thiserror`, `tracing`, etc.).

---

## MR Sequence

### MR 1: Module Scaffolding and Foundation Types

Set up the `emulator` module structure within `emma65` and implement all shared types that downstream modules depend on.

**Scope:**
- `emulator/mod.rs` — module root, re-exports
- `emulator/error.rs` — `ExecError`, `BusError`, `BusConfigError`, `CpuBuildError`
- `emulator/cpu/status.rs` — `StatusRegister` (bitflags newtype over `u8`)
- `emulator/cpu/variant.rs` — `CpuVariant` enum, `InvalidOpcodePolicy`
- `emulator/cpu/opcodes.rs` — `Mnemonic` enum (all 65C02 mnemonics), `AddressingMode` enum, `DecodedOp` struct, 256-entry decode table, variant-aware validity
- `emulator/bus/region.rs` — `AddressRange`
- `emulator/exec/mod.rs` — `ClockSpeed` struct with `mhz()`, `hz()`, `unlimited()` constructors; `StepResult` enum

**Tests:**
- `StatusRegister` flag manipulation, `from_byte`/`to_byte` round-trip
- `ClockSpeed` conversions (MHz to Hz, unlimited sentinel)
- Decode table: every opcode decodes to correct mnemonic/mode/byte_len/base_cycles
- Variant filtering: WDC-only opcodes marked invalid under `Cmos65C02`
- `AddressRange` contains/overlap checks

**Crate dependencies introduced:** `bitflags`, `thiserror`

---

### MR 2: Bus and Memory Subsystem

Implement the configurable memory bus with RAM, ROM, and IO device regions.

**Scope:**
- `emulator/device/mod.rs` — `IoDevice` trait, `DeviceId` (trait definition only, no implementations)
- `emulator/bus/mod.rs` — `Bus` struct, `BusConfig` builder, `UnmappedPolicy`, `RomWritePolicy`
- Address decode with most-specific-wins overlap resolution
- `read()` / `write()` (side-effectful, CPU-facing)
- `peek()` / `peek_range()` (side-effect-free, debugger-facing)
- `load_rom()`

**Tests:**
- RAM read/write round-trip
- ROM read-only behavior (both Ignore and Error policies)
- Unmapped address behavior (both DefaultValue and Error policies)
- Most-specific-wins: IO device region shadows larger ROM region
- `AmbiguousOverlap` error for same-size conflicts
- `peek` does not trigger device side effects (using a mock `IoDevice`)
- Device offset translation: device receives address relative to its base
- `peek_range` across region boundaries

---

### MR 3: ALU

Implement all arithmetic, logic, and flag-update operations as pure functions consumed by the CPU instruction dispatcher.

**Scope:**
- `emulator/cpu/alu.rs` — all ALU operations:
  - `ADC` / `SBC` with binary and BCD modes
  - `AND`, `ORA`, `EOR`
  - `ASL`, `LSR`, `ROL`, `ROR` (accumulator and memory variants)
  - `INC`, `DEC` (memory and register variants)
  - `CMP`, `CPX`, `CPY`
  - `BIT` (including 65C02 immediate mode)
  - `TRB`, `TSB`
  - N, Z, C, V flag computations

**Tests:**
- Binary ADC/SBC: carry-in, carry-out, overflow detection, zero/negative flags
- BCD ADC/SBC: valid BCD digits, carries between nibbles, edge cases (99+1, 00-1)
- Shift/rotate: carry in/out, flag updates, accumulator vs memory result
- Compare: equal, less-than, greater-than, flag combinations
- BIT: zero-page and absolute (N/V from memory), immediate (Z only)
- TRB/TSB: mask behavior, Z flag

---

### MR 4: CPU Core — Instruction Execution

Implement the CPU struct, builder, addressing mode resolution, and instruction dispatch for all 65C02 opcodes.

**Scope:**
- `emulator/cpu/mod.rs` — `Cpu` struct, `Registers`, `CpuBuilder`
- Addressing mode resolution: effective address calculation for all 16 modes, including page-crossing cycle penalty detection
- Instruction dispatch: fetch-decode-execute for all valid opcodes
- `step()` — basic order of operations (fetch, decode, execute, accumulate cycles, tick devices, return `StepResult::Executed`)
- Invalid opcode handling per `InvalidOpcodePolicy` (NOP with correct PC advance, or error)
- `reset()` — read reset vector, initialize registers
- Public API: `registers()`, `registers_mut()`, `bus()`, `bus_mut()`, `variant()`, `clock_speed()`, `cycles()`

**Tests:**
- Each addressing mode resolves correct effective address (including page-crossing detection)
- Representative instructions per category: loads/stores, transfers, stack ops, branches, jumps, arithmetic, logic, shifts, flag manipulation
- `BRK` pushes PC+2 and status, reads IRQ vector
- `JSR`/`RTS` round-trip
- `RTI` restores flags and PC
- `PHX`/`PHY`/`PLX`/`PLY` (65C02 additions)
- `STZ`, `BRA` (65C02 additions)
- `BBR`/`BBS`/`RMB`/`SMB` (WDC-only, with variant gating)
- Invalid opcode: NOP policy advances PC correctly, Error policy returns `StepResult::Error`
- `reset()` reads vector from 0xFFFC/0xFFFD
- Device `tick()` called after each instruction with correct cycle count

---

### MR 5: Interrupt Controller and CPU Interrupt Handling

Implement the interrupt controller and wire interrupt servicing into the CPU step loop.

**Scope:**
- `emulator/interrupt.rs` — `InterruptController`, `IrqSource`
  - `assert_irq()` / `release_irq()` — level-triggered multi-source IRQ
  - `signal_nmi()` / `take_nmi()` — edge-triggered NMI
  - `poll_devices()` — sync device `irq_active()` into source set
  - `irq_active()` — true if any source asserted
- Wire into `Cpu::step()`:
  - NMI servicing (higher priority): push PC and status, read NMI vector (0xFFFA), set I flag
  - IRQ servicing (when I flag clear): push PC and status, read IRQ vector (0xFFFE), set I flag
  - WAI behavior: tick devices and poll, return `Waiting` until interrupt arrives
  - STP behavior: return `Stopped`, only `reset()` clears
- Public API: `interrupts()`, `interrupts_mut()`, `is_waiting()`, `is_stopped()`

**Tests:**
- IRQ asserted with I clear → CPU vectors through 0xFFFE, pushes correct state
- IRQ asserted with I set → no action
- Multi-source IRQ: release one source, IRQ remains active if another is asserted
- NMI edge detection: signal_nmi latches, take_nmi clears
- NMI has priority over simultaneous IRQ
- NMI servicing pushes correct state, vectors through 0xFFFA
- poll_devices syncs device irq_active into controller
- WAI: returns Waiting, then wakes on IRQ/NMI and services interrupt
- STP: returns Stopped, reset() clears

---

### MR 6: Debugger Support — Breakpoints, Watch Integration, Disassembler

Add all debugger-facing features: breakpoints, watch expression evaluation in the step loop, and disassembly.

**Scope:**
- Breakpoints on `Cpu`: `add_breakpoint()`, `remove_breakpoint()`, `clear_breakpoints()`, `breakpoints()`
- `WatchContext` trait implementation (borrows `&Bus` for peek and `&Registers`)
- Wire `WatchEvaluator` into `Cpu`: `evaluator()`, `evaluator_mut()`
- Update `step()` order of operations: check breakpoints (step 3), evaluate watches (step 4) before instruction execution
- `emulator/disasm/mod.rs` — `Disassembler`, `DisassembledLine`
  - `disassemble_one()` and `disassemble_range()` using `Bus::peek`
  - Operand formatting per addressing mode

**Dependencies:** This MR depends on the `watch` module's public API. The interface boundary is the `WatchEvaluator` and `WatchExpression` types, plus `compile_watch()`. Details to be confirmed when `emma65` source is available.

**Tests:**
- Breakpoint at PC → `StepResult::Breakpoint` before instruction executes
- Breakpoint removal, clear
- Watch expression triggers → `StepResult::WatchTriggered` with correct index and PC
- Watch error → `StepResult::WatchError`
- Disassemble known instruction sequences, verify mnemonic/operand text
- Disassemble across region boundaries
- Disassembler uses peek (no side effects)

---

### MR 7: Bus Trace

Add bus tracing with callback support and a built-in binary trace writer.

**Scope:**
- `emulator/bus/trace.rs` — `BusOp`, `TraceRecord`, `BusTraceCallback` trait, `BinaryTraceWriter`
- Wire into `Bus`: `set_trace_callback()`, invoke callback on every `read()` and `write()` (not `peek`)
- Near-zero cost when no callback registered (branch-predictable `is_some()` check)
- `BinaryTraceWriter`: 12-byte records to `BufWriter<File>`
- Timestamp: captured once per instruction by CPU, shared across bus accesses within that instruction

**Tests:**
- Callback receives correct addr/value/op for reads and writes
- Callback not invoked on peek
- No callback registered → no overhead (benchmark optional)
- BinaryTraceWriter produces correct binary format
- Timestamps group by instruction

---

### MR 8: Transport Layer and Device Events

Implement the transport abstraction and all four transport implementations, plus the async device event channel.

**Scope:**
- `emulator/transport/mod.rs` — `Transport` trait, `TransportError`
- `emulator/transport/pipe.rs` — `PipeTransport` (bidirectional OS pipe pair, non-blocking)
- `emulator/transport/tcp.rs` — `TcpTransport` (Tokio task, crossbeam channel bridge)
- `emulator/transport/unix_socket.rs` — `UnixSocketTransport` (Tokio task, crossbeam channel bridge)
- `emulator/transport/pty.rs` — `PtyTransport` (Tokio task, crossbeam channel bridge)
- `emulator/device/mod.rs` additions — `DeviceEvent`, `ErrorSender`, `ErrorReceiver`, `device_event_channel()`

**Tests:**
- PipeTransport: send/recv round-trip, try_recv returns None when empty, is_connected, shutdown
- TcpTransport: connect, send/recv, disconnect detection
- UnixSocketTransport: connect, send/recv, disconnect detection
- PtyTransport: open, send/recv, shutdown
- DeviceEvent channel: send events, receive from other thread

**Crate dependencies introduced:** `crossbeam-channel`

---

### MR 9: Console Device

Implement the simple polling console device — the first concrete `IoDevice` implementation.

**Scope:**
- `emulator/device/console.rs` — `Console` implementing `IoDevice`
  - Offset 0 read (Data Input): when Data Latch zero, returns non-zero if byte available, zero if none
  - Offset 0 read (Data Input)) when Data Latch non-zero, returns latched value, and resets Data Latch to zero
  - Offset 0 write (Data Output): sends byte to transport
  - Offset 1 read (Data Latch): reads and latches input byte
  - Offset 1 write (Data Latch): zero clears latch, non-zero simulates input
  - `tick()` — no-op (no timers)
  - `irq_active()` — always false (poll-only)
  - `attach_transport()`, `set_error_sender()`

**Tests:**
- Write to offset 0 → byte appears on transport
- Transport sends byte → offset 1 read latches it, offset 0 reads non-zero
- Latch non-zero → offset 0 reads latched value resets latch register to zero
- Clear latch → offset 0 reads zero
- Simulate input via offset 1 write
- No transport attached → reads return 0, writes are silent
- peek does not consume input or trigger transport IO

**Integration test:** Wire Console into a Bus, execute a short program that writes bytes — verify they appear on a PipeTransport.

---

### MR 10: ACIA Devices (Acia6551, Mc6850)

Implement both serial communication interface devices.

**Scope:**
- `emulator/device/acia6551.rs` — `Acia6551` implementing `IoDevice`
  - 4 registers: data, status, command, control
  - TX/RX via transport
  - IRQ support (receive data ready, transmit data register empty)
  - `tick()` for baud rate timing based on control register settings
- `emulator/device/mc6850.rs` — `Mc6850` implementing `IoDevice`
  - 2 addresses (status/control at offset 0, data at offset 1)
  - Clock divider, word format, interrupt enables via control register
  - Master reset via control register bits
  - IRQ for receive and transmit

**Tests per device:**
- Register read/write behavior matches hardware data sheet
- TX: write data register → byte appears on transport
- RX: transport sends byte → status register shows data ready, data register returns byte
- IRQ assertion on receive-ready, transmit-ready
- Control register changes baud rate / word format
- Mc6850: master reset clears state
- peek returns register values without consuming data or clearing status bits

---

### MR 11: VIA Peripheral Protocol Codec

Implement the protocol codec for VIA peripheral communication, as a standalone module that can be tested independently of the VIA emulation and transport layer. This module is part of the public API surface for SPI consumers building external peripherals. The full protocol specification is in `emulator-via-protocol.md`.

**Scope:**
- `emulator/device/via_protocol.rs` — protocol codec module
  - `ViaProtocolFormat` enum: `Binary`, `Ascii`
  - `ViaProtocolMessage` enum: `PortStateChange { port, value }`, `ControlSignalChange { signals, state }`
  - `ViaProtocolEncoder` — formats outgoing messages in binary or ASCII
  - `ViaProtocolDecoder` — parses incoming byte stream into messages, handling:
    - Format auto-negotiation (first byte high bit determines binary vs ASCII)
    - ASCII case insensitivity, whitespace/control character skipping
    - Silent ignore of invalid data (no error signaling per spec)
    - Full message consumption on valid start character even if body is invalid
  - Control signal bit layout: CB1 (bit 3), CB2 (bit 2), CA1 (bit 1), CA2 (bit 0)
  - Public types exposed for SPI consumers

**Tests:**
- Binary encoding/decoding: port A, port B state changes round-trip
- Binary encoding/decoding: control signal set/clear round-trip
- ASCII encoding/decoding: port state changes with upper/lower case
- ASCII encoding/decoding: control signal changes (CA10, CB21, etc.)
- Format negotiation: 0xFF selects binary, 0x20 selects ASCII
- ASCII whitespace/control character skipping between messages
- Invalid data silently ignored (no errors, no panics)
- Full message consumption: valid start char + invalid body consumes correct byte count
- Edge cases: empty stream, partial messages, interleaved whitespace in ASCII mode

---

### MR 12: VIA Device (Via6522)

Implement the WDC 65C22 VIA — the most complex built-in device.

**Scope:**
- `emulator/device/via.rs` — `Via6522` implementing `IoDevice`
  - 16 registers (offsets 0x0–0xF)
  - Timer 1 (free-run and one-shot modes, IRQ on underflow)
  - Timer 2 (one-shot and pulse-counting modes)
  - Shift register (8 modes)
  - Port A and Port B with data direction registers
  - CA1/CA2/CB1/CB2 handshaking
  - IRQ flag register (IFR) and enable register (IER)
  - `tick()` — decrement timers by cycle count, trigger IRQ on underflow
  - Timer 1 PB7 toggle: sends port B state change messages, enabled by default, configurable to suppress
  - Shift register: per-transition control signal messages for CB1 (clock) and CB2 (data)
- Protocol integration:
  - Uses `ViaProtocolEncoder`/`ViaProtocolDecoder` from MR 11 for transport communication
  - Connection handshake: peripheral sends format selector, VIA responds with full state dump (port A, port B, CA1, CA2, CB1, CB2 — 6 messages)
  - Outgoing: port/control state changes encoded and sent via `Transport`
  - Incoming: bytes from `Transport` decoded into protocol messages, applied to input pin state
  - VIA handles protocol framing internally; `Transport` trait unchanged

**Tests:**
- Timer 1 one-shot: load latch, tick until underflow, verify IRQ and timer value
- Timer 1 free-run: verify reload from latch after underflow
- Timer 2 one-shot: similar to timer 1
- Timer 1 PB7 toggle: verify port B state message sent on underflow, verify suppression config
- Port read/write with DDR masking
- Port output → protocol message sent on transport
- Incoming protocol message → input pin state updated, IRQ triggered per CA1/CA2/CB1/CB2 config
- Connection handshake: format selection, full state dump on connect
- IFR/IER: interrupt flag set on event, cleared by writing to IFR, masked by IER
- `irq_active()` reflects IFR & IER state
- Shift register: CB1/CB2 transitions generate control signal messages
- peek does not affect timer state, clear interrupt flags, or generate protocol messages

---

### MR 13: Execution Model — Free-Running Mode

Implement the async execution model that moves the CPU into a dedicated thread for full-speed or throttled execution.

**Scope:**
- `emulator/exec/mod.rs` — `RunHandle`, `run()` function
  - Move CPU into dedicated OS thread (not `tokio::spawn`)
  - Hot loop: `cpu.step()`, check stop channel between instructions
  - `RunHandle::stop()` — send stop signal via `tokio::sync::watch`
  - `RunHandle::wait()` — await `StepResult` via oneshot
  - `RunHandle::take_cpu()` — await CPU return via oneshot
- Clock speed throttling:
  - Batched: sleep once per ~1000 instructions
  - Compare elapsed cycles against wall-clock time, sleep if ahead
  - `ClockSpeed::unlimited()` skips throttling entirely
- Return both `StepResult` and `Cpu` when execution stops (breakpoint, watch trigger, error, or user stop)

**Tests:**
- Start free-run, send stop → CPU returned with consistent state
- Breakpoint hit during free-run → `StepResult::Breakpoint`, CPU returned
- Watch trigger during free-run → `StepResult::WatchTriggered`, CPU returned
- Throttled execution at 1 MHz: verify wall-clock time is approximately correct (tolerance-based)
- Unlimited mode: completes faster than throttled mode
- CPU ownership: cannot access CPU while running (enforced by move semantics — this is a compile-time guarantee, not a runtime test)

---

### MR 14: Integration and Functional Tests

End-to-end verification with real test ROMs and full system configurations.

**Scope:**
- **Klaus Dormann 65C02 test suite**: load the test ROM, run to completion, check for success signature (a specific PC address that indicates all tests passed)
- **Instruction integration tests**: load short test programs, step through, verify register/memory state against expected values
- **Full system tests**: configure a machine with ROM, RAM, Console device, and PipeTransport; run a program that echoes characters; verify transport IO
- **Device integration tests**: configure machines with ACIA/VIA devices, run programs that interact with device registers, verify correct behavior through transport
- **Free-run integration**: start a program in free-run mode, interact via transport, stop execution, verify state
- **Bus trace integration**: run a program with tracing enabled, verify trace output contains expected bus operations

---

## Dependency Order

```
MR 1  Foundation types, opcode decode
  ↓
MR 2  Bus and memory ──────────────────────┐
  ↓                                         │
MR 3  ALU                                  │
  ↓                                         │
MR 4  CPU core ←────────────────────────────┘
  ↓
MR 5  Interrupt controller
  ↓
MR 6  Debugger support (breakpoints, watch, disasm)
  ↓
MR 7  Bus trace
  ↓
MR 8  Transport layer + device events ──┐
  ↓                                      │
MR 9  Console device ←──────────────────┘
  ↓
MR 10 ACIA devices
  ↓
MR 11 VIA peripheral protocol codec
  ↓
MR 12 VIA device ←──────────────────────(uses MR 11 codec)
  ↓
MR 13 Execution model (free-run)
  ↓
MR 14 Integration + functional tests
```

MRs 8–12 (transport + devices) are independent of MRs 6–7 (debugger support, bus trace) and could be developed in parallel if desired. The critical path is: 1 → 2 → 3 → 4 → 5 → 13 → 14.

## Open Questions (to resolve when emma65 source is available)

1. **Watch module API**: What types does the `watch` module export? MR 6 needs `WatchEvaluator`, `WatchExpression`, and `compile_watch()` — or their equivalents.
2. **Existing module layout**: Where does `emulator` fit in the current `src/` tree? Are there existing shared types or error conventions to follow?
3. **Existing dependencies**: Which crates are already in `Cargo.toml`? Avoids introducing duplicates.
4. **Test conventions**: Does emma65 use any test harness crates, fixture patterns, or test organization conventions?
