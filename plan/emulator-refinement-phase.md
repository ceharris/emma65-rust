# WDC65C02 Emulator — Refinement Phase Transcript

## Via6522 Detailed Design

**User**: Can you provide some details on your planned design and implementation of the Via6522?

**Assistant**: Provided detailed breakdown covering:

### Register Map (16 registers, offsets 0x0–0xF)

| Offset | Register | Description |
|--------|----------|-------------|
| 0x0 | ORB/IRB | Output/Input Register B |
| 0x1 | ORA/IRA | Output/Input Register A (with handshake) |
| 0x2 | DDRB | Data Direction Register B |
| 0x3 | DDRA | Data Direction Register A |
| 0x4 | T1C-L | Timer 1 Counter Low (read: counter, write: latch) |
| 0x5 | T1C-H | Timer 1 Counter High (write starts timer) |
| 0x6 | T1L-L | Timer 1 Latch Low |
| 0x7 | T1L-H | Timer 1 Latch High (write reloads, no start) |
| 0x8 | T2C-L | Timer 2 Counter Low (read: counter, write: latch) |
| 0x9 | T2C-H | Timer 2 Counter High (write starts timer) |
| 0xA | SR | Shift Register |
| 0xB | ACR | Auxiliary Control Register |
| 0xC | PCR | Peripheral Control Register |
| 0xD | IFR | Interrupt Flag Register |
| 0xE | IER | Interrupt Enable Register |
| 0xF | ORA/IRA | Output/Input Register A (no handshake) |

### Read Side Effects (fire on `read`, NOT on `peek`)

- ORA (0x1): Clears CA1 and CA2 interrupt flags in IFR
- ORB (0x0): Clears CB1 and CB2 interrupt flags in IFR
- T1C-L (0x4): Clears T1 interrupt flag in IFR
- T2C-L (0x8): Clears T2 interrupt flag in IFR
- SR (0xA): Clears shift register interrupt flag in IFR

### Timer Modes (governed by ACR)

**Timer 1** (ACR bits 7–6):
- `00`: One-shot, no PB7 output
- `01`: Continuous, no PB7 output (reloads from latch on underflow)
- `10`: One-shot, PB7 one-shot pulse output
- `11`: Continuous, PB7 square wave output (toggles PB7 on each underflow)

**Timer 2** (ACR bit 5):
- `0`: One-shot, timed interval
- `1`: Pulse counting mode on PB6

### Transport Mapping

Configurable port binding:
```rust
pub enum ViaPortBinding { PortA, PortB, ShiftRegister }
```

### Design Decisions Flagged

1. **Timer granularity**: With elapsed always being 1 (per instruction), timers count down in instruction-steps, not clock cycles.
2. **Handshake modes**: Full CA1/CA2/CB1/CB2 emulation is complex. Could start simplified.
3. **Port pin granularity vs. byte-level transport**: Real VIA ports are bit-addressable; Transport trait is byte-oriented.

---

## Clock Speed Emulation

**User**: I think it would be helpful if the CPU design included the ability to emulate an effective clock speed.

**Assistant**: Proposed adding cycle tracking and clock speed throttling.

### Cycle Tracking

Each instruction has a known cycle count (2–7 typically). `DecodedOp` gains a `base_cycles` field. The CPU tracks a cumulative `cycles: u64` counter, computing actual cycle count during execution (base + page-crossing penalty).

### ClockSpeed Configuration

```rust
pub struct ClockSpeed { hz: u64 }
impl ClockSpeed {
    pub fn mhz(mhz: f64) -> Self;
    pub fn hz(hz: u64) -> Self;
    pub fn unlimited() -> Self;
}
```

Set at CPU construction time. Any frequency is supported — 1 MHz, 2 MHz, or arbitrary values. Stored as Hz internally.

**User**: A common clock speed in the real hardware is 1.8432 MHz — because it allows the timer of the 6522 to count hundredths of a second. Is that clock speed supportable?

**Assistant**: Yes. Since `ClockSpeed` stores frequency as `u64` in Hz, `ClockSpeed::mhz(1.8432)` converts to 1,843,200 Hz internally and works exactly as well as round MHz values.

**Decision**: Clock speed is set at construction time (not changeable at runtime).

### Ripple Effect on Device Ticking

`IoDevice::tick` now receives cycle counts instead of instruction counts:
```rust
fn tick(&mut self, cycles: u32);
```

This makes VIA timers realistic — a timer loaded with 18,432 at 1.8432 MHz fires at exactly 100 Hz.

### Throttling in Free-Running Mode

Batched throttling to avoid per-instruction syscall overhead. Run ~1000 instructions per batch, then compare elapsed cycles against wall-clock time and sleep if ahead. With `ClockSpeed::unlimited()`, throttling is skipped entirely. Single-step mode is unthrottled.

---

## Address Space Mapping

**User**: Using real hardware, there are two common approaches for mapping the address space. In the simple case, the space is divided on large boundaries. More sophisticated systems use programmable logic devices to carve up the address space more efficiently — e.g., RAM at 0x0000..0x7FFF, ROM at 0x8000..0xFFFF, with IO devices at 0xFF00..0xFFFF shadowing the ROM. How might the API for the bus be improved to support either approach?

**Assistant**: Proposed three approaches:
1. **Most-specific-wins (automatic resolution)** — smallest region containing the address wins
2. **Explicit layering (last-added wins)** — registration order determines priority
3. **Explicit priority** — numeric priority per region

**Decision**: Most-specific-wins. Matches how real hardware designers think about it. Two identically-sized regions overlapping the same address would be a config error (`AmbiguousOverlap`).

**Follow-up**: For vector addresses (0xFFFA–0xFFFF), the user needs to ensure they resolve to the right region. In the PLD-style layout with IO at 0xFF00–0xFF0F, vectors still fall through to ROM — correct behavior. If someone mapped a large IO region covering the vectors, that's a user error.

