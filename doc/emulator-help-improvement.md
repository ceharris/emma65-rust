# Device Help: `long_help` on `--device` + `--list-devices` flag

## Context

The `--help` output for `emma65` shows a terse one-liner for `--device` with no syntax guidance.
Users need to know the `type@address[,key=value,...]` syntax and what device types are available
and what attributes each accepts. Two mechanisms will cover this:

1. `long_help` on `--device` (visible with `--help` but not `-h`) — explains the syntax and
   value format, and mentions `--list-devices`.
2. `--list-devices` flag — prints a full structured device catalog from the live registry and exits.

## Design

### 1. Add structured metadata methods to `DeviceModule` trait

In `src/emulator/config/device.rs`, add two new required methods and a supporting type:

```rust
/// Metadata for a single device attribute.
pub struct AttributeInfo {
    pub name: &'static str,
    pub type_name: &'static str,   // e.g. "int", "u8", "path", "bool", "transport"
    pub required: bool,
    pub default: Option<&'static str>,  // None means no default; Some("(random)"), Some("—"), etc.
}
```

```rust
/// Returns a one-line description of this device module.
fn description(&self) -> &'static str;

/// Returns metadata for all configuration attributes accepted by this module.
fn attributes(&self) -> &'static [AttributeInfo];
```

### 2. Implement the new methods on all six concrete modules

Each module file gets a `static ATTRIBUTES: &[AttributeInfo] = &[...]` and implements `description()` + `attributes()`. Summary of content:

| Module | description | attributes |
|---|---|---|
| `RamModule` | "Random-access memory region" | `size` (int, required), `fill` (u8, no, "(random)"), `image` (path, no) |
| `RomModule` | "Read-only memory region" | `size` (int, required), `image` (path, required) |
| `ConsoleModule` | "Serial console device" | `transport` (transport, no) |
| `Acia6551Module` | "MOS 6551 Asynchronous Communications Interface Adapter" | `with_tdre_bug` (bool, required), `with_overrun` (bool, required), `transport` (transport, no) |
| `Mc6850Module` | "Motorola 6850 Asynchronous Communications Interface Adapter" | `transport` (transport, no) |
| `Via6522Module` | "MOS 6522 Versatile Interface Adapter" | `transport` (transport, no) |

### 3. Update `DeviceRegistry` to expose module metadata

In `src/emulator/config/registry.rs`:

- Add `pub struct DeviceInfo { pub name: String, pub description: &'static str, pub attributes: &'static [AttributeInfo] }` (in this file, re-export `AttributeInfo` from `device.rs`).
- Add `device_info: Vec<DeviceInfo>` field to `DeviceRegistry`.
- In `register()`, push `DeviceInfo { name: name.clone(), description: module.description(), attributes: module.attributes() }`.
- Add `pub fn device_info(&self) -> &[DeviceInfo]`.

The `Vec` preserves registration order (matches `with_builtins()` order).

### 4. Add `long_help` to the `--device` field

In `src/emulator/config/emulator.rs`, update the `devices` field annotation:

```rust
#[clap(long = "device", num_args = 1.., long_help = "\
Device configuration specifications. Use --list-devices to see available device types.

Syntax:
  --device type@address[,key=value,...]

  type       Device type name (e.g. ram, rom, acia/6551)
  address    Bus address in hex (e.g. 0x0000)
  key=value  Attribute assignments (comma-separated)

Value formats:
  Decimal:  1024
  Hex:      0xFF
  Octal:    0o77
  Binary:   0b1010
  K suffix: 32K (= 32768)
  Boolean:  true or false

Examples:
  --device ram@0x0000,size=32K
  --device rom@0x8000,size=16K,image=firmware.bin
  --device acia/6551@0xD000,with_tdre_bug=false,with_overrun=false
")]
pub devices: Option<Vec<DeviceSpec>>,
```

### 5. Add `--list-devices` to `CliArgs`, update `AppConfig::load()`

In `src/bin/emulator/config.rs`:

- Add to `CliArgs`:
  ```rust
  /// Print available device types and their configuration attributes, then exit
  #[clap(long = "list-devices")]
  list_devices: bool,
  ```
- Add `pub enum LoadResult { Config(AppConfig), ListDevices }`.
- Change `AppConfig::load()` to `pub fn load() -> Result<LoadResult, Box<figment::Error>>`.
- If `cli.list_devices` is true, return `Ok(LoadResult::ListDevices)` early (before Figment extraction).

### 6. Update `main.rs` to handle `ListDevices`

In `src/bin/emulator/main.rs`, after `AppConfig::load()`:

```rust
let config = match AppConfig::load()? {
    LoadResult::Config(c) => c,
    LoadResult::ListDevices => {
        let registry = DeviceRegistry::with_builtins();
        print_device_catalog(registry.device_info());
        return Ok(());
    }
};
```

Add a `fn print_device_catalog(devices: &[DeviceInfo])` in main.rs (or a helper module) that formats the structured table:

```
ram — Random-access memory region

  ATTRIBUTE  TYPE   REQUIRED  DEFAULT
  size       int    yes       —
  fill       u8     no        (random)
  image      path   no        —

rom — Read-only memory region
  ...
```

## Files to modify

- `src/emulator/config/device.rs` — add `AttributeInfo` struct, `description()` and `attributes()` to trait
- `src/emulator/config/registry.rs` — add `DeviceInfo`, `device_info` field + accessor
- `src/emulator/config/memory.rs` — implement new methods on `RamModule` and `RomModule`
- `src/emulator/config/console.rs` — implement new methods on `ConsoleModule`
- `src/emulator/config/acia6551.rs` — implement new methods on `Acia6551Module`
- `src/emulator/config/mc6850.rs` — implement new methods on `Mc6850Module`
- `src/emulator/config/via6522.rs` — implement new methods on `Via6522Module`
- `src/emulator/config/emulator.rs` — add `long_help` to `--device` field
- `src/bin/emulator/config.rs` — add `--list-devices`, `LoadResult` enum, update `load()`
- `src/bin/emulator/main.rs` — handle `LoadResult::ListDevices`, add `print_device_catalog()`

## Verification

1. `cargo build` — no errors
2. `cargo clippy` — no warnings
3. `./target/debug/emma65 -h` — `--device` shows the short one-liner; no `--list-devices` detail
4. `./target/debug/emma65 --help` — `--device` shows the full syntax block; `--list-devices` appears in options
5. `./target/debug/emma65 --list-devices` — prints structured table catalog and exits cleanly
6. `cargo test` — all existing tests still pass