# For Contributors

Emma65 is written in Rust (2024 edition), as a Cargo workspace. The root
crate (`emma65`) exposes a library plus two binaries, `emma65` and
`emma65-tracer`; three further workspace members — `debugger/src-tauri`
(`emma65-debugger`), `display` (`emma65-display`), and `led-matrix`
(`emma65-led-matrix`) — are thin front ends built on that library. Its
central public module is `emulator`, which implements everything those front
ends are built on: a 65C02 CPU model, a configurable memory bus, a library of
memory-mapped I/O devices, and the pluggable transports that connect those
devices to the outside world. The sections below describe how those pieces
fit together internally, for a contributor adding a new device or working on
the emulator core itself; see [The Emulator Core](the-emulator-core.md) for a
feature-level tour of the same territory.

## CPU

`emulator::cpu::Cpu` is built with `Cpu::builder(variant)`, which takes a
`CpuVariant` (`Cmos65C02` or `Wdc65C02`), a `ClockSpeed`, and a `Bus`, and
owns the `Registers`, the interrupt-priority logic, and the fetch/decode/
execute loop. `cpu::opcodes::decode_table()` builds a fixed `[DecodedOp; 256]`
lookup table once, keyed by opcode byte — decoding an instruction is an array
index, not a match statement, keeping `Cpu::step()` cheap enough to run
unthrottled at full host speed. Every effective-address computation and ALU
operation lives behind that same table entry's `AddressingMode` and
`Mnemonic`, so adding an instruction is a matter of extending the table and
`execute()`'s corresponding match arm — variant gating (which opcodes exist
on CMOS vs. WDC 65C02) is a property of the table itself, checked once at
decode time rather than scattered through execution.

