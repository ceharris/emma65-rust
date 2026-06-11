# WDC65C02 Emulator — Rust Module Architecture

## Context

Design a WDC65C02 microprocessor emulator as a Rust library crate (`emu65c02`). The emulator is not a standalone binary — its public API is consumed by a development and debugging platform. It is not cycle-accurate in terms of per-cycle bus timing, but it tracks instruction cycle counts for clock speed emulation and device timer accuracy. It must support configurable memory maps (ROM, RAM, IO devices), configurable clock speed, single-step and async free-running execution, breakpoints, watch expressions, bus tracing, and interrupt handling matching real hardware semantics.

---

## 1. Crate and Module Structure

```
emu65c02/
  src/
    lib.rs                  -- crate root, re-exports public API
    cpu/
      mod.rs                -- Cpu struct, step(), builder, register access
      alu.rs                -- ADC, SBC, BCD logic, flag updates
      opcodes.rs            -- 256-entry decode table, DecodedOp, Mnemonic enum
      variant.rs            -- CpuVariant enum, opcode validity per variant
      status.rs             -- StatusRegister (bitflags newtype)
    bus/
      mod.rs                -- Bus struct, BusConfig builder, address decode, read/write/peek
      region.rs             -- Region types, AddressRange
      trace.rs              -- BusTraceCallback trait, TraceRecord, BinaryTraceWriter
    device/
      mod.rs                -- IoDevice trait, DeviceId, DeviceEvent, ErrorSender
      via.rs                -- Via6522 (WDC 65C22)
      acia6551.rs            -- Acia6551 (WDC 65C51)
      mc6850.rs             -- Mc6850 (Motorola MC6850 ACIA)
      console.rs            -- Console (simple polling console device)
    transport/
      mod.rs                -- Transport trait, TransportError
      tcp.rs                -- TcpTransport
      unix_socket.rs        -- UnixSocketTransport
      pty.rs                -- PtyTransport
      pipe.rs               -- PipeTransport (bidirectional pipe pair)
    interrupt.rs            -- InterruptController, IrqSource
    disasm/
      mod.rs                -- Disassembler, DisassembledLine
    watch/
      mod.rs                -- WatchContext trait, WatchEvaluator, WatchCompiler, Watchpoint, WatchError
    exec/
      mod.rs                -- StepResult, RunHandle, async run()
    error.rs                -- ExecError, BusError, BusConfigError, CpuBuildError
```

### Dependency direction (strictly acyclic)

```
exec --> cpu --> bus --> device --> transport
                   \--> interrupt
disasm --> bus, cpu::variant
watch  --> bus, cpu (registers)
```

---

## 2. Core Types

### CPU Variant and Policies

```rust
pub enum CpuVariant { Cmos65C02, Wdc65C02 }

pub enum InvalidOpcodePolicy { Nop, Error }
```

`Wdc65C02` adds 34 opcodes (STP, WAI, BBR0-7, BBS0-7, RMB0-7, SMB0-7). In `Cmos65C02` mode, those opcodes are either multi-byte NOPs (with correct PC advancement) or errors, per policy.

### Registers and Status Flags

```rust
bitflags! {
    pub struct StatusRegister: u8 {
        const C = 0x01; const Z = 0x02; const I = 0x04; const D = 0x08;
        const B = 0x10; const UNUSED = 0x20; const V = 0x40; const N = 0x80;
    }
}

pub struct Registers {
    pub a: u8, pub x: u8, pub y: u8, pub sp: u8, pub pc: u16, pub p: StatusRegister,
}
```

### Decoded Instruction

```rust
pub enum AddressingMode {
    Implied, Accumulator, Immediate, ZeroPage, ZeroPageX, ZeroPageY,
    Absolute, AbsoluteX, AbsoluteY, Indirect, IndirectX, IndirectY,
    ZeroPageIndirect, AbsoluteIndirectX, Relative, ZeroPageRelative,
}

pub struct DecodedOp {
    pub opcode: u8,
    pub mnemonic: Mnemonic,
    pub mode: AddressingMode,
    pub byte_len: u8,       // 1, 2, or 3
    pub base_cycles: u8,    // base cycle count (before page-crossing penalties)
    pub is_valid: bool,      // false for NOP-substituted invalid opcodes
}
```