**Decision**: Trust the user's expertise. Document the pitfall but don't enforce it in code.

---

## Watch Expression Evaluator Architecture

**User**: Watch expressions are parsed and compiled into opaque bytecode. The evaluator uses a stack and supports variable bindings that persist across evaluations. Should the evaluator be trait-based? Uncertain about ownership and concurrency implications.

**Assistant**: Recommended a concrete struct over a trait:

### Why the closure approach breaks down

The original plan's closure-based WatchExpression can't handle shared mutable variable state across expressions or persistent state across evaluations.

### Why a trait doesn't add much

Only one evaluator implementation exists — the bytecode format and evaluator are tightly coupled. A trait adds vtable indirection without benefit. Testability doesn't require a trait.

### Recommended: concrete struct owned by CPU

```rust
pub struct WatchExpression {
    source: String,
    bytecode: Vec<u8>,   // pub(crate), opaque to API consumers
}

pub fn compile_watch(source: &str) -> Result<WatchExpression, CompileError>;

pub struct WatchEvaluator {
    expressions: Vec<WatchExpression>,
    variables: HashMap<u32, u32>,   // persist across evaluations
    stack: Vec<u32>,                // reused each evaluation
}

impl WatchEvaluator {
    pub fn new() -> Self;
    pub fn add(&mut self, expr: WatchExpression) -> usize;
    pub fn remove(&mut self, index: usize);
    pub fn clear(&mut self);
    pub fn expressions(&self) -> &[WatchExpression];
    pub fn evaluate_all(&mut self, ctx: &dyn WatchContext) -> Result<Option<usize>, (usize, WatchError)>;
    pub fn variables(&self) -> &HashMap<u32, u32>;
    pub fn set_variable(&mut self, id: u32, value: u32);
}
```

### Ownership and concurrency

- CPU owns WatchEvaluator directly. No Arc, no Mutex.
- When CPU moves to free-run thread, evaluator moves with it. Compiler statically prevents debugger access while running.
- When CPU is returned via `take_cpu()`, debugger regains access through `cpu.evaluator_mut()`.
- The concurrency constraint is enforced by the type system, not runtime checks.

CPU API:
```rust
impl Cpu {
    pub fn evaluator(&self) -> &WatchEvaluator;
    pub fn evaluator_mut(&mut self) -> &mut WatchEvaluator;
}
```

Compilation is decoupled — `compile_watch(source)` can be called any time.

**Decision**: Adopt the concrete struct approach.

---

## Additional IO Devices

### Console Device

**User**: I want a simple console device with three registers mapped into two addresses.

**Device registers:**
- Address 0 (read): Data Input — non-zero if input byte available, zero if none
- Address 0 (write): Data Output — sends a byte to the console
- Address 1 (read): Data Latch — reads and latches an input byte; subsequent read of Data Input returns latched value and resets latch to zero
- Address 1 (write): Data Latch — write zero to clear, non-zero to simulate input

**Transport**: Two pipes (one per direction), no protocol overhead.

**UI**: VT-220 terminal in Tauri, shuttling bytes between terminal and pipes.

**Assistant evaluation**: Clean design. Pipes are the right transport — simpler than PTY since terminal emulation lives in Tauri, not the OS. New `PipeTransport` implementation fits the existing `Transport` trait naturally.

Data path: `6502 program ↔ ConsoleDevice ↔ PipeTransport ↔ pipes ↔ Tauri UI ↔ VT-220 renderer`

**Assistant**: Should this device support IRQ?

**User**: No. Use cases are poll-based. For interrupt-driven needs, the ACIA serves that role.

### MC6850 ACIA

**User**: I would also like to include the MC6850 ACIA.

**Assistant**: Two addresses, simpler than the 6551:

| Address | Read | Write |
|---------|------|-------|
| 0 | Status Register | Control Register |
| 1 | RX Data | TX Data |

Supports IRQ (receive and transmit). Master reset via control register bits. Same IoDevice pattern as other devices.

---

## External Device Extensibility

**User**: I'd like a mechanism to allow extensibility of the device model to an external implementation at runtime — an application protocol over a socket exposing the bus interface.

**Assistant**: Analyzed the socket-based approach and identified concerns:
1. **Synchronous read/write latency**: Each device register access would block for a socket round-trip (~1-10μs for Unix domain sockets).
2. **Tick overhead**: `tick()` called every instruction would require per-instruction socket communication.
3. **DMA complexity**: External device bus access requires new architecture.

**User**: Maybe this isn't the best approach. Could we make IoDevice and related elements part of a public SPI that external Rust modules could implement, allowing devices defined in an external crate to be added at build time?

**Assistant**: This is the natural Rust pattern. The IoDevice trait is already designed for this. External crates depend on `emu65c02`, implement the trait, and register devices via `BusConfig::add_device`.

### Public SPI types needed by external device crates:
- `IoDevice` trait, `DeviceId`
- `Transport` trait, `TransportError`
- `ErrorSender`, `DeviceEvent`
- `DmaOp`, `DmaResult` (if using DMA)

### Runtime Discovery

**User**: Is there a mechanism for external crates to be discovered at runtime?

**Assistant**: Analyzed options:
1. **Dynamic libraries** — fragile due to Rust's unstable ABI
2. **C FFI boundary** — ABI-stable but loses type safety
3. **WASM plugins** — sandboxed but heavyweight
4. **Scripting** — accessible but slowest
5. **Compile-time** — clean, zero-cost, type-safe

**Decision**: Primary mechanism is compile-time SPI. A C FFI adapter could be added later if runtime loading becomes essential. The target audience for "bring your own device" is technical enough to rebuild.
