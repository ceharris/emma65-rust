# Bundle the default configuration as a TOML template (with ROM + labels), shared by the CLI binary and the debugger's default profile

## Context

`src/bin/emulator/config.rs::apply_default_if_unconfigured` currently hardcodes the emulator's built-in fallback device layout (32K RAM, embedded TaliForth ROM, VIA, PTM, two ACIAs, LFSR, console) as Rust string literals in CLI-flag syntax (`"ram@0x0000,size=32768,fill=0"` etc.), used only by the standalone `emma65` binary when launched with zero configured devices. This logic was never shared with the Tauri debugger, which has a parallel but unfixed gap: `debugger/src-tauri/src/profile.rs::ensure_profile_dir` explicitly does *not* seed the `default` profile itself ("nowhere to seed it from"). If `~/.emma/debugger/profiles/default/emulator.toml` doesn't exist, the debugger still starts — Figment tolerates the missing file and every `Config` field is `Option` — but it silently builds a session with an empty bus: no RAM, no ROM, no console. The debugger looks alive but is non-functional on first run, with no error shown.

The fix is to define the default device layout once, as an actual TOML file checked into the source tree (not Rust string literals), bundle a default ROM image and a VICE labels file alongside it, and share one materialization mechanism between both consumers: the debugger's default-profile creation (writing into a persistent profile directory) and the CLI binary's zero-config fallback (writing into an ephemeral tempdir, then loaded through the normal `Toml::file()` config-loading path instead of hand-assembled in Rust).

**Path resolution:** neither `ExpandedPathBuf` nor the config loader resolves relative `image=`/`labels=` paths against the TOML file's own directory today — only `~/` expansion and (otherwise) process-cwd-relative. So the materialized `emulator.toml` must reference the copied-in ROM/labels files by absolute path, *except* that a path falling under `$HOME` should be written with `~/`-shorthand (consistent with how a hand-written config would reference `~/.emma/...` paths, and correctly expanded back on load by `ExpandedPathBuf`). Real directory-relative resolution is explicit future work — out of scope here — but the plan isolates the absolute/`~`-shorthand rendering behind one small function so that future work has an obvious, single seam to extend.

**Blocking prerequisite:** a VICE-format label file for the bundled TaliForth ROM does not exist in the repo yet. It must be supplied as `src/emulator/config/default/program.lbl` before/during implementation — nothing here compiles/passes tests without it landing first. `symbol::load_vice_labels` imposes no extension requirement; `.lbl` is chosen only because it matches that module's own test fixture naming.

## Approach

### 1. New bundled resource directory: `src/emulator/config/default/`

Move `src/bin/emulator/default.bin` → `src/emulator/config/default/program.bin` (unchanged bytes) so it lives in the library crate, reachable by both the `emma65` binary and `emma65-debugger` (which depends on the library, not the binary crate). Add the (separately supplied) `program.lbl` alongside it, plus a new template file.

**`../src/emulator/config/default/emulator-template.toml`** — a plain-text template (never parsed as TOML by this crate; only the *materialized* copy is), with two placeholder tokens substituted via `str::replace` before being written out:

```toml
# Bundled default configuration for emma65. This is a TEMPLATE: {{ROM_IMAGE}}
# and {{LABELS}} are replaced with materialized paths by
# emma65::emulator::config::default::materialize_default_config.

cpu-variant = "WDC65C02"
clock-speed-hz = 1843200

[[devices]]
type = "ram"
address = 0x0000
size = 32768
fill = 0

[[devices]]
type = "rom"
address = 0x8000
size = 32768
image = "{{ROM_IMAGE}}"
labels = "{{LABELS}}"

[[devices]]
type = "via/6522"
address = 0xff80
transport = "unix:~/.emma/sock/via6522"

[[devices]]
type = "ptm/6840"
address = 0xff90
transport = "unix:~/.emma/sock/mc6840"

[[devices]]
type = "acia/6551"
address = 0xfff0
transport = "pty:~/.emma/dev/ttyS0"

[[devices]]
type = "acia/6850"
address = 0xfff4
transport = "pty:~/.emma/dev/ttyS1"

[[devices]]
type = "lfsr"
address = 0xfff6
mode = "step"

[[devices]]
type = "console"
address = 0xfff8
break = 0x3
```