### Clock Speed

```rust
pub struct ClockSpeed { hz: u64 }

impl ClockSpeed {
    pub fn mhz(mhz: f64) -> Self;     // e.g., ClockSpeed::mhz(1.8432) for 1.8432 MHz
    pub fn hz(hz: u64) -> Self;        // e.g., ClockSpeed::hz(1_843_200)
    pub fn unlimited() -> Self;         // no throttling
}
```

Stored as Hz internally, so any frequency is supported — 1 MHz, 1.8432 MHz, 2 MHz, or arbitrary values. Used for throttling in free-running mode and for realistic device timer behavior.

---

## 3. Memory Subsystem (Bus)

### Configuration

```rust
pub struct AddressRange { pub start: u16, pub end: u16 }

pub enum UnmappedPolicy { DefaultValue(u8), Error }
pub enum RomWritePolicy { Ignore, Error }

pub struct BusConfig { /* builder */ }
impl BusConfig {
    pub fn new() -> Self;
    pub fn add_ram(self, range: AddressRange) -> Self;
    pub fn add_rom(self, range: AddressRange, data: &[u8]) -> Self;
    pub fn add_device(self, range: AddressRange, id: DeviceId, device: Box<dyn IoDevice>) -> Self;
    pub fn unmapped_policy(self, policy: UnmappedPolicy) -> Self;
    pub fn rom_write_policy(self, policy: RomWritePolicy) -> Self;
}
```

### Address Space Overlap Resolution

Regions may overlap. When an address falls in multiple regions, the **most-specific (smallest) region wins**. This supports common hardware layouts where IO devices shadow a portion of a larger ROM region (e.g., IO at 0xFF00–0xFF0F overlaying ROM at 0x8000–0xFFFF). Regions are added in any order — no ordering dependency. Two regions of identical size overlapping the same address are a configuration error (`BusConfigError::AmbiguousOverlap`).

Note: the user is responsible for ensuring that machine vectors (0xFFFA–0xFFFF) resolve to the correct region. This concern is documented but not enforced in code.

### Bus API — Two Read Paths

```rust
impl Bus {
    // CPU-facing (side-effectful)
    pub fn read(&mut self, addr: u16) -> Result<u8, BusError>;
    pub fn write(&mut self, addr: u16, val: u8) -> Result<(), BusError>;

    // Debugger-facing (side-effect-free)
    pub fn peek(&self, addr: u16) -> Result<u8, BusError>;
    pub fn peek_range(&self, start: u16, len: u16) -> Vec<u8>;

    pub fn load_rom(&mut self, base_addr: u16, data: &[u8]) -> Result<(), BusConfigError>;
    pub fn set_trace_callback(&mut self, cb: Option<Box<dyn BusTraceCallback>>);
}
```

### Bus Trace

```rust
pub enum BusOp { Read, Write }

pub struct TraceRecord {
    pub timestamp_ns: u64,   // nanoseconds since emulation start (monotonic wall clock)
    pub addr: u16,
    pub value: u8,
    pub op: BusOp,
}

pub trait BusTraceCallback: Send {
    fn trace(&mut self, record: &TraceRecord);
}
```

Near-zero cost when no callback is registered — a branch-predictable `is_some()` check. The timestamp is captured once per instruction by the CPU (via `Instant::now().duration_since(epoch).as_nanos()`), and all bus accesses within that instruction share the same value. On macOS this uses `mach_absolute_time()` (~20ns overhead, no true syscall). The timestamp both groups accesses by instruction and provides wall-clock correlation.

A built-in `BinaryTraceWriter` writes 12-byte records (timestamp_ns:8 + addr:2 + value:1 + op:1) to a `BufWriter<File>`.

---

