# Embedded Default Configuration

## Context

When the emulator binary is launched with no device configuration (no TOML file, no `--device` flags, no env vars), `Config::build()` succeeds but produces a CPU with an empty bus — every read returns `0xFF` and the machine spins pointlessly. The goal is to embed a ~32 KB ROM image and a matching default memory layout directly in the binary, so an unconfigured launch does something useful.

---

## Approach

Inject the default after `AppConfig::load()` in `main.rs`, before `config.emulator.build(&registry)`. If `devices` is `None` or an empty vec after all config sources are merged, write the embedded ROM bytes to a `NamedTempFile` and populate `config.emulator.devices` with a default RAM + ROM layout. `Config::build()` then runs as normal against that layout. The tempfile can be dropped after `build()` returns because the bytes are already in memory.

The library (`src/emulator/`) is not touched. All changes are in `src/bin/emulator/`.

---

## Memory layout for the default configuration

| Device  | Address range     | Size   | Notes |
|---------|-------------------|--------|-------|
| RAM     | `0x0000`–`0x7FFF` | 32 KB  | zero-filled |
| ROM     | `0x8000`–`0xFFFF` | 32 KB  | embedded image |
| Console | `0xFFF8`–`0xFFF9` | 2 B    | overlaps ROM; most-specific-wins; PTY symlink at `$HOME/.emma/dev/ttyS0` |

The console's 2-byte region is smaller than the ROM's 32 KB region, so the bus's most-specific-wins rule gives the console priority at those two addresses without an `AmbiguousOverlap` error.

The reset vector in the TaliForth ROM is `0xF000` (confirmed from the binary). CPU variant is set to `Wdc6502` (TaliForth uses `STP`). Clock speed remains whatever was configured (or the existing default: unlimited).

---

## Files to create / modify

### New: `src/bin/emulator/default.bin`

Copy `taliforth-emma65.bin` (32 768 bytes, already in the project root) to `src/bin/emulator/default.bin`. This is the ROM mapped at `0x8000`–`0xFFFF`.

### Modified: `Cargo.toml`

Move `tempfile` from `[dev-dependencies]` to `[dependencies]` (it is already in the build graph as a dev dep; this just makes it available to the binary at runtime).

### Modified: `src/bin/emulator/main.rs`

Add `const DEFAULT_ROM: &[u8] = include_bytes!("default.bin");` at the top.

After `AppConfig::load()` and before `config.emulator.build(&registry)`, insert:

```rust
let _default_rom_file = apply_default_if_unconfigured(&mut config);
```

The returned `Option<NamedTempFile>` is kept alive until after `build()` returns (the tempfile must not be dropped while `build()` is reading it).

Print a notice to stderr when the default is used:
```
eprintln!("notice: no devices configured; using built-in default configuration");
```

### Modified: `src/bin/emulator/config.rs`

Add the helper:

```rust
/// If no devices are configured, writes the embedded default ROM to a tempfile,
/// populates `config.emulator.devices` with the default layout, and returns the
/// tempfile handle (must be kept alive until `Config::build()` completes).
pub fn apply_default_if_unconfigured(config: &mut AppConfig) -> Option<tempfile::NamedTempFile> {
    if config.emulator.devices.as_ref().map_or(true, |d| d.is_empty()) {
        let f = tempfile::Builder::new()
            .suffix(".bin")
            .tempfile()
            .expect("failed to create tempfile for default ROM");
        std::fs::write(f.path(), crate::DEFAULT_ROM)
            .expect("failed to write default ROM to tempfile");
        let rom_path = f.path().to_path_buf();
        config.emulator.cpu_variant_spec.get_or_insert(CpuVariantSpec::Wdc6502);
        let home = std::env::var("HOME").expect("HOME not set");
        let pty_symlink = std::path::Path::new(&home).join(".emma/dev/ttyS0");
        config.emulator.devices = Some(vec![
            "ram@0x0000,size=32768,fill=0".parse().unwrap(),
            format!("rom@0x8000,size=32768,image={}", rom_path.display())
                .parse()
                .unwrap(),
            format!("console@0xfff8,transport=pty:{}", pty_symlink.display())
                .parse()
                .unwrap(),
        ]);
        Some(f)
    } else {
        None
    }
}
```

---

## Verification

```bash
cargo build --bin emma65          # embeds the new default.bin; should compile cleanly
cargo clippy                      # no new warnings

# Run with no config — should start using TaliForth default
target/debug/emma65

# Run with explicit config — default must NOT apply
target/debug/emma65 --cpu-variant WDC65C02 \
    --device ram@0x0000,size=32768,fill=0 \
    --device rom@0x8000,size=32768,image=<path>

cargo test --test emulator_binary   # existing subprocess tests still pass
cargo test                          # full suite
```

Add one new test to `tests/emulator_binary.rs`:

| Test | Setup | Expected |
|---|---|---|
| `run_with_no_config_uses_default` | Invoke binary with zero arguments | exits non-zero (TaliForth runs indefinitely; the test kills after timeout) or check stderr contains "built-in default configuration" notice |