`Cpu::step()` runs a fixed sequence every instruction: service a pending
RESET, then NMI, then a recognized IRQ (in that priority order) if one is
pending; otherwise fetch, decode, and execute one instruction. After the
instruction (or the STP/WAI idle case) it calls `bus.tick_devices(cycles)`
with the exact cycle count that instruction took, then polls
`bus.device_interrupt_states()` to refresh the interrupt controller. This is
the mechanism that keeps every device's timers and interrupt lines
synchronized with CPU time without per-cycle callbacks — see
[Device Interfacing](#device-interfacing) below for the device side of that
same contract. `exec::run()`/`run_from()` drive this loop on a background
thread (throttled to a target `ClockSpeed` by batching cycle-vs-wall-time
comparisons over ~1,000 instructions), exposing a `RunHandle`/`RunStopper`
for control and a `CpuLiveSnapshot` read without pausing the CPU;
`step_into()`, `step_over_subroutine()`, `step_over_breakpoint()`, and
`step_return()` build the debugger's single-step commands on top of the same
`step()` primitive.

## Bus

`emulator::bus::BusConfig` is a builder: `.ram()`, `.rom()`, and `.device()`
each add a named `AddressRange`, and `.build()` resolves all 65,536 possible
addresses to their most-specific owner exactly once — consulting
`IoDevice::claims()` on any overlapping device candidate to settle
conditional chip-select — into a flat lookup table. Every subsequent
`Bus::read()`/`write()` the CPU performs is then a single array index into
that table, with no region walk or `claims()` re-check at runtime, so bus
access cost stays effectively constant regardless of how many devices are
configured. This is also the seam that keeps `IoDevice` implementations
decoupled from addressing: a device is only ever handed the absolute bus
address it was invoked with, never told which region it lives in, so the
same device type can be remapped to a different address purely through
configuration. `bus::symbol::load_vice_labels` loads a VICE-format label file
into a `SymbolTable`, shared by the disassembler, the tracer, and the
debugger's address-to-name resolution.

## Device Interfacing

Every device — built-in or custom — implements `IoDevice`
(`emulator::device`), stored in the bus as a boxed trait object behind a
`DeviceId`. Three methods are required: `read`/`write` (always passed the
*absolute* bus address, never a device-relative offset) and `peek`, which
must be side-effect-free since it backs the debugger's Memory panel,
watchpoints, and the disassembler rather than a real CPU access. Everything
else — `tick(cycles)`, `irq_active()`, `take_nmi()`, `reset()`, `patch()`,
`shutdown()`, `claims()` — defaults to a no-op and is opted into only as a
device actually needs it; `tick()` and the two interrupt hooks are the ones
built-in devices rely on most, since they're what `Cpu::step()` calls into
after every instruction (see [CPU](#cpu) above). A device signals NMI by
setting its own internal pending flag on the triggering event and reporting
it once, from `take_nmi()`; it never calls anything on itself to raise an
interrupt directly. Device *construction* is a separate concern from the
`IoDevice` trait: a `DeviceModule` implementation is the piece that turns a
TOML `[[devices]]` entry or `--device` CLI flag into an `IoDevice` instance
and a call to `BusConfig::device()` — see
[Adding a Custom Device Module](#adding-a-custom-device-module) below, which
walks through implementing both halves for a new device type.

## Peripheral Transport

Devices that exchange byte streams with something outside the emulator —
the console, the VIA, the PTM, both ACIAs — hold a `Transport`
(`emulator::transport`), a small byte-stream trait with implementations for
TCP sockets, Unix sockets, PTYs, spawned child processes over a pipe
(`PipeTransport`), and an in-process variant (`InternalPipeTransport`) used
internally to wire a console straight to the host's own stdin/stdout or
terminal window. Every transport's actual I/O runs on its own thread or
async task; a lock-free ring buffer (`ChannelRelay`/`TransportRelay`)
decouples that from the device's synchronous `tick()` call, so a device
drains whatever bytes have arrived — none, one, or a burst — without ever
blocking, and the transport side never blocks waiting for the CPU thread
either. `TransportReporter` surfaces connect/disconnect/error events as
`DeviceEvent`s over a channel obtained from `device_event_channel()`, which
is how the debugger UI shows transport status without polling it. A device
that needs a transport is configured the same way as `EchoDevice` in
[A Device That Uses a Transport](#a-device-that-uses-a-transport) below:
deserialize a `TransportSpec` from the device's attributes and call
`to_transport_with_reporter()` to obtain a connected `Transport` and its
paired relay during `DeviceModule::instantiate()`.

## Configuration

The `emma65` binary (`src/bin/emulator/`) uses the `emulator::config` module
to load configuration from all sources (TOML, environment, CLI), build an
`EmulatorSession`, and run the free loop. The `emulator::config` module is
the integration point for contributors adding new device types. The
`emma65-tracer` binary (`src/bin/tracer/`) and the `emma65-debugger` crate
(`debugger/src-tauri/`) are both thin front ends over the same library.

## Other Public Modules

Beyond `emulator`, the crate exposes three more top-level public modules:

- **`assembler`** — assembles 6502 assembly source into one or more
  `.org`-delimited output segments plus a symbol table, via `assemble(source)`
  (`src/assembler/`; see `plan/assembler-plan.md` for the full design).
  Output segments are ready to load into emulator memory (e.g. via
  `Bus::patch`) or round-trip through the disassembler for verification.
- **`disassembler`** — decodes bus memory into human-readable instruction
  listings via side-effect-free `peek` reads, sharing the same opcode decode
  table and variant logic as the CPU (see [CPU](#cpu) above); its `trace`
  submodule reconstructs a disassembly listing from a previously recorded
  binary execution trace, used by the `emma65-tracer` binary and the
  debugger's Trace window.
- **`watch`** — a self-contained watchpoint expression pipeline: `Scanner` →
  `Vec<Token>` → `Parser` → `Expr` AST → `Compiler` → `Vec<OpCode>` →
  `Evaluator` → `Operand`. The scanner and parser use zero-copy techniques —
  token text slices borrow directly from the source string — so the pipeline
  produces no heap allocations until bytecode emission. `WatchCompiler` and
  `WatchEvaluator` are the primary entry points; `WatchEvaluator` owns
  variable name-to-index mappings and persistent variable storage so that
  watchpoint variables survive across steps.

## Key Dependencies

| Crate               | Purpose                                                             |
|---------------------|-----------------------------------------------------------------------|
| `bitflags`          | Processor status register flag sets                                 |
| `thiserror`         | Structured, typed error enums                                       |
| `rand`              | Random fill for uninitialized RAM                                   |
| `tokio`             | Async runtime backing TCP, Unix socket, and PTY transport tasks     |
| `crossbeam-channel` | Sync/async bridge between device `tick()` calls and transport tasks |
| `libc` / `nix`      | PTY and pipe setup on Unix                                          |
| `serde`             | Serialization framework for configuration structs                   |
| `clap`              | CLI argument parsing                                                |
| `figment`           | Multi-source configuration merging (TOML, env vars, CLI)            |
| `tempfile`          | Temporary file for the embedded default ROM at startup              |

The other three workspace members are thin binary crates. `debugger/src-tauri`
(crate `emma65-debugger`) adds Tauri 2, `tauri-plugin-dialog`/
`tauri-plugin-log`, and (on Linux) `gtk` on the Rust side, plus a
React/TypeScript/Vite frontend in `debugger/frontend` — see
[The Debugger](the-debugger.md). `display` (crate `emma65-display`) and
`led-matrix` (crate `emma65-led-matrix`) each add the `sdl2` crate (requires
SDL2 development headers to build — see
[Running the Display Peripheral](running-the-display-peripheral.md) and
[Running the LED Matrix Peripheral](running-the-led-matrix-peripheral.md)) and
reuse the root crate's compositing code directly rather than duplicating it.

## Adding a Custom Device Module

A custom device is two pieces: an `IoDevice` implementation (the device
itself) and a `DeviceModule` implementation (the glue that lets it be
configured from TOML/CLI). Device modules are registered with
`DeviceRegistry` before `Config::build()` is called; once registered, a
module's `name()` can appear as the `type` field in a TOML `[[devices]]`
entry or in a CLI `--device` shorthand.

**Step 1** — Implement `IoDevice`. `read`/`write`/`peek` are always passed
the *absolute* bus address, not an offset into the device's own registers —
store the device's own base address (typically via a `with_address()`
builder, matching every built-in device) and compute `address - self.address`
yourself if you have more than one register. `peek()` must be side-effect-free
— it backs the debugger's Memory panel, watchpoints, and the disassembler, so
it must never change device state or interrupt lines the way a real access
might. Beyond those three required methods, override whichever optional ones
your device actually needs — `tick()` for cycle-accurate timing,
`irq_active()`/`take_nmi()` for interrupts, `reset()`/`shutdown()` for
lifecycle, `claims()` for conditional chip-select — all described in
[Memory-Mapped I/O Devices](the-emulator-core.md#memory-mapped-io-devices). Note that both
interrupt hooks are polled by the CPU, not called by the device itself:
`irq_active()` is a live query of your own IRQ state, and `take_nmi()` is
called once per step to report and clear a pending edge that your own code
raised internally (e.g. via a `signal_nmi()`-style helper of your own that
sets a pending flag on the triggering write) — not something a device calls
on itself. Everything else defaults to a no-op.

```rust
use emma65::emulator::IoDevice;

struct BlinkerDevice {
    address: u16,
    on: bool,
}

impl BlinkerDevice {
    fn new() -> Self {
        Self { address: 0, on: false }
    }

    fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }
}

impl IoDevice for BlinkerDevice {
    fn read(&mut self, _address: u16) -> u8 {
        self.on as u8
    }

    fn write(&mut self, _address: u16, value: u8) {
        self.on = value != 0;
    }

    fn peek(&self, _address: u16) -> u8 {
        self.on as u8
    }

    fn name(&self) -> &str {
        "myvendor/blinker"
    }
}
```

**Step 2** — Implement `DeviceModule`. The trait requires `name()` and an
async
`instantiate()` that receives a `BusConfig` builder, the mapped address, a
`HashMap<String, figment::value::Value>` of configuration attributes, an
`InstantiationContext` (holds the configured clock speed, an error-event
sender, and — for the console only — a pre-built transport slot), and a
shared `DeviceIdAllocator` for obtaining a `DeviceId` that won't collide with
any other configured device. The implementing struct must also be
`Clone + Send + Sync + 'static`.

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use emma65::emulator::{AddressRange, BusConfig};
use emma65::emulator::bus::DeviceIdAllocator;
use emma65::emulator::config::{DeviceModule, DeviceModuleError, InstantiationContext};

#[derive(Clone)]
struct BlinkerModule;

impl DeviceModule for BlinkerModule {
    fn name(&self) -> &'static str { "myvendor/blinker" }

    async fn instantiate(
        &self,
        bus_config: BusConfig,
        address: u16,
        _attributes: &HashMap<String, figment::value::Value>,
        _context: &InstantiationContext,
        id_allocator: Arc<Mutex<DeviceIdAllocator>>,
    ) -> Result<BusConfig, DeviceModuleError> {
        let device_id = id_allocator.lock().unwrap().next(false);
        bus_config
            .device(
                AddressRange::new(address, address + 1),
                device_id,
                Box::new(BlinkerDevice::new().with_address(address)),
            )
            .map_err(DeviceModuleError::BusConfig)
    }
}
```

**Step 3** — Deserialize attributes from the `HashMap`. Follow the pattern
used by
`RamModule` and `RomModule` in `src/emulator/config/memory.rs`: define a serde
`Deserialize` struct, then extract it with `figment::Figment`:

```rust
use figment::providers::Serialized;
use figment::value::{Dict, Value};

#[derive(serde::Deserialize)]
struct BlinkerAttributes {
    color: String
}

let attrs = Dict::from_iter(attributes.clone());
let config: BlinkerAttributes = figment::Figment::new()
.merge(Serialized::defaults(attrs))
.extract()
.map_err( | e| DeviceModuleError::Config(e.to_string())) ?;
```

**Step 4** — Register the module and build:

```rust
let mut registry = emma65::emulator::DeviceRegistry::with_builtins();
registry.register(BlinkerModule);
let session = config.build( & registry).await?;
```

Once registered, the module is available by name in TOML and CLI
configuration:

```toml
[[devices]]
type = "myvendor/blinker"
address = 0xD000
color = "red"
```

### A Device That Uses a Transport

`BlinkerDevice` never talks to anything outside the emulator, so its module
never touches [Transport Options](io-devices.md#transport-options). A device that does —
follow the pattern used by every built-in transport-attached device (see
`src/emulator/config/mc6850.rs` for the simplest real example): deserialize
an optional `transport` attribute, convert it to a `TransportSpec`, and hand
it to `TransportSpec::to_transport_with_reporter()` to get back a connected
`Transport` and its paired `TransportRelay`. `EchoDevice` below sends
whatever's written to it out over its transport, and buffers whatever the
transport delivers for the next read — draining the relay from `tick()`,
never from `read()`, so an idle transport never blocks CPU execution (see
[Transport Options](io-devices.md#transport-options) for why that's safe to rely on):

```rust
use std::collections::VecDeque;
use emma65::emulator::{IoDevice, Transport, TransportRelay};

struct EchoDevice {
    address: u16,
    transport: Option<Box<dyn Transport>>,
    relay: Option<TransportRelay>,
    rx_buffer: VecDeque<u8>,
}

impl EchoDevice {
    fn new() -> Self {
        Self { address: 0, transport: None, relay: None, rx_buffer: VecDeque::new() }
    }

    fn with_address(mut self, address: u16) -> Self {
        self.address = address;
        self
    }

    fn attach_transport(&mut self, transport: Box<dyn Transport>, relay: TransportRelay) {
        self.transport = Some(transport);
        self.relay = Some(relay);
    }
}

impl IoDevice for EchoDevice {
    fn read(&mut self, _address: u16) -> u8 {
        self.rx_buffer.pop_front().unwrap_or(0)
    }

    fn write(&mut self, _address: u16, value: u8) {
        if let Some(transport) = self.transport.as_mut() {
            transport.send(value);
        }
    }

    fn peek(&self, _address: u16) -> u8 {
        self.rx_buffer.front().copied().unwrap_or(0)
    }

    fn tick(&mut self, _cycles: u32) {
        if let Some(relay) = self.relay.as_mut() {
            let rx_buffer = &mut self.rx_buffer;
            relay.drain_bytes_into(|b| rx_buffer.push_back(b));
        }
    }

    fn name(&self) -> &str {
        "myvendor/echo"
    }
}
```

```rust
use emma65::emulator::config::{TransportSpec, TransportSpecFormat};

#[derive(Clone)]
struct EchoModule;

#[derive(serde::Deserialize)]
struct EchoAttributes {
    transport: Option<TransportSpecFormat>,
}

impl DeviceModule for EchoModule {
    fn name(&self) -> &'static str { "myvendor/echo" }

    async fn instantiate(
        &self,
        bus_config: BusConfig,
        address: u16,
        attributes: &HashMap<String, figment::value::Value>,
        context: &InstantiationContext,
        id_allocator: Arc<Mutex<DeviceIdAllocator>>,
    ) -> Result<BusConfig, DeviceModuleError> {
        let attrs = figment::value::Dict::from_iter(attributes.clone());
        let config: EchoAttributes = figment::Figment::new()
            .merge(figment::providers::Serialized::defaults(attrs))
            .extract()
            .map_err(|e| DeviceModuleError::Config(e.to_string()))?;

        let transport_spec = config.transport
            .map(TransportSpec::try_from)
            .transpose()
            .map_err(DeviceModuleError::Config)?;

        let device_id = id_allocator.lock().unwrap().next(false);
        let mut device = EchoDevice::new().with_address(address);
        if let Some(transport_spec) = transport_spec {
            let (transport, relay) = transport_spec
                .to_transport_with_reporter(context.pipe_exit_reporter(device_id))
                .await
                .map_err(DeviceModuleError::Transport)?;
            device.attach_transport(transport, relay);
        }

        bus_config
            .device(AddressRange::new(address, address + 1), device_id, Box::new(device))
            .map_err(DeviceModuleError::BusConfig)
    }
}
```

```toml
[[devices]]
type = "myvendor/echo"
address = 0xD100
transport = { pty = { path = "~/.emma/dev/ttyEcho" } }
```