## 4. IO Devices

### IoDevice Trait

```rust
pub struct DeviceId(pub u32);

pub trait IoDevice: Send {
    fn read(&mut self, offset: u16) -> u8;      // side-effectful
    fn write(&mut self, offset: u16, value: u8);
    fn peek(&self, offset: u16) -> u8;           // side-effect-free
    fn tick(&mut self, cycles: u32);             // advance internal state by CPU cycles elapsed
    fn irq_active(&self) -> bool;
    fn name(&self) -> &str;
}
```

Addresses passed as offsets from device base (bus does the translation). Devices are relocatable and testable without a full bus. The `cycles` parameter to `tick` is the actual cycle count of the instruction just executed, enabling realistic timer behavior (e.g., a VIA timer loaded with 18,432 at 1.8432 MHz fires at 100 Hz).

### Built-in Devices

All built-in devices delegate external IO to an optional `Transport` and report errors through an optional `ErrorSender`. All follow the same construction pattern: `new()`, `attach_transport()`, `set_error_sender()`.

**Via6522** — WDC 65C22 VIA. 16 registers (offsets 0x0–0xF), two timers, shift register, two 8-bit IO ports with data direction registers, handshaking via CA1/CA2/CB1/CB2, IRQ support. Transport binding is configurable per port:

```rust
pub enum ViaPortBinding { PortA, PortB, ShiftRegister }

impl Via6522 {
    pub fn new() -> Self;
    pub fn attach_transport(&mut self, transport: Box<dyn Transport>, binding: ViaPortBinding);
    pub fn set_error_sender(&mut self, sender: ErrorSender);
}
```

**Acia6551** — WDC 65C51 ACIA. 4 registers (data, status, command, control). TX/RX with IRQ support. Transport carries the serial byte stream.

**Mc6850** — Motorola MC6850 ACIA. 2 addresses (status/control at offset 0, data at offset 1). Simpler than the 6551 — single control register handles clock divider, word format, and interrupt enables. IRQ support for both receive and transmit. Master reset via control register bits.

**Console** — Simple polling console device. 2 addresses, 3 logical registers:
- Offset 0 read (Data Input): returns non-zero if an input byte is available, zero if none
- Offset 0 write (Data Output): sends a byte to the console
- Offset 1 read (Data Latch): reads and latches an input byte if available; subsequent Data Input reads and returns the latched value, resets the latch to zero
- Offset 1 write (Data Latch): write zero to clear latch, non-zero to simulate input

No IRQ — purely poll-based. Designed for use with `PipeTransport` connecting to a VT-220 terminal emulator in the Tauri UI.

---

## 5. Transport Layer

```rust
pub trait Transport: Send {
    fn try_recv(&mut self) -> Option<u8>;
    fn send(&mut self, byte: u8) -> Result<(), TransportError>;
    fn is_connected(&self) -> bool;
    fn shutdown(&mut self);
}
```

Implementations: `TcpTransport`, `UnixSocketTransport`, `PtyTransport`, `PipeTransport`. The socket and PTY transports each spawn a Tokio task that owns the async IO handle. The sync side uses `crossbeam` bounded channels (not `tokio::sync`) because devices run on the CPU thread with no async runtime. `PipeTransport` uses a pair of OS pipes (one per direction) with non-blocking IO — the simplest transport with zero protocol overhead, well-suited for connecting the Console device to the Tauri UI.

### Async Error Reporting

```rust
pub enum DeviceEvent {
    TransportConnected { device: DeviceId },
    TransportDisconnected { device: DeviceId, reason: String },
    TransportError { device: DeviceId, error: TransportError },
    DeviceInfo { device: DeviceId, message: String },
}

pub type ErrorSender = tokio::sync::mpsc::UnboundedSender<DeviceEvent>;
pub type ErrorReceiver = tokio::sync::mpsc::UnboundedReceiver<DeviceEvent>;

pub fn device_event_channel() -> (ErrorSender, ErrorReceiver);
```

