# For Contributors

Emma65 is written in Rust (2024 edition), as a Cargo workspace. Key
dependencies of the root `emma65` crate:

| Crate               | Purpose                                                             |
|---------------------|---------------------------------------------------------------------|
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

The root crate exposes a library (`emma65`) and two binaries, `emma65` and
`emma65-tracer`. The library has two top-level public modules:

- **`emulator`** — the CPU, memory bus, and device infrastructure. Submodules:
  `cpu` (opcode decode table, addressing modes, status register, variant
  selection, bus-access trace recording), `bus` (address regions, bus
  operations, IRQ/NMI controller, device ID allocation, VICE symbol loading),
  `device` (device trait, built-in devices, and the `protocol` submodule for
  VIA/PTM peer-communication framing), `exec` (clock speed, step results, live
  snapshots, free-running and single-step execution), `transport`
  (byte-stream abstraction, implementations, and the relay/reporter types
  that connect devices to them), `disasm` (instruction disassembler, and a
  `trace` submodule that reconstructs disassembly from a recorded trace), and
  `error` (typed errors for every failure category).

- **`watch`** — a self-contained watchpoint expression pipeline: `Scanner` →
  `Vec<Token>` → `Parser` → `Expr` AST → `Compiler` → `Vec<OpCode>` →
  `Evaluator` →
  `Operand`. The scanner and parser use zero-copy techniques — token text
  slices borrow directly from the source string — so the pipeline produces no
  heap allocations until bytecode emission. `WatchCompiler` and
  `WatchEvaluator` are the primary entry points;
  `WatchEvaluator` owns variable name-to-index mappings and persistent
  variable storage so that watchpoint variables survive across steps.

The `emma65` binary (`src/bin/emulator/`) uses the `emulator::config` module
to load configuration from all sources (TOML, environment, CLI), build an
`EmulatorSession`, and run the free loop. The `emulator::config` module is
the integration point for contributors adding new device types. The
`emma65-tracer` binary (`src/bin/tracer/`) and the `emma65-debugger` crate
(`debugger/src-tauri/`) are both thin front ends over the same library.

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
