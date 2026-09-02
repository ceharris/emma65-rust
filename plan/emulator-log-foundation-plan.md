# Foundational support for structured device/CPU logging to a file

Design discussion outcome, captured for a later implementation pass. This does **not** build the
future debugger log-tailing panel — it adds the library-level primitive needed to make one
buildable: a record type, a bounded/drop-on-full channel, a background writer thread, and a
human-readable-yet-parseable text format, plus converting this crate's existing 11 `debug!` call
sites (the entire scope of `debug!`/`info!`/`warn!`/`error!` usage in `src/` today) to use it.

## Context

Every built-in device's `IoDevice::reset()`, plus `Cpu::reset()`, currently reports its reset
event via `log::debug!` — 11 call sites total: `Cpu::reset()` (`src/emulator/cpu/mod.rs:256`) and
one in each of `console.rs`, `finch.rs`, `led_matrix.rs`, `lfsr.rs`, `mc6840.rs`, `mc6850.rs`,
`phoebe.rs`, `r6551.rs`, `via6522.rs`, `vireo.rs`. These go wherever the active `log` sink sends
them — stdout/stderr for the CLI's `env_logger::init()`, the debugger's own `tauri-plugin-log`
sink for that crate — which is fine for ad-hoc debugging but useless as a durable, structured
record a UI panel could tail and render in columns: no timestamp, no cycle count, no
level/category field, just whatever `Display` produces for the format string.

This crate already has the shape of primitive needed, built for a different purpose:
`src/emulator/cpu/trace.rs`'s `ChannelTraceCallback` + `spawn_trace_writer` — a bounded
`crossbeam-channel`, a background `std::thread` that owns the file and loops `rx.recv()`, and a
drop-on-full policy backed by an `Arc<AtomicU64>` counter so a full channel never blocks the CPU
thread. That trace facility is purpose-built for high-frequency binary bus-op records; this pass
needs the same mechanical skeleton applied to low-frequency, human-readable text records —
different enough in shape (text vs. binary, one record kind vs. four, no seek/replay reader) to
warrant its own small module rather than parameterizing `trace.rs`.

The other gap is distribution: nothing today gives a device a channel to a shared sink, except the
narrower, already-existing `ErrorSender`/`DeviceEvent` mechanism (`src/emulator/device/mod.rs`),
which is unbounded (never drops — a different, deliberately looser tradeoff than what's wanted
here), untyped as free-text (`DeviceInfo { message: String }`, no level/category/timestamp/cycles),
and consumed today only by the CLI's `println!`/`eprintln!` loop in `src/bin/emulator/main.rs`.
Merging the two is out of scope for this pass (see Follow-up) — they solve different problems and
independently evolving them is lower risk than merging now.

Devices also have no way to learn the CPU's live cycle count today: `IoDevice::reset(&mut self)`
takes no arguments, and nothing analogous to `InstantiationContext.error_sender` exists to hand a
device a shared, cheap-to-read counter. Changing `reset()`'s signature to
`reset(&mut self, cycles: u64)` would touch it directly — the trait default, all 10 concrete
overrides, `Bus::reset_devices()`, its one production call site (`Cpu::bus_reset()`), and 14 direct
`.reset()`/`.reset_devices()` call sites in unit tests across 9 device files (grep-verified: real
disruption, not hypothetical) — for a signature change whose only purpose is plumbing one `u64`.
Since every one of those same devices is *also* about to gain a new shared handle (the log sender)
distributed via `InstantiationContext`, exactly like `error_sender` is today, the natural fix is to
bundle a live cycle-count reader into that same handle instead: no `IoDevice` trait change, no new
`InstantiationContext` field beyond the one already needed for logging.

## Design

### 1. New module `src/emulator/logging.rs`: record type, sender, channel, writer thread

Named `logging`, not `log`, to avoid ambiguity with the `log` crate (`use log::debug;`) already
imported by every file this pass touches.

```rust
//! Structured logging: a record type, a sender with a built-in `log`-crate fallback, a bounded
//! channel to a background writer thread, and a human-readable, unambiguously column-parseable
//! text format — the library-level primitive for writing device/CPU diagnostic messages to a
//! file a UI can tail, without ever stalling CPU execution.

/// Severity of a logged message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel { Info, Warn, Error }

impl From<LogLevel> for log::Level {
    fn from(level: LogLevel) -> log::Level {
        match level {
            LogLevel::Info => log::Level::Info,
            LogLevel::Warn => log::Level::Warn,
            LogLevel::Error => log::Level::Error,
        }
    }
}

/// Broad source of a logged message. `Transport` is included now even though this pass populates
/// no call site with it — the user-facing format only ever needs to support three categories
/// (CPU, device, transport), and reserving the variant now avoids a breaking format-consumer
/// change later for whatever UI panel parses this column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCategory { Cpu, Device, Transport }

/// One logged message: when it was written, how many CPU cycles had accumulated at that point,
/// its severity, its broad source, and free text.
#[derive(Debug, Clone)]
pub struct LogRecord {
    pub timestamp: std::time::SystemTime,
    pub cycles: u64,
    pub level: LogLevel,
    pub category: LogCategory,
    pub message: String,
}
```

**Text format** — single-space-separated fields, `message` always last: `RFC 3339 timestamp
(millisecond precision, UTC)` — `cycle count` — `level` — `category` — `message`. None of the
first four fields can ever contain a space, so a plain space is an unambiguous delimiter and a
consumer just does `line.splitn(5, ' ')` and takes the 5th piece verbatim — no tab-width awareness
needed to eyeball it in a terminal or editor, unlike a tab-separated format. A literal `\n`/`\r`
inside `message` is escaped to a two-character `\n`/`\r` sequence before writing, so the
one-record-per-line contract holds regardless of message content. No header line — the column
schema is fixed by the writer, not declared in the file. Example line:

```
2026-08-11T19:42:07.123Z 1048576 INFO DEVICE via@0xc000 reset
```