The UI holds the receiver and polls independently of CPU execution.

---

## 6. Interrupt System

```rust
pub struct IrqSource(pub u32);

pub struct InterruptController {
    irq_sources: HashSet<IrqSource>,  // currently asserted
    nmi_pending: bool,                 // edge detected, not yet consumed
    nmi_line: bool,                    // previous NMI line state
}

impl InterruptController {
    pub fn assert_irq(&mut self, source: IrqSource);
    pub fn release_irq(&mut self, source: IrqSource);
    pub fn irq_active(&self) -> bool;        // true if any source asserted
    pub fn signal_nmi(&mut self);             // edge trigger
    pub fn take_nmi(&mut self) -> bool;       // consume and clear
    pub fn poll_devices(&mut self, devices: &HashMap<DeviceId, Box<dyn IoDevice>>);
}
```

**IRQ**: Level-triggered, multi-source. `poll_devices` is called after each instruction — iterates devices and syncs their `irq_active()` state into the source set. No circular references needed.

**NMI**: Edge-triggered. `signal_nmi()` latches a pending flag. `take_nmi()` atomically reads and clears it. NMI has priority over IRQ.

**WAI**: CPU sets `waiting = true`. Each `step()` ticks devices and polls interrupts. If an interrupt arrives, WAI clears and the interrupt is serviced. Otherwise returns `StepResult::Waiting`.

**STP**: CPU sets `stopped = true`. Only `cpu.reset()` clears it.

---

## 7. Watch Expressions

### Operand Type

```rust
pub type Operand = u32;
```

All watch-pipeline values — register IDs, flag IDs, memory contents, variable storage, and expression results — use the `Operand` type alias. This provides uniformity across the evaluator and makes the pipeline's value width explicit.

### WatchContext Trait

```rust
pub trait WatchContext {
    fn read_register_u32(&self, register_id: Operand) -> Operand;   // zero-extended
    fn read_register_i32(&self, register_id: Operand) -> Operand;   // sign-extended
    fn read_flag(&self, flag_id: Operand) -> Operand;                // 0 or 1
    fn read_mem_u32(&self, addr: u16, width: u8) -> Operand;        // zero-extended
    fn read_mem_i32(&self, addr: u16, width: u8) -> Operand;        // sign-extended
}
```

Implemented internally by a struct borrowing `&Bus` (for peek) and `&Registers`. All methods return `Operand`; the bytecode evaluator handles signed/unsigned interpretation.

### Watchpoint and Compilation

Expressions are parsed and compiled into opaque bytecode stored as `Watchpoint` values. Compilation is stateful — the `WatchCompiler` holds parser state and delegates variable allocation to the `WatchEvaluator`.

```rust
pub struct Watchpoint {
    source: String,
    code: Vec<OpCode>,       // pub(crate), opaque to API consumers
}

impl Watchpoint {
    pub fn source(&self) -> &str;
}

pub struct WatchCompiler { /* parser state */ }

impl WatchCompiler {
    pub fn new(
        map_register: impl Fn(&str) -> Option<Operand> + 'static,
        map_flag: impl Fn(&str) -> Option<Operand> + 'static,
        map_symbol: impl Fn(&str) -> Option<Operand> + 'static,
    ) -> Self;

    pub fn compile(&mut self, source: &str, evaluator: &mut WatchEvaluator) -> Result<Watchpoint, Error>;
    pub fn compile_all(&mut self, source: &str, evaluator: &mut WatchEvaluator) -> (Vec<Watchpoint>, Vec<Error>);
}
```

### WatchEvaluator

The evaluator is a concrete struct (not a trait) owned by the CPU. It owns the watchpoint collection, variable name→index mappings, and variable runtime storage.