Field names verified against existing module attribute structs (e.g. `ConsoleAttributes::break_key` renamed `"break"` in `src/emulator/config/console.rs:26`; `MemoryAttributes::{image,labels}` in `memory.rs:24-31`; transport shorthand strings accepted per `src/emulator/config/transport.rs`). TOML 1.0 supports `0x`-prefixed hex integers directly (matches `address: u16` and `break_key: Option<u8>` field types) — verify this parses cleanly as the very first implementation step; if not, fall back to decimal literals with identical semantics.

### 2. Shared materialization function

**`src/emulator/config/default/mod.rs`** (new):

```rust
const ROM_IMAGE: &[u8] = include_bytes!("program.bin");
const LABELS: &[u8] = include_bytes!("program.lbl");
const TEMPLATE: &str = include_str!("emulator-template.toml");

/// Writes the bundled default ROM image, VICE labels file, and a rendered
/// `emulator.toml` into `dest` (created if missing). Returns the path to
/// the written `emulator.toml`. Used both for the debugger's persistent
/// `default` profile directory and a CLI-launch tempdir — one source of
/// truth for the bundled device layout.
pub fn materialize_default_config(dest: &Path) -> Result<PathBuf, MaterializeError> { ... }
```

Steps: `create_dir_all(dest)`; write `dest/program.bin`; write `dest/program.lbl`; render `TEMPLATE` via two `.replace()` calls using a small path-rendering helper; write rendered TOML to `dest/emulator.toml`; return that path. Simple `MaterializeError { path, source: io::Error }` wrapping each fallible write with the path that failed.

**Path-rendering seam — add to `src/emulator/config/path.rs`** (co-located with `ExpandedPathBuf`, whose `~/` expansion this is the write-side counterpart of):

```rust
/// Renders an absolute `path` for a materialized config value: `~/`-shorthand
/// if under `$HOME` (which `ExpandedPathBuf` expands back on load), otherwise
/// the absolute path unchanged. The seam for future directory-relative
/// resolution: nothing in the config-loading path (this type, `loader::load_image`,
/// `symbol::load_vice_labels`) resolves a relative path against the TOML file's
/// own directory yet, so a materialized reference can't be relative even when
/// the resource is a sibling of the config file — when that lands, this is the
/// one place that needs to change.
pub(crate) fn portable_path(path: &Path) -> String { ... }
```

Register the new module in `src/emulator/config/mod.rs` as `pub mod default;` (same visibility precedent as the existing `pub mod loader;` — reachable as `emma65::emulator::config::default::materialize_default_config` without a top-level re-export).

### 3. Debugger integration — `debugger/src-tauri/src/profile.rs`

In `ensure_profile_dir`, when creating the `default` profile for the first time, call the new materializer instead of leaving the directory empty:

```rust
pub fn ensure_profile_dir(name: &str) -> Result<PathBuf, String> {
    let dir = profile_dir(name)?;
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(...)?;
        if name == "default" {
            emma65::emulator::config::default::materialize_default_config(&dir)
                .map_err(|e| format!("Failed to seed default profile: {e}"))?;
        } else {
            copy_missing_files_from_default(&dir)?;
        }
    }
    Ok(dir)
}
```

Update the doc comment (currently: "the default profile itself is never auto-seeded, since there's nowhere to seed it from") to reflect that it's now seeded from the bundled default.

**Composition with named-profile seeding:** `copy_missing_files_from_default` already copies every *file* (not just `emulator.toml`) found directly under `profiles/default/` into a newly created named profile, skipping existing ones — no change needed there. Once `profiles/default/` contains `emulator.toml`, `program.bin`, and `program.lbl`, creating a new named profile via `ensure_profile_dir("custom")` copies all three; `custom/emulator.toml`'s `image=`/`labels=` paths still correctly point at `profiles/default/program.bin`/`program.lbl` (a named profile legitimately referencing shared default-profile resources, same as a hand-authored config could).

**Test update required:** `ensure_profile_dir_creates_default_without_seeding` currently asserts the opposite of the new behavior (`!dir.join("emulator.toml").exists()`). Rewrite to assert the default profile *is* seeded (`emulator.toml`, `program.bin`, `program.lbl` all present). Add a composition test creating `default` then a named profile and asserting the named profile received copies of all three files.

### 4. CLI binary integration

**`src/bin/emulator/config.rs`** — replace `apply_default_if_unconfigured`'s body: when devices are unconfigured, materialize into a fresh `tempfile::TempDir`, load its `emulator.toml` through the same `Figment`/`Toml::file()` path used for a user-supplied `--config`, and merge the result into `config.emulator` — preserving any already-set `cpu_variant_spec`/`clock_speed_hz` (from CLI/env/`--config`) and taking `devices` unconditionally from the default:

