# Embedded Default Configuration

## Context

When the emulator binary is launched with no device configuration (no TOML file, no `--device`
flags, no env vars), `Config::build()` succeeds but produces a CPU with an empty bus — every read
returns `0xFF` and the machine spins pointlessly. The debugger has a parallel gap: if
`~/.emma/debugger/profiles/default/emulator.toml` doesn't exist, it still starts but builds a
session with an empty bus too — the debugger looks alive but is non-functional.

The default device layout — 32K RAM, the TaliForth ROM, a VIA, a PTM, two ACIAs, an LFSR, and a
console — is defined once, as a checked-in TOML template plus its bundled ROM image and VICE
labels file, and shared by both consumers via a single materialization function. See
`doc/emulator-default-config-bundling-plan.md` for the implementation plan this design followed.

---

## Bundled resources: `src/emulator/config/default/`

- **`program.bin`** — the TaliForth ROM image (32 768 bytes), mapped at `0x8000`–`0xFFFF`.
- **`program.lbl`** — a VICE-format label file for `program.bin`, loaded into the bus's symbol
  table.
- **`emulator.toml.template`** — the default device layout, as plain text (never parsed as TOML by
  this crate directly). Two placeholder tokens, `{{ROM_IMAGE}}` and `{{LABELS}}`, are substituted
  with materialized file paths before the template is written out as a real `emulator.toml`.

All three are embedded in the library binary via `include_bytes!`/`include_str!`, so both the
`emma65` binary and the `emma65-debugger` crate (which depends on the library, not the binary
crate) can reach them.

## `emulator::config::default::materialize_default_config`

```rust
pub fn materialize_default_config(dest: &Path) -> Result<PathBuf, MaterializeError>
```

Creates `dest` if missing, writes `dest/program.bin` and `dest/program.lbl`, renders the template
(substituting the `image=`/`labels=` paths just written) into `dest/emulator.toml`, and returns
that path.

**Path rendering:** neither `ExpandedPathBuf` nor the config loader resolves relative
`image=`/`labels=` paths against the TOML file's own directory, so the rendered `emulator.toml`
references `program.bin`/`program.lbl` by absolute path — except that a path falling under `$HOME`
is written with `~/`-shorthand instead (matching how a hand-written config would reference
`~/.emma/...` paths, and correctly expanded back on load by `ExpandedPathBuf`). This rendering is
isolated in `path::portable_path`, the one seam that would need to change if directory-relative
resolution is added later.

## Consumers

**`emma65` binary** (`src/bin/emulator/config.rs::apply_default_if_unconfigured`) — if no devices
are configured after CLI/env/`--config` merging, materializes the default into a fresh
`tempfile::TempDir`, loads its `emulator.toml` through the same `Figment`/`Toml::file()` path used
for a user-supplied `--config`, and merges the result into the running `Config` — preserving any
`cpu-variant`/`clock-speed-hz` already set, taking `devices` unconditionally from the default. The
`TempDir` is kept alive (as `_default_config_dir` in `main.rs`) until `Config::build()` completes.

**`emma65-debugger`** (`debugger/src-tauri/src/profile.rs::ensure_profile_dir`) — the first time
the `default` profile directory is created, materializes the bundled default directly into
`~/.emma/debugger/profiles/default/` (a persistent directory, not a tempdir). Any other new named
profile is still seeded by copying files from `default` (`copy_missing_files_from_default`), which
now includes `program.bin`/`program.lbl` along with `emulator.toml`.

---

## Verification

```bash
cargo build --workspace                 # program.bin/program.lbl embed cleanly, both crates compile
cargo test --workspace                  # full suite
cargo clippy                             # no new warnings, covers debugger crate too

# Run with no config — should start using the bundled TaliForth default
target/debug/emma65

# Run with explicit config — default must NOT apply
target/debug/emma65 --cpu-variant WDC65C02 \
    --device ram@0x0000,size=32768,fill=0 \
    --device rom@0x8000,size=32768,image=<path>
```

`tests/emulator_binary.rs::run_with_no_args_uses_bundled_default_and_keeps_running` spawns the
binary with zero arguments and asserts the process is still running after a short delay (TaliForth
idles indefinitely rather than exiting).