```rust
pub struct WatchEvaluator {
    watchpoints: Vec<Watchpoint>,
    vars: Variables,                // name → index mappings
    var_storage: Vec<Operand>,      // runtime values, indexed by variable ID
}

impl WatchEvaluator {
    pub fn new() -> Self;

    // Watchpoint management
    pub fn add(&mut self, watchpoint: Watchpoint) -> usize;
    pub fn remove(&mut self, index: usize) -> Watchpoint;
    pub fn clear(&mut self);
    pub fn watchpoints(&self) -> &[Watchpoint];

    // Evaluation — called by CPU before each instruction.
    // Returns Ok(None) if no watch triggered, Ok(Some(index)) if one did,
    // or Err with the index and error details.
    pub fn evaluate_all(
        &mut self,
        ctx: &dyn WatchContext,
    ) -> Result<Option<usize>, (usize, WatchError)>;

    // Variable inspection (debugger UI, when stopped)
    pub fn variables(&self) -> &[Operand];
    pub fn set_variable(&mut self, id: usize, value: Operand);
}

pub enum WatchError {
    DivisionByZero, InvalidRegister(Operand), InvalidFlag(Operand),
    InvalidMemoryWidth(u8), StackOverflow,
}
```

The CPU owns the `WatchEvaluator` directly. When the CPU moves into the free-run thread, the evaluator moves with it — Rust's ownership system statically prevents debugger access while running. When the CPU is returned via `take_cpu()`, the debugger regains access through `cpu.evaluator_mut()`. The `WatchCompiler` remains with the debugger UI and is only used when the CPU is stopped (since it requires `&mut WatchEvaluator` to allocate variables).

---

## 8. Execution Model

### StepResult

```rust
pub enum StepResult {
    Executed(DecodedOp),
    Breakpoint(u16),
    WatchTriggered { watch_index: usize, pc: u16 },
    WatchError { watch_index: usize, pc: u16, error: WatchError },
    Waiting,
    Stopped,
    Error(ExecError),
}
```

### Single-Step Order of Operations

1. Check STP → return `Stopped`
2. Check WAI → tick devices, poll interrupts; if none pending return `Waiting`, else clear WAI
3. Check PC against breakpoint set → return `Breakpoint`
4. Evaluate watch expressions → return `WatchTriggered` or `WatchError` on first hit
5. Service pending NMI (higher priority)
6. Service pending IRQ (if I flag clear)
7. Fetch, decode, execute instruction; compute actual cycle count (base + page-crossing penalty)
8. Accumulate cycles into `cpu.cycles`
9. Tick all devices with the instruction's cycle count
10. Poll devices for IRQ state
11. Return `Executed`

### Free-Running Execution

```rust
pub struct RunHandle {
    stop_tx: tokio::sync::watch::Sender<bool>,
    result_rx: tokio::sync::oneshot::Receiver<StepResult>,
    cpu_rx: tokio::sync::oneshot::Receiver<Cpu>,
}

impl RunHandle {
    pub fn stop(&self);
    pub async fn wait(self) -> StepResult;
    pub async fn take_cpu(self) -> Cpu;
}

pub fn run(cpu: Cpu) -> RunHandle;
```

The CPU is **moved** into a dedicated OS thread (not shared via `Arc<Mutex>`). The hot loop calls `cpu.step()` and checks the stop channel between instructions. When execution stops, both the `StepResult` and the `Cpu` are sent back via oneshot channels. No locking in the hot path.

### Clock Speed Throttling (Free-Running Mode)

When `ClockSpeed` is not `unlimited`, the free-running loop throttles to match the target frequency. To avoid per-instruction syscall overhead, throttling is batched:

```
loop {
    for _ in 0..BATCH_SIZE {
        check stop channel;
        match cpu.step() { Executed => continue, other => return }
    }
    // Compare elapsed cycles against wall-clock time
    let expected = wall_elapsed * clock_hz;
    let actual = cpu.cycles() - start_cycles;
    if actual > expected { sleep(delta); }
}
```

A batch size of ~1000 instructions gives sub-millisecond granularity at 1 MHz (~1ms of emulated time per batch) while keeping sleep syscall overhead negligible. With `ClockSpeed::unlimited()`, the throttling check is skipped entirely.

