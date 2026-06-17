# Configuration wiring: RomModule, RamModule, DeviceRegistry::with_builtins, DeviceSpec accessors, EmulatorSession

## Context

The config module has a complete device instantiation pipeline (`DeviceModule`, `DeviceRegistry`, `InstantiationContext`, four I/O device modules) but nothing that wires `AppConfig` + `DeviceRegistry` into a running emulator. This work completes that wiring, adds memory modules (ROM/RAM) that fit the existing `DeviceModule` pattern, and introduces `EmulatorSession` as the startup output type — usable by both the CLI utility and the future debugger.

---

## 1. `DeviceSpec` accessors (`src/emulator/config/device.rs`)

Add three read-only methods to `DeviceSpec`. Fields remain private.

```rust
pub fn address(&self) -> u16 { self.address }
pub fn type_name(&self) -> &str { &self.type_name }
pub fn attributes(&self) -> &HashMap<String, Value> { &self.attributes }
```

---

## 2. `RomModule` and `RamModule` (`src/emulator/config/rom.rs`, `src/emulator/config/ram.rs`)

These follow the same pattern as the four I/O modules but call `bus_config.rom()` / `bus_config.ram()` / `bus_config.ram_with_data()` instead of `bus_config.device()`. No `DeviceId`, no transport, no error sender.

**Attributes (both):**
- `size: u16` — number of bytes; forms the address range `address..address+size-1`
- `file: Option<PathBuf>` — binary file to load into the region (mandatory for ROM, optional for RAM)

**`RomModule::instantiate`:**
1. Parse attributes into `RomAttributes { size: u16, file: PathBuf }`
2. Read file bytes with `tokio::fs::read(&file).await` → `DeviceModuleError::Config` on failure
3. Call `bus_config.rom(AddressRange::new(address, address + size - 1), data)` → `DeviceModuleError::BusConfig` on error

**`RamModule::instantiate`:**
1. Parse attributes into `RamAttributes { size: u16, file: Option<PathBuf> }`
2. If `file` is `Some`, read it: `bus_config.ram_with_data(range, data)`
3. If `file` is `None`: `bus_config.ram(range)`

Module type names: `"rom"` and `"ram"`.

Re-export both from `src/emulator/config/mod.rs`.

---

## 3. `DeviceRegistry::with_builtins` (`src/emulator/config/registry.rs`)

```rust
pub fn with_builtins() -> Self {
    let mut r = Self::new();
    r.register(ConsoleModule);
    r.register(Via6522Module);
    r.register(Acia6551Module);
    r.register(Mc6850Module);
    r.register(RomModule);
    r.register(RamModule);
    r
}
```

---

## 4. `EmulatorSession` (`src/emulator/session.rs`)

New file in the top-level emulator module. Re-export from `src/emulator/mod.rs`.

```rust
pub struct EmulatorSession {
    pub bus: Bus,
    pub cpu: Cpu,
    pub error_receiver: ErrorReceiver,
}
```

Fields `pub` for now — the session is a plain data carrier at this stage, and the debugger will build on top of it. Methods can be added later.

---

## 5. `CpuVariant` in `AppConfig` (`src/emulator/config/app.rs`)

Add `cpu_variant: CpuVariant` as a Clap/Serde field on `AppConfig`. `CpuVariant` must derive `Serialize`, `Deserialize`, and `ValueEnum` (Clap) if not already — check and add derives as needed.

## 6. `AppConfig::build` (`src/emulator/config/app.rs`)

New method on `AppConfig` that drives the full startup sequence. Signature:

```rust
pub async fn build(
    &self,
    registry: &DeviceRegistry,
) -> Result<EmulatorSession, StartupError>
```

**`StartupError`** (new enum in `src/emulator/config/app.rs` or a dedicated `src/emulator/config/error.rs`):
```rust
pub enum StartupError {
    Device { type_name: String, address: u16, source: DeviceModuleError },
}
```

**Sequence:**
1. Call `device_event_channel()` to create `(error_sender, error_receiver)`
2. Build `InstantiationContext { clock_hz: self.clock_speed_hz, error_sender: Some(error_sender) }`
3. Start with `BusConfig::new()`
4. For each `DeviceSpec` in `self.devices.iter().flatten()`:
   - Call `registry.instantiate(spec.type_name(), bus_config, spec.address(), spec.attributes(), &context).await`
   - On error: return `Err(StartupError::Device { type_name: spec.type_name().to_string(), address: spec.address(), source: e })`
   - On success: update `bus_config`
5. Call `bus_config.build()` to get `bus`
6. Call `Cpu::new(self.cpu_variant)` (or equivalent) to get `cpu`
7. Return `Ok(EmulatorSession { bus, cpu, error_receiver })`

---

## 7. `StartupError` display

Implement `std::fmt::Display` for `StartupError` with a human-readable message, e.g.:  
`"failed to configure device 'rom' at address 0xC000: <source>"`

---

## Files to create or modify

- **Create:** `src/emulator/config/rom.rs`
- **Create:** `src/emulator/config/ram.rs`
- **Create:** `src/emulator/session.rs`
- **Modify:** `src/emulator/config/device.rs` — add accessors
- **Modify:** `src/emulator/config/registry.rs` — add `with_builtins()`
- **Modify:** `src/emulator/config/app.rs` — add `cpu_variant: CpuVariant` field, add `build()`, add `StartupError`
- **Modify:** `src/emulator/config/mod.rs` — re-export `RomModule`, `RamModule`, `StartupError`
- **Modify:** `src/emulator/mod.rs` — declare and re-export `session` module, re-export `EmulatorSession`

---

## Verification

```bash
cargo build
cargo test
cargo clippy
```

No new tests strictly required for this PR — the startup path touches async I/O and device instantiation already covered by existing integration tests. Unit tests for `RomModule` and `RamModule` attribute parsing would be valuable additions but can follow in a separate PR.