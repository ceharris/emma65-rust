# Emulator Utility Startup Wiring

## Context

The `emma65` standalone emulator binary (`src/bin/emulator/`) is currently a hard-coded test harness. The library already has a complete configuration and startup system (`emulator::config::Config`, `DeviceRegistry`, `EmulatorSession`, `run()`). This plan wires them together into a real CLI utility with layered config (TOML file → env vars → CLI args) and an async event loop.

---

## Task 1 — Fix `Config` Clap attributes in `src/emulator/config/emulator.rs`

`cpu_variant_spec` and `clock_speed_hz` have no `#[clap(long)]` attribute, so Clap currently treats them as positional arguments. Add long flag names to make them optional named options:

```rust
#[clap(long = "cpu-variant")]
pub cpu_variant_spec: Option<CpuVariantSpec>,

#[clap(long = "clock-speed-hz")]
pub clock_speed_hz: Option<u64>,
```

Both are already `Option<T>` so no default value annotation is needed — they're absent when not supplied.

**Verify:** `cargo build` passes.

---

## Task 2 — Add `signal` feature to Tokio

**File:** `Cargo.toml`

Add `"signal"` to the tokio features list. It's needed for `tokio::signal::ctrl_c()` in Task 4.

```toml
tokio = { version = "1", features = ["net", "io-util", "rt", "rt-multi-thread", "sync", "macros", "fs", "time", "signal"] }
```

**Verify:** `cargo build` still passes.

---

## Task 2 — Build out `AppConfig` in `src/bin/emulator/config.rs`

Two structs are needed: one for Clap parsing (which captures the `--config` path), and one for the merged config.

### `CliArgs` — Clap-only, not serialized

```rust
#[derive(Parser)]
struct CliArgs {
    /// Path to a TOML configuration file.
    #[clap(long = "config")]
    config: Option<std::path::PathBuf>,

    #[clap(flatten)]
    app: AppConfig,
}
```

`CliArgs` exists solely to capture `--config` before Figment runs. Do not derive `Serialize`/`Deserialize` on it.

### `AppConfig` — merged config (Clap + Serde)

Add fields to the existing skeleton:

```rust
#[derive(Debug, Clone, Parser, Serialize, Deserialize)]
#[clap(name = "emma65")]
#[serde(rename_all = "kebab-case")]
pub struct AppConfig {
    /// Embeds all emulator config fields (cpu-variant, clock-speed-hz, device).
    #[clap(flatten)]
    pub emulator: emma65::emulator::Config,
}
```

No `exit_on_stp` flag — the emulator utility always exits on STP. Without a CPU reset mechanism there is no way to resume execution, so staying alive after STP would just leave the process hanging until Ctrl+C.

### `AppConfig::load()` — Figment layering

```rust
use figment::{Figment, providers::{Toml, Env, Serialized}};
use clap::Parser;

impl AppConfig {
    pub fn load() -> Result<Self, figment::Error> {
        let cli = CliArgs::parse();
        let mut figment = Figment::new();
        if let Some(path) = cli.config {
            figment = figment.merge(Toml::file(path));
        }
        figment
            .merge(Env::prefixed("EMMA65_"))
            .merge(Serialized::globals(&cli.app))
            .extract()
    }
}
```

Layer order (lowest → highest priority): TOML file → `EMMA65_`-prefixed env vars → CLI args.

`Serialized::globals` treats the Clap-parsed `AppConfig` as the highest-priority source, so explicit CLI flags always override TOML/env.

Note: `Toml::file()` comes from the `Format` trait — add `Format` to the import: `use figment::providers::{Format, Toml, Env, Serialized};`

**Verify:** `cargo build` passes.

---

## Task 3 — Write startup sequence in `main.rs`

Replace the entire existing hand-rolled test harness. Start with a minimal stub to verify `--help` works before building out the full sequence:

```rust
mod config;
use config::AppConfig;

#[tokio::main]
async fn main() {
    let _config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
}
```

**Verify:** `cargo run --bin emma65 -- --help` shows all emulator config flags plus `--exit-on-stp` and `--config`.

Then expand with the remaining steps:

**3a — Load config.** Already done in the stub above. `figment::Error` implements `Display`.

**3b — Build registry.** `let registry = emma65::emulator::DeviceRegistry::with_builtins();` — infallible.

**3c — Build emulator session.**
```rust
let session = match config.emulator.build(&registry).await {
    Ok(s) => s,
    Err(e) => { eprintln!("startup error: {e}"); std::process::exit(1); }
};
let (cpu, error_receiver) = (session.cpu, session.error_receiver);
```
`BuildError` implements `Display` (`src/emulator/config/emulator.rs`).

**3d — Start the run loop.**
```rust
let run_handle = emma65::emulator::run(cpu);
```
`run()` signature: `pub fn run(cpu: Cpu) -> RunHandle` (`src/emulator/exec/mod.rs`).

**Verify:** The binary compiles and exits cleanly with status 0 when run with no arguments — `Config::build()` with no devices is valid; it produces an empty bus and the CPU runs against it. To see a real startup error, pass an unknown device type: `cargo run --bin emma65 -- --device bogus@0x0000` should print a startup error and exit 1.

---

## Task 4 — Write the async event loop in `main.rs`

`RunHandle::wait(self)` consumes the handle, which makes it awkward to also call `stop(&self)` from a different `select!` branch. Bridge it via a oneshot channel so the handle can be stopped independently:

```rust
use tokio::sync::oneshot;

let (cpu_done_tx, mut cpu_done_rx) = oneshot::channel::<StepResult>();
tokio::spawn(async move {
    let _ = cpu_done_tx.send(run_handle.wait().await);
});
```

Now `cpu_done_rx` can be polled in `select!` and the run thread can be signalled separately if needed (though for now Ctrl+C just exits the process, which is sufficient).

Event loop:

Note: devices without a transport (e.g. RAM) never clone the `ErrorSender`, so it is dropped at the end of `build()`. `error_receiver.recv()` would immediately return `None` and break the loop before the CPU runs. Use a guard flag to disable the branch when the channel closes rather than breaking:

```rust
use emma65::emulator::{DeviceEvent, StepResult};

let mut events_open = true;
loop {
    tokio::select! {
        event = error_receiver.recv(), if events_open => match event {
            Some(DeviceEvent::TransportError { device, error }) =>
                eprintln!("device {}: transport error: {}", device.0, error),
            Some(DeviceEvent::TransportDisconnected { device, reason }) =>
                eprintln!("device {}: disconnected: {}", device.0, reason),
            Some(DeviceEvent::DeviceInfo { device, message }) =>
                eprintln!("device {}: {}", device.0, message),
            Some(DeviceEvent::TransportConnected { .. }) => {}
            None => events_open = false,
        },

        result = &mut cpu_done_rx => {
            match result.unwrap_or(StepResult::Stopped) {
                StepResult::Error(e) => {
                    eprintln!("CPU error: {e}");
                    std::process::exit(1);
                }
                _ => break,
            }
        },

        _ = tokio::signal::ctrl_c() => break,
    }
}
```


**Verify:**
- `cargo build --bin emma65` compiles clean
- `cargo run --bin emma65 -- --help` shows full help text
- Running with `--device ram@0x0000,size=65536` starts without panicking
- Ctrl+C exits cleanly