Throttling applies only to free-running mode. Single-step mode is unthrottled — pacing in the debugger's "slow run" mode is controlled by the UI's delay between step calls.

---

## 9. CPU Construction and Public API

### Builder Chain

```rust
let cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
    .clock_speed(ClockSpeed::mhz(1.8432))
    .invalid_opcode_policy(InvalidOpcodePolicy::Nop)
    .bus_config(bus_config)
    .build()?;
```

### CPU Public Methods

```rust
impl Cpu {
    // State inspection
    pub fn registers(&self) -> Registers;
    pub fn registers_mut(&mut self) -> &mut Registers;
    pub fn bus(&self) -> &Bus;
    pub fn bus_mut(&mut self) -> &mut Bus;
    pub fn interrupts(&self) -> &InterruptController;
    pub fn interrupts_mut(&mut self) -> &mut InterruptController;
    pub fn variant(&self) -> CpuVariant;
    pub fn clock_speed(&self) -> &ClockSpeed;
    pub fn cycles(&self) -> u64;
    pub fn is_waiting(&self) -> bool;
    pub fn is_stopped(&self) -> bool;

    // Breakpoints
    pub fn add_breakpoint(&mut self, addr: u16);
    pub fn remove_breakpoint(&mut self, addr: u16) -> bool;
    pub fn clear_breakpoints(&mut self);
    pub fn breakpoints(&self) -> &HashSet<u16>;

    // Watch evaluator
    pub fn evaluator(&self) -> &WatchEvaluator;
    pub fn evaluator_mut(&mut self) -> &mut WatchEvaluator;

    // Execution
    pub fn step(&mut self) -> StepResult;
    pub fn reset(&mut self);
}
```

---

## 10. Async and Threading Architecture

```
┌─────────────────────────────────────────┐
│  Debugger / UI  (owns Tokio runtime)    │
└────────────────┬────────────────────────┘
                 │  async API / RunHandle
┌────────────────▼────────────────────────┐
│  exec module                             │
│  run() spawns OS thread, returns handle  │
│  stop() sends via watch channel          │
│  wait() awaits oneshot                   │
└────────────────┬────────────────────────┘
                 │  std::thread::spawn
┌────────────────▼────────────────────────┐
│  CPU thread (sync hot loop)              │
│  Owns Cpu by move — no locks             │
│  Checks stop channel between instrs      │
└─────────────────────────────────────────┘

┌─────────────────────────────────────────┐
│  Tokio tasks (transport IO)              │
│  One per transport, owns async handles   │
│  Bridges to device via crossbeam channel │
└─────────────────────────────────────────┘
```

---

## 11. Disassembler

```rust
pub struct DisassembledLine {
    pub addr: u16,
    pub raw_bytes: Vec<u8>,
    pub mnemonic: Mnemonic,
    pub operand_text: String,
    pub is_valid: bool,
}

pub struct Disassembler { variant: CpuVariant }

impl Disassembler {
    pub fn new(variant: CpuVariant) -> Self;
    pub fn disassemble_one(&self, bus: &Bus, addr: u16) -> DisassembledLine;
    pub fn disassemble_range(&self, bus: &Bus, start: u16, end: u16, max: usize) -> Vec<DisassembledLine>;
}
```

Uses `Bus::peek` exclusively — no side effects.

---

## 12. Error Types

```rust
// Synchronous (execution-stopping, returned through StepResult)
pub enum ExecError {
    UnmappedAddress { addr: u16, op: BusOp },
    RomWrite { addr: u16, value: u8 },
    InvalidOpcode { addr: u16, opcode: u8 },
}

pub enum BusError { Unmapped { addr: u16 }, RomWrite { addr: u16 } }
pub enum BusConfigError { AmbiguousOverlap { .. }, RomSizeMismatch { .. }, DuplicateDeviceId(DeviceId) }
pub enum CpuBuildError { BusConfig(BusConfigError) }

// Asynchronous (informational, via DeviceEvent channel — does NOT stop CPU)
pub enum DeviceEvent { TransportConnected { .. }, TransportDisconnected { .. }, TransportError { .. }, DeviceInfo { .. } }
```