```rust
pub fn apply_default_if_unconfigured(config: &mut AppConfig) -> Option<tempfile::TempDir> {
    if config.emulator.devices.as_ref().is_none_or(|d| d.is_empty()) {
        let dir = tempfile::tempdir().expect("failed to create tempdir for default config");
        let toml_path = emma65::emulator::config::default::materialize_default_config(dir.path())
            .expect("failed to materialize default config");
        let default: emma65::emulator::Config = Figment::new()
            .merge(Toml::file(&toml_path))
            .extract()
            .expect("bundled default config failed to parse");
        config.emulator.cpu_variant_spec = config.emulator.cpu_variant_spec.take().or(default.cpu_variant_spec);
        config.emulator.clock_speed_hz = config.emulator.clock_speed_hz.take().or(default.clock_speed_hz);
        config.emulator.devices = default.devices;
        Some(dir)
    } else {
        None
    }
}
```

Note the signature drops the `default_rom: &[u8]` parameter (the ROM now comes from the library's embedded bundle, not a caller-supplied slice) and returns `tempfile::TempDir` instead of `NamedTempFile` (must stay alive until `Config::build()` completes, same lifetime pattern as today). Remove the now-unused `DEFAULT_CLOCK_SPEED`/`DEFAULT_CPU_VARIANT` consts — the template is now the single source of truth.

**`src/bin/emulator/main.rs`**: delete `const DEFAULT_ROM: &[u8] = include_bytes!("default.bin");`; update the call site to `let _default_config_dir = apply_default_if_unconfigured(&mut config);`. Delete `src/bin/emulator/default.bin` (moved to the library as `program.bin`).

No `Cargo.toml` changes: `tempfile::TempDir` is already available from the existing `[dependencies.tempfile]` entry (used today for `NamedTempFile`); the debugger crate needs no `tempfile` dependency since it materializes into a persistent directory via plain `std::fs`.

### 5. Migration check

Repo-wide `default.bin` references are exactly three, all handled above: `src/bin/emulator/main.rs` (`include_bytes!`, deleted), `CLAUDE.md` (doc reference, updated below), `plan/emulator-default-config.md` (superseded, updated below). No `build.rs`, `tauri.conf.json` resources, or `.gitignore` entries reference it.

### 6. Documentation updates

- `CLAUDE.md`: drop "embeds default.bin ROM" from the `main.rs` bullet (the binary no longer embeds it directly); update the "binary applies a built-in default..." paragraph to point at the bundled-template mechanism in `src/emulator/config/default/` instead of describing hand-assembled devices; note in the debugger-crate section that the `default` profile directory is now auto-seeded on first run rather than left empty.
- `plan/emulator-default-config.md`: this is the original design doc for the CLI-only, hand-assembled version of this feature and is now stale (describes `NamedTempFile` + literal `DeviceSpec` strings). Update it in place to describe the new bundled-template/shared-materialization design, so it doesn't mislead a future reader.

## Verification

```bash
cargo build --workspace                 # confirms program.bin/program.lbl embed cleanly, both crates compile
cargo test --workspace                  # full suite, including new/updated tests below
cargo clippy                             # no new warnings, covers debugger crate too
```

- New unit tests in `src/emulator/config/default/mod.rs`: materialization writes all three files with correct bytes/content and no leftover `{{...}}` tokens; the rendered `emulator.toml` round-trips through `Figment`/`Config` extraction with the expected 8 devices; `~/`-shorthand vs. absolute-path rendering under/outside `$HOME` (mutate `HOME` under a test lock, following the existing `HOME_ENV_LOCK` pattern from the debugger crate — introduce an equivalent small lock in the library crate's test module for these filesystem-touching tests).
- `debugger/src-tauri/src/profile.rs`: rewrite `ensure_profile_dir_creates_default_without_seeding` (premise now inverted) and add a composition test for named-profile seeding from a freshly-materialized default profile, per §3.
- `tests/emulator_binary.rs`: add a zero-args launch test confirming the bundled default now produces a running (not immediately erroring) process — this repo's integration tests currently have no such case; TaliForth runs indefinitely so the test needs a short spawn/sleep/assert-still-running/kill pattern (no existing helper for this in the file — add one).
- Manual check for the debugger: delete (or rename aside) `~/.emma/debugger/profiles/default/` if it exists, launch `emma65-debugger`, confirm the default profile now boots with a working TaliForth prompt in the terminal window instead of a silently dead session — the user should perform this manual UAT per this project's usual practice for debugger UI behavior.