Timestamp formatting uses the `jiff` crate. `jiff 0.2.34` is already compiled into this workspace
as a transitive dependency of `env_logger 0.11.11` (confirmed via `cargo tree -p emma65 -i jiff`),
and `jiff::Timestamp`'s `Display` impl explicitly honors the standard precision specifier
(`format!("{ts:.3}")` truncates to millisecond precision, per `jiff`'s own documented examples).
Promoting it to a direct dependency (`jiff = "0.2"` in `Cargo.toml`) adds zero new crates to the
build and avoids hand-rolling RFC 3339/calendar-math formatting. `LogRecord.timestamp` stays a
plain `std::time::SystemTime` — the `jiff` dependency is confined to the formatting function's
internals, not the public record type.

**Sender with a built-in fallback, so no call site ever needs an `Option`/`match`:**

```rust
/// Where a `LogSender` delivers records.
#[derive(Clone)]
enum Sink {
    /// Hands records to a background writer thread over a bounded channel; drops and counts a
    /// record if the channel is full rather than ever blocking the caller.
    File { tx: crossbeam_channel::Sender<LogRecord>, dropped: std::sync::Arc<std::sync::atomic::AtomicU64> },
    /// No file sink configured: forwards through the `log` crate instead, at the matching
    /// `log::Level`, so nobody's `RUST_LOG`-based visibility regresses by default.
    LogCrate,
}

/// Producer handle: cloned into `Cpu` and into every device that wants to log. Every `Cpu` and
/// device owns one unconditionally (defaulting to the `log`-crate fallback via `Default`), so
/// call sites never branch on whether a real sink is configured — that's `Sink`'s job.
#[derive(Clone)]
pub struct LogSender {
    sink: Sink,
    cycles: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Default for LogSender {
    /// A sender with no file sink configured; forwards through the `log` crate.
    fn default() -> Self {
        LogSender { sink: Sink::LogCrate, cycles: Default::default() }
    }
}

impl LogSender {
    /// Enqueues (or, with no file sink configured, forwards to the `log` crate) a record built
    /// from `level`/`category`/`message`, stamped with the current time and the live cycle count
    /// (see `set_cycles`). Never blocks the caller.
    pub fn log(&self, level: LogLevel, category: LogCategory, message: impl Into<String>) {
        let message = message.into();
        let cycles = self.cycles.load(std::sync::atomic::Ordering::Relaxed);
        match &self.sink {
            Sink::File { tx, dropped } => {
                let rec = LogRecord { timestamp: std::time::SystemTime::now(), cycles, level, category, message };
                if tx.try_send(rec).is_err() {
                    dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Sink::LogCrate => {
                log::log!(level.into(), "[{:?}] cycles={cycles} {message}", category);
            }
        }
    }

    /// Updates the live cycle count subsequent `log()` calls (from any clone of this sender)
    /// will be stamped with. Called once per elapsed CPU cycle by whichever `Cpu` owns the
    /// authoritative counter; devices never call this themselves.
    pub fn set_cycles(&self, cycles: u64) {
        self.cycles.store(cycles, std::sync::atomic::Ordering::Relaxed);
    }

    /// Number of records dropped so far because the writer thread's channel was full. Always 0
    /// for a `log`-crate-backed sender (nothing is ever dropped on that path).
    pub fn dropped_count(&self) -> u64 {
        match &self.sink {
            Sink::File { dropped, .. } => dropped.load(std::sync::atomic::Ordering::Relaxed),
            Sink::LogCrate => 0,
        }
    }
}

/// Spawns a background thread draining a bounded channel of `capacity`, formatting each record
/// and writing+flushing it to `writer` immediately (messages through this facility are expected
/// to be infrequent, so a per-line flush is not a throughput concern, and lets a UI tailing the
/// file see new lines right away). Returns a producer handle (backed by `Sink::File`) and a join
/// handle; once every `LogSender` clone is dropped, the channel disconnects, the thread drains
/// what's left and exits, and `join()` returns.
pub fn spawn_log_writer<W: std::io::Write + Send + 'static>(
    writer: W,
    capacity: usize,
) -> (LogSender, std::thread::JoinHandle<()>) {
    let (tx, rx) = crossbeam_channel::bounded::<LogRecord>(capacity);
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let handle = std::thread::spawn(move || {
        let mut writer = std::io::BufWriter::new(writer);
        let mut line = String::new();
        while let Ok(rec) = rx.recv() {
            line.clear();
            format_record(&rec, &mut line);
            let _ = writer.write_all(line.as_bytes());
            let _ = writer.flush();
        }
    });
    (LogSender { sink: Sink::File { tx, dropped }, cycles: Default::default() }, handle)
}
```

Always drop-on-full for the `File` sink — no `OverflowPolicy`-style blocking mode, since the user
explicitly asked for "drops log messages rather than slowing CPU step execution" and nothing in
scope needs guaranteed delivery.

**Formatting macro**, so a call site is one line instead of a `format!` plus a `.log()` call —
declared with plain module-scoped visibility and re-exported via `pub(crate) use`, the standard
Rust 2018+ way to make a `macro_rules!` path-addressable without `#[macro_export]`-ing it to the
crate root:

```rust
/// Formats `$fmt, $args...` and logs it through `$sender` at `$level`/`$category` — the same
/// one-line ergonomics `debug!("...", ...)` already had at each of this pass's 11 call sites.
macro_rules! log_msg {
    ($sender:expr, $level:expr, $category:expr, $($arg:tt)*) => {
        $sender.log($level, $category, format!($($arg)*))
    };
}
pub(crate) use log_msg;
```

Re-exported from `src/emulator/mod.rs` as `pub(crate) use logging::log_msg;` so call sites in
`cpu/mod.rs` and the 10 device files can write `crate::emulator::log_msg!(...)`. Kept
`pub(crate)` (not fully public): only this crate's own `IoDevice` implementations use it in this
pass; `LogSender`/`LogLevel`/`LogCategory` stay `pub` since external code can still call `.log()`
directly if needed.

### 2. Cycle-count plumbing: bundled into `LogSender`, not the `IoDevice` trait

As established in Context: a live cycle-count reader lives in `LogSender` (the `Arc<AtomicU64>`
field above), which `Cpu` updates once per elapsed cycle in `finish_cycle()`. `Arc<AtomicU64>` is
used only because `IoDevice: Send` requires a `Send`-safe container — a device's `reset()` always
runs synchronously on whichever single thread currently owns the `Cpu`+`Bus`+devices unit, so
there's no real concurrency to solve.

`Cpu::reset()`'s existing ordering already gives the right semantics for free: `self.bus_reset()`
(which drives every device's `reset()`) runs *before* `self.cycles = 0`, so devices logging mid-
`bus_reset()` see the pre-reset accumulated count — exactly "the number of CPU cycles that had
accumulated when the message was written." The CPU's own reset message logs after the zero, so it
always reads `cycles: 0` (expected/correct).

```rust
// Cpu::finish_cycle() — the sole place `self.cycles` is incremented:
fn finish_cycle(&mut self, cycles: u8) {
    self.cycles += cycles as u64;
    self.log_sender.set_cycles(self.cycles);
    // ... existing tick_devices / poll_devices calls, unchanged ...
}

// Cpu::reset():
pub fn reset(&mut self) -> Result<(), ExecError> {
    self.bus_reset();               // devices' reset() fires here, sees pre-reset cycle count
    // ... vector fetch, register init (unchanged) ...
    self.cycles = 0;
    self.log_sender.set_cycles(0);
    self.waiting = false;
    self.stopped = false;
    crate::emulator::log_msg!(self.log_sender, LogLevel::Info, LogCategory::Cpu, "6502 CPU reset");
    Ok(())
}
```

`Cpu.log_sender` is a plain `LogSender` field (not `Option`), defaulted via `LogSender::default()`
in `CpuBuilder::build()` unless overridden by a new `CpuBuilder::log_sender(sender)` /
`Cpu::set_log_sender(sender)` (mirroring `set_trace_callback`).

### 3. Devices: inherent `set_log_sender`, no trait change, one-line call sites

Each of the 10 device structs gets a `log_sender: LogSender` field (defaulted via
`LogSender::default()` in its constructor — never `Option`) and an inherent setter, the same shape
`Phoebe`/`Finch`/`Vireo` already use for `set_error_sender`:

```rust
/// Installs a log sender for diagnostic messages (e.g. `reset()`).
pub fn set_log_sender(&mut self, sender: LogSender) {
    self.log_sender = sender;
}
```

Each device's `reset()` body converts its `debug!` call 1:1 (`Console` shown; the other 9 devices
get the identical transformation):

```rust
fn reset(&mut self) {
    // ... existing reset logic unchanged ...
    crate::emulator::log_msg!(self.log_sender, LogLevel::Info, LogCategory::Device, "{} @0x{:04x} reset", self.name(), self.address);
}
```

### 4. `InstantiationContext` distributes a real `LogSender`, exactly like `error_sender`

`src/emulator/config/registry.rs` — this field stays `Option<LogSender>`: `None` means "no file
sink configured," in which case device modules simply don't call `set_log_sender`, leaving each
device's own `Default`-constructed (log-crate-backed) sender in place.

```rust
pub struct InstantiationContext {
    pub clock_hz: Option<u64>,
    pub error_sender: Option<ErrorSender>,
    pub console_transport: Option<TransportSlot>,
    /// Shared sender for diagnostic messages (e.g. device `reset()`), cloned into any device
    /// module that calls `set_log_sender`. `None` means no file sink is configured; devices keep
    /// their own default `log`-crate-backed sender in that case.
    pub log_sender: Option<LogSender>,
}
```

Each of the 10 device config modules' `instantiate()` gets one new guard, mirroring the existing
`error_sender` guard already present in `phoebe.rs`/`finch.rs`/`vireo.rs`:

```rust
if let Some(sender) = &context.log_sender {
    dev.set_log_sender(sender.clone());
}
```

**Grep-verified `InstantiationContext { .. }` sites needing the new field** (15 total: 1 struct
definition + 14 construction literals): the struct def and 4 test literals in `registry.rs`, 5
test literals in `console.rs`, 1 in `pic_finch.rs`, 2 in `emulator.rs` (`Config::build` and
`build_with_context`), 1 in `src/bin/emulator/main.rs`, 1 in `debugger/src-tauri/src/lib.rs`.

### 5. CLI wiring: `--log-file`, mirroring `--trace-file`

`src/bin/emulator/config.rs` — new field on `AppConfig`/`CliArgs`, same shape as the existing
`trace_file: Option<ExpandedPathBuf>` (`#[clap(long = "trace-file")]`):

```rust
/// Path to write structured device/CPU log messages to.
#[clap(long = "log-file")]
pub log_file: Option<emma65::emulator::ExpandedPathBuf>,
```

`src/bin/emulator/main.rs`: unlike `--trace-file` (which attaches to the already-built `Cpu` after
`Config::build_with_context` returns), the log file must be opened and `spawn_log_writer` called
**before** `InstantiationContext` is constructed, so `log_sender` can be threaded into device
construction. The returned `LogSender` is cloned onto `InstantiationContext.log_sender` and
attached to the CPU (via `Cpu::set_log_sender`) once `session.cpu` exists. At shutdown, `main()`
joins the writer's handle exactly as it already does for `trace_writer_handle` — once every
`LogSender` clone (held by `Cpu` and any devices) is dropped, the channel disconnects and the
thread's `join()` returns promptly; no new shutdown coordination needed.

### 6. Debugger crate: compile-only update

`debugger/src-tauri/src/lib.rs`'s one `InstantiationContext { ... }` literal gains
`log_sender: None,` — nothing else changes. No log file is opened, no profile-directory wiring, no
new panel (see Follow-up).

## What's deliberately unchanged

- `IoDevice` trait: no new methods, no `reset()` signature change (see Context/§2's rationale).
- `Bus::reset_devices()` / `Cpu::bus_reset()`: unchanged signatures and call order — devices still
  reset before `Cpu` zeroes its own `cycles`, which is exactly what makes "devices see the
  pre-reset accumulated count" fall out for free.
- `DeviceEvent`/`ErrorSender`/`ErrorReceiver` (`src/emulator/device/mod.rs`) and the CLI's
  `println!`/`eprintln!` consumption loop in `main.rs`: entirely untouched. That channel is
  unbounded (never drops — a deliberately looser tradeoff than this facility wants) and
  host-reaction-typed (`TransportConnected`, `RejectedWrite`, etc.), not free-text-diagnostic. This
  pass adds a second, narrower, differently-shaped channel rather than merging into it now; see
  Follow-up.
- No blocking/`OverflowPolicy` choice for the `File` sink — always drop-on-full, per the user's
  explicit requirement.
- No header line in the log file, no per-device-id column — a device's identity stays embedded in
  its message text (`"{name} @0x{addr:04x} reset"`), exactly as today's `debug!` calls already
  format it.

## Files to modify

- `src/emulator/logging.rs` (new) — `LogLevel`, `LogCategory`, `LogRecord`, `Sink`, `LogSender`
  (incl. `Default`), `log_msg!` macro, text formatter, `spawn_log_writer`, unit tests.
- `src/emulator/mod.rs` — `mod logging;` + `pub use logging::{LogCategory, LogLevel, LogRecord, LogSender, spawn_log_writer};` + `pub(crate) use logging::log_msg;`.
- `src/emulator/cpu/mod.rs` — `Cpu.log_sender: LogSender` field, `CpuBuilder::log_sender()`, `Cpu::set_log_sender()`, `finish_cycle()`/`reset()` updates.
- The same one-field/one-setter/one-call-site change repeated in the 10 device files:
  `src/emulator/device/{console,finch,led_matrix,lfsr,mc6840,mc6850,phoebe,r6551,via6522,vireo}.rs`.
- `src/emulator/config/registry.rs` — `InstantiationContext.log_sender: Option<LogSender>` field (struct def + 4 test literals).
- The same one-line guard repeated in the matching 10 config modules:
  `src/emulator/config/{console,finch,led_matrix,lfsr,mc6840,mc6850,phoebe,r6551,via6522,vireo}.rs`.
- `src/emulator/config/emulator.rs` — both `InstantiationContext` literals in `Config::build`/`build_with_context` gain `log_sender: None,`/forwarded value.
- `src/emulator/config/console.rs` (5 test literals), `src/emulator/config/pic_finch.rs` (1 test literal) — add `log_sender: None,`.
- `src/bin/emulator/config.rs` — `--log-file` clap arg.
- `src/bin/emulator/main.rs` — open the log file, `spawn_log_writer`, attach to `Cpu` and `InstantiationContext`, join the handle at shutdown.
- `debugger/src-tauri/src/lib.rs` — add `log_sender: None,` to its one `InstantiationContext` literal.
- `Cargo.toml` — add `jiff = "0.2"` as a direct dependency.

## Verification

- `cargo build --workspace` — confirms the new `InstantiationContext` field compiles through every construction site, including the debugger crate.
- `cargo test --workspace` — new tests in `logging.rs`: text formatter escapes `\n`/`\r` in `message` but leaves other content alone; a capacity-1 channel drops and counts a second `log()` call before the writer thread drains it; dropping every `LogSender` clone and joining the writer's handle deterministically flushes and closes the file (no sleep-based polling, matching `spawn_trace_writer`'s existing drop/join test shape); a `Default`-constructed `LogSender` forwards through the `log` crate and reports `dropped_count() == 0`. Update/add a test per device asserting the emitted record (captured via a test `File`-backed sender) has `category: LogCategory::Device` and the expected message text. Add a `Cpu::reset()` test asserting the CPU's own record carries `cycles: 0`, and a test asserting a device's mid-`bus_reset()` record carries the pre-reset accumulated count.
- `cargo clippy` — run after the edits, not just the initial pass.
- All newly-`pub` items need doc comments before committing.
- Manual check: `cargo run --bin emma65 -- --log-file /tmp/emma65.log ...`, confirm the file contains one space-delimited line per device reset plus the CPU's own, parseable via `cut -d' ' -f1-4` for the fixed columns and `cut -d' ' -f5-` for the message.

## Follow-up (out of scope for this pass)

- The actual debugger UI panel that tails/parses this file and renders it as a table.
- Wiring the debugger crate to open a real log file (e.g. under the profile directory, alongside
  `program.bin`/`emulator.toml` — see `debugger/src-tauri/src/profile.rs`'s `ensure_profile_dir`)
  and thread a `LogSender` through its own `InstantiationContext`/`Cpu` construction.
- Deciding whether/how `DeviceEvent` (`TransportConnected`, `TransportError`, `DeviceInfo`,
  `RejectedWrite`, `OutboundBytesDropped`, `InboundEventsDropped`) should also flow through this
  facility, populating `LogCategory::Transport` for real.
- Any consumer of `LogSender::dropped_count()` (e.g. a periodic "N log messages dropped" warning).
- Log file rotation/append-across-sessions (this pass truncates on each run, matching
  `--trace-file`'s existing behavior).
- Extending `log_msg!` (or adding sibling macros) beyond this crate's own device/CPU code, and any
  public re-export of it, should downstream `IoDevice` implementations want it.