---

## 13. Ownership and Composition

Strictly hierarchical — no `Arc`, `Mutex`, or `RefCell` in the core emulation path:

```
Cpu
 ├── Bus (owned)
 │    ├── Vec<MappedRegion>
 │    ├── Vec<u8> (ram)
 │    ├── Vec<u8> (rom)
 │    ├── HashMap<DeviceId, Box<dyn IoDevice>>
 │    │    ├── Via6522 → Option<Box<dyn Transport>>
 │    │    ├── Acia6551 → Option<Box<dyn Transport>>
 │    │    ├── Mc6850 → Option<Box<dyn Transport>>
 │    │    └── Console → Option<Box<dyn Transport>>
 │    └── Option<Box<dyn BusTraceCallback>>
 ├── InterruptController (owned)
 ├── WatchEvaluator (owned)
 │    ├── Vec<Watchpoint> (compiled bytecode)
 │    ├── Variables (name → index mappings)
 │    └── Vec<u32> (variable runtime storage)
 ├── ClockSpeed (owned)
 ├── u64 (cycle counter)
 └── HashSet<u16> (breakpoints)
```

The only synchronization boundary is the `RunHandle`↔thread interface (move semantics + Tokio channels). Transport implementations encapsulate their own internal `crossbeam` channels.

---

## 14. VIA Peripheral Protocol

The Via6522 communicates with external virtual peripherals using an application protocol over its transport connection. The full protocol specification is in `emulator-via-protocol.md`. This section summarizes the key design decisions.

### Protocol Overview

The protocol virtualizes the VIA's physical pin interface. Two message types cover the entire external interface:
1. **Port State Change** — communicates the state of all 8 pins on port A or port B
2. **Control Signal Change** — communicates transitions on CA1, CA2, CB1, CB2

The same message types and formats are used bidirectionally (VIA→peripheral and peripheral→VIA).

### Format Negotiation

The protocol supports two wire formats:
- **Binary** — compact 1–2 byte messages, selected when the first received byte has its high bit set
- **ASCII** — human-readable messages using printable characters, selected when the first received byte has its high bit clear

The peripheral selects the format by sending a format selector byte upon connection (0xFF for binary, 0x20 for ASCII). The VIA then responds with a full state dump: port A state, port B state, and all four control signals (CA1, CA2, CB1, CB2) — 6 messages total. This establishes a known baseline for the peripheral.

### Shift Register

Shift register operations are communicated as per-transition control signal messages for CB1 (clock) and CB2 (data). This is faithful to the pin-level behavior and adequate for emulated clock speeds. The protocol's byte space allows a dedicated bulk message type to be added in the future if throughput becomes a concern.

### Timer 1 PB7 Toggle

When Timer 1 is configured to toggle PB7 on underflow, the VIA sends a port B state change message for each toggle. This is enabled by default but configurable — the toggle messaging can be suppressed for configurations where the traffic is unnecessary.

### Protocol Codec

The protocol encoder/decoder is implemented as a standalone module (`via_protocol`), independent of the VIA emulation logic and transport layer. This enables thorough unit testing of message parsing and formatting in isolation.

The protocol codec types are part of the public SPI surface, allowing external peripheral crates to reuse the codec rather than reimplementing the protocol.

### Error Handling

The protocol silently ignores invalid data — no error signaling is defined. Implementations should disconnect the transport on OS-level communication errors.

### Transport Integration

The VIA uses the same byte-oriented `Transport` trait as other devices. Protocol framing is handled internally by the VIA — the transport layer remains a simple byte pipe.

---

## 15. Device Extensibility (SPI)

The `IoDevice` trait and supporting types form a public Service Provider Interface (SPI) that external Rust crates can implement to add new devices at build time. An external device crate depends on `emma65`, imports the SPI types, implements `IoDevice`, and registers the device via `BusConfig::add_device`.

**Public SPI types required by external device crates:**
- `IoDevice` trait, `DeviceId`
- `Transport` trait, `TransportError` (for devices with external IO)
- `ErrorSender`, `DeviceEvent` (for async error reporting)
- `ViaProtocolEncoder`, `ViaProtocolDecoder`, `ViaProtocolMessage` (for peripherals communicating with a VIA)

These types should be treated as a stability boundary — changes are breaking for the ecosystem. A C FFI adapter layer could be added in the future if runtime plugin loading becomes essential, but the primary mechanism is compile-time integration.

---

## 16. Key Design Rationale

- **Move semantics for free-run**: CPU moves into the thread, eliminating all locking from the hot loop. State inspection happens only after stopping.
- **Poll devices for IRQ**: Avoids circular references (device→interrupt controller→bus→device). One iteration per instruction over a small device map is cheap.
- **crossbeam for transport bridging**: Device side is synchronous (CPU thread), needs non-blocking `try_recv` without an async runtime.
- **Offsets in IoDevice**: Devices don't know their base address. Bus translates. Devices are relocatable and unit-testable in isolation.
- **Separate WatchContext trait**: Controls the interface to side-effect-free reads only, provides u32 uniformity, and includes register/flag access that Bus doesn't have.
- **Cycle-based device ticking**: Passing actual instruction cycle counts to `tick` makes device timers realistic without requiring cycle-accurate bus emulation. A VIA timer latch of 18,432 at 1.8432 MHz fires at exactly 100 Hz.
- **Batched throttling**: Sleeping once per ~1000 instructions avoids syscall overhead while maintaining sub-millisecond timing accuracy at typical clock speeds.
- **Most-specific-wins address resolution**: Matches how hardware designers think about decode logic. PLD-style layouts (IO shadowing ROM) just work without explicit priority management.
- **Concrete WatchEvaluator over trait**: Only one evaluator implementation exists; a trait adds vtable indirection without benefit. Ownership by the CPU means concurrency safety is enforced by the type system. The evaluator internalizes variable storage so it moves with the CPU during free-run; the `WatchCompiler` stays with the debugger UI and requires `&mut WatchEvaluator` to allocate variables, which is only possible when the CPU is stopped.
- **Compile-time device SPI**: External device crates integrate at build time via the `IoDevice` trait. Avoids the fragility of Rust's unstable ABI for dynamic plugins, and the target audience can reasonably be expected to rebuild.
- **VIA protocol codec as standalone module**: Separates message encoding/decoding from VIA emulation logic and transport IO, enabling independent unit testing of all three layers. Exposed as a public SPI type so external peripheral crates can reuse it.
- **Per-transition shift register signaling**: Faithful to pin-level behavior. At emulated clock speeds the message rate is negligible. Extensible to bulk messages if a future use case demands it.

---

## Verification Plan

1. **Unit tests per module**: opcode decode tables, ALU (especially BCD), address decoding, interrupt controller state machine, device register behavior
2. **Integration tests**: load a known ROM, step through instructions, verify register/memory state against expected values
3. **Klaus Dormann's 65C02 test suite**: a well-known functional test ROM that exercises all instructions and addressing modes — run to completion and check for success signature
4. **Device tests**: test all built-in devices (Via6522, Acia6551, Mc6850, Console) against the IoDevice trait with mock transports
5. **Free-run tests**: start execution, send stop signal, verify CPU is returned with correct state; verify clock speed throttling produces approximately correct wall-clock timing
6. **Watch evaluator tests**: compile expressions, evaluate against mock WatchContext, verify variable persistence across evaluations, verify error propagation (division by zero, etc.)
7. **Breakpoint tests**: set PC breakpoints, step, verify correct StepResult variants
8. **Bus policy tests**: verify unmapped/ROM-write errors and defaults per configuration; verify most-specific-wins overlap resolution
9. **Address map tests**: verify overlapping regions resolve correctly (IO shadowing ROM), verify AmbiguousOverlap error for same-size conflicts
