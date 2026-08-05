# Emulator: Richer CPU Execution Tracing

Implements the design direction from
[issue #266](https://github.com/ceharris/emma65-rust/issues/266).

## Context

The current CPU trace facility (`src/emulator/cpu/trace.rs`) records each bus
read/write with a host monotonic-clock timestamp (`Instant`-sampled once per
instruction) purely so a consumer can group bus ops belonging to the same
instruction. This is expensive (a clock sample every step) and, per user
feedback in issue #266, not actually useful: what a trace consumer needs to
reconstruct program behavior after the fact is (a) a cheap correlation id,
not wall-clock time, (b) the CPU register state as it was *immediately
before* each instruction executed — recorded directly, not reconstructed via
a shadow-register simulation (explicitly rejected: doesn't hold up once a
debugger user halts and hand-edits registers mid-session), and (c) a way to
turn the raw byte-level trace into disassembled instructions without manual
post-processing.

This plan replaces the timestamp-based correlation with a `u64` instruction
counter, adds a lazily-emitted register snapshot per instruction, and adds a
trace-driven disassembly path (`TraceDisassembler` + a `BinaryTraceReader`
for offline analysis of recorded trace files). The binary trace file format
changes incompatibly; since there are no consumers of existing trace files
(verified: no references anywhere in the `debugger/src-tauri` workspace
member, only this crate's own tests use `TraceRecord`), this ships as a
straight breaking change plus a version header for future-proofing.

## Decisions

- Disassembly output is a **separate stream**, not a new `TraceKind`
  variant — keeps `TraceRecord`/`TraceKind` `Copy` and fixed-width. The
  issue's own wording suggested merging disassembly into the trace record
  stream; this was explicitly reconsidered and overridden after flagging
  the trade-off (a merged variant would make `TraceRecord` variable-length
  and non-`Copy`, since `DisassembledLine` holds `Vec<u8>`/`String` fields).
- The new binary format gets an **8-byte magic+version header**, even
  though nothing currently reads existing trace files — cheap insurance for
  a format explicitly meant to support later reconstruction/analysis.
- Scope includes a **`BinaryTraceReader`** for offline trace-file analysis,
  in addition to the writer and in-process `TraceDisassembler`.
- The standalone emulator binary (`src/bin/emulator/`) gets a `--trace-file`
  CLI option that writes a trace for the whole run. This is small and
  self-contained (the writer/channel-offload plumbing already exists), it
  gives real users of the binary immediate benefit, and it doubles as the
  end-to-end verification path for this whole plan (no debugger UI work
  needed to exercise it).

Single branch for the whole issue, one commit per unit below (per this
project's multi-unit workflow convention), pushed incrementally without
waiting for merge.

## Current implementation (ground truth)

- `src/emulator/cpu/trace.rs`: `TraceRecord { timestamp_ns, addr, value, op: BusOp }`,
  `TraceCallback` trait, `BinaryTraceWriter<W>` (fixed 12-byte LE records:
  timestamp_ns@0-7, addr@8-9, value@10, op@11), `OverflowPolicy`,
  `ChannelTraceCallback` + `spawn_trace_writer` (writer-thread offload via
  `crossbeam_channel`), `TraceState { epoch: Instant, current_ns: u64 }`.
- `src/emulator/cpu/mod.rs`: `Cpu::step()` calls `trace_state.tick()` at
  line ~256-258, *before* breakpoint/watch checks, RESET/NMI/IRQ dispatch,
  and opcode fetch — i.e. at the exact moment `self.regs` still holds the
  state prior to whatever this step does. `bus_read`/`bus_write` call
  `emit_trace()` on success only (never on `peek`, which the disassembler
  and debugger use for side-effect-free reads). `Registers { a, x, y, s,
  pc, p: StatusRegister }` is `Copy`; `StatusRegister` is a `bitflags`
  newtype over `u8` with `.bits()`.
- `src/emulator/disasm/mod.rs`: `Disassembler::disassemble_one(&self, bus:
  &Bus, addr: u16) -> DisassembledLine` peeks opcode + operand bytes from
  `Bus`, then builds `DisassembledLine` via `format_operand(&decoded,
  &raw_bytes, addr, symbol_table)` — which only needs already-collected
  `raw_bytes` + `SymbolTable`, no further bus access. This means
  byte-collection (bus-driven) and decoding (pure) are already naturally
  separable.
- No consumers outside this crate: `debugger/src-tauri` has zero references
  to `BusOp`/`TraceRecord`/`TraceCallback`; its `Disassembler` usage is
  unrelated (bus/peek-driven live disassembly view, must keep working
  unchanged).
- Tests referencing the current shape: `src/emulator/cpu/trace.rs:142-208`
  (4 tests), `src/emulator/cpu/mod.rs` ~2197-2283 (5 tests, including
  `trace_timestamps_group_by_instruction` which directly encodes the
  "same timestamp = same instruction" contract being replaced), and
  `tests/exec_integration.rs:100-183` (`bus_trace_captures_reads_and_writes`,
  asserts `timestamp_ns` monotonicity across the whole trace).

## Unit-by-unit plan

### Unit 1 — Redefine core trace types (`src/emulator/cpu/trace.rs`)

Replace `TraceRecord`/`TraceState`; rewrite `BinaryTraceWriter` for the new
layout; add `BinaryTraceReader`.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceKind {
    /// Register snapshot taken immediately before this instruction executed.
    Registers(super::Registers),
    Read { addr: u16, value: u8 },
    Write { addr: u16, value: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceRecord {
    /// Monotonically increasing id shared by every record (Registers + all
    /// bus ops) belonging to the same `Cpu::step()` call.
    pub instr_id: u64,
    pub kind: TraceKind,
}
```

`Registers`/`StatusRegister` are already `Copy`/`Eq`, so `TraceRecord`
keeps deriving both with no further changes.

**Binary format** (magic+version header, confirmed with user):
- 8-byte file header, written once: 4-byte magic (e.g. `b"E65T"`) + 1-byte
  format version (`1`) + 3 reserved/zero bytes.
- 16-byte fixed-width records thereafter, little-endian:
  - bytes 0..8: `instr_id` (u64)
  - byte 8: tag (0 = Registers, 1 = Read, 2 = Write)
  - bytes 9..16: 7-byte payload, tag-dependent, zero-padded
    - Registers: `a, x, y, s` (4×u8) + `pc` (u16) + `p.bits()` (u8) = 7 bytes exactly
    - Read/Write: `addr` (u16) + `value` (u8) = 3 bytes, 4 bytes padding

`BinaryTraceWriter::new()` writes the header on construction (or lazily on
first `record()` call — construction is simpler and matches "one writer per
file"). `BinaryTraceReader<R: Read>::new(inner: R) -> io::Result<Self>`
reads and validates the header eagerly (bad magic/version → `io::Error`),
then implements `Iterator<Item = io::Result<TraceRecord>>`, reading 16 bytes
per `next()` call (clean EOF at a record boundary → `None`; a short/partial
final read or unknown tag byte → `Some(Err(..))`).

Also add `BinaryTraceReader`, `TraceKind`, `spawn_trace_writer`,
`OverflowPolicy`, and `ChannelTraceCallback` to the re-export list in
`src/emulator/mod.rs:13` (currently only `BinaryTraceWriter`,
`TraceCallback`, `TraceRecord` are re-exported) — the new binary's
`--trace-file` option (Unit 8) and any external consumer need these without
reaching into `emma65::emulator::cpu::trace` directly.

Replace `TraceState`:

```rust
pub(in crate::emulator) struct TraceState {
    next_instr_id: u64,
    pending: Option<(u64, super::Registers)>, // (instr_id, snapshot), not yet flushed
}

impl TraceState {
    pub(in crate::emulator) fn new() -> Self { .. }

    /// Called once per `Cpu::step()`, before any register mutation for that step.
    pub(in crate::emulator) fn begin_instruction(&mut self, regs: super::Registers) {
        let id = self.next_instr_id;
        self.next_instr_id = self.next_instr_id.wrapping_add(1);
        self.pending = Some((id, regs));
    }

    pub(in crate::emulator) fn current_instr_id(&self) -> u64 {
        self.next_instr_id.wrapping_sub(1)
    }

    /// Takes the pending Registers snapshot for `instr_id` if not yet flushed.
    pub(in crate::emulator) fn take_pending_registers(&mut self, instr_id: u64) -> Option<super::Registers> {
        match self.pending.take() {
            Some((id, regs)) if id == instr_id => Some(regs),
            other => { self.pending = other; None }
        }
    }
}
```

`instr_id` continues incrementing across `Cpu::reset()` (matches old
`epoch`/timestamp behavior, which `reset()` never touched).

**Tests**: rewrite the 4 existing tests in `trace.rs` for the new shape
(`trace_record_fields`, `binary_writer_record_layout` — now covering
header + all 3 tags/padding, `capturing_callback_receives_records`,
replace `trace_state_tick_advances_monotonically` with a
`begin_instruction`/`take_pending_registers` test). Add: a
`take_pending_registers_returns_none_for_stale_id` test, a
`binary_writer_registers_record_roundtrips_through_reader` test proving
writer output round-trips through `BinaryTraceReader` byte-for-byte
(including header validation and a bad-magic/bad-version rejection case).

### Unit 2 — Wire `TraceState` into `Cpu` (`src/emulator/cpu/mod.rs`)

- `step()` (~256-258): replace `self.trace_state.tick()` with
  `self.trace_state.begin_instruction(self.regs)`, same call site.
- `emit_trace()` (~961-970) becomes the lazy-flush point: on the first bus
  op for the current `instr_id`, flush the pending `Registers` record
  before the `Read`/`Write` record; subsequent bus ops for the same
  `instr_id` just emit their own record. Steps that short-circuit before
  any bus op (breakpoint/watch hit, still-WAI) never emit a dangling
  `Registers` record — same behavior as today's "only emit on actual bus
  activity."

**Tests**: update the 5 existing trace tests in `cpu/mod.rs` (~2197-2283)
for the new record shape; replace `trace_timestamps_group_by_instruction`
with a test asserting Registers-record-first-then-bus-ops grouping by
`instr_id`. Add an end-to-end test driving real `cpu.step()` calls (not raw
`bus_read`/`bus_write`) asserting: each bus-touching instruction emits
exactly one `Registers` record whose `pc` matches the instruction's actual
starting PC (proves pre-execution timing), and a breakpoint-halted
instruction emits zero records.

### Unit 3 — Update `tests/exec_integration.rs`

`bus_trace_captures_reads_and_writes` (lines 100-183): match on `rec.kind`
instead of `rec.addr`/`rec.op`; replace the `timestamp_ns` monotonicity
assertion with the same `windows(2)` check over `instr_id`. Add an
assertion that the `Registers` record preceding the `STA $0300` write shows
`a == 0x55`, confirming correct pre-instruction timing on a real multi-
instruction program.

### Unit 4 — Split `Disassembler::disassemble_one` into collect + build (`src/emulator/disasm/mod.rs`)

Pure refactor, no behavior change — sets up Unit 5.

```rust
impl Disassembler {
    fn collect_bytes(&self, bus: &Bus, addr: u16) -> Vec<u8> { /* unchanged logic, extracted */ }

    /// Pure: builds a `DisassembledLine` from already-collected bytes.
    /// No bus access — only the opcode table + symbol table.
    pub(crate) fn build_line(&self, addr: u16, raw_bytes: Vec<u8>, symbol_table: &SymbolTable) -> DisassembledLine { .. }

    pub fn disassemble_one(&self, bus: &Bus, addr: u16) -> DisassembledLine {
        let raw_bytes = self.collect_bytes(bus, addr);
        self.build_line(addr, raw_bytes, bus.symbol_table())
    }
}
```

No existing tests should need changes (external behavior unchanged). Add
one test confirming `collect_bytes` + `build_line` composed match
`disassemble_one`'s existing output, to lock in the refactor.

### Unit 5 — `TraceDisassembler` (new `src/emulator/disasm/trace.rs`)

State machine consuming `&TraceRecord`s, holding an internal `Disassembler`
(reused for `build_line`) plus an owned `SymbolTable` clone (labels are
static, a one-time clone at construction is sufficient — no live-refresh
requirement for v1).

```rust
pub struct TraceDisassembler {
    disassembler: Disassembler,
    symbols: SymbolTable,
    pending: Option<Pending>,
}

struct Pending { instr_id: u64, addr: u16, raw_bytes: Vec<u8>, expected_len: Option<u8> }

impl TraceDisassembler {
    pub fn new(variant: CpuVariant, symbols: SymbolTable) -> Self { .. }

    /// Feeds one trace record; returns a completed line once an instruction's
    /// opcode/operand bytes have all been observed.
    pub fn feed(&mut self, rec: &TraceRecord) -> Option<DisassembledLine> { .. }
}
```

Logic: `TraceKind::Registers(regs)` starts a new `Pending` at `regs.pc`
(dropping any unfinished prior `Pending`). `TraceKind::Read { addr, value }`
only contributes to `pending.raw_bytes` when `addr` matches the next
expected sequential address (`pending.addr + raw_bytes.len()`) — this
naturally excludes effective-address/pointer-table reads and data reads
performed by the instruction, since those don't land on consecutive bytes
from the opcode's start. Once the opcode byte is known, `expected_len`
comes from `self.disassembler`'s decode table (`byte_len`); the line
completes and is emitted once `raw_bytes.len() == expected_len`.
`TraceKind::Write` is always ignored (never part of opcode/operand fetch).

**Tests** (new — no trace+disasm coverage exists today): single-byte
implied instruction, 2-byte immediate instruction, an indexed-indirect /
indirect-indexed instruction to confirm EA-pointer reads and the final data
read are correctly excluded, an instruction with a data write to confirm
writes are ignored, and an integration test comparing `TraceDisassembler`
output (fed from a real captured trace) against `Disassembler::
disassemble_range`'s output for the same program, to prove the two paths
agree.

### Unit 6 — `DisassemblingTraceCallback` adapter

Small wrapper composing `TraceDisassembler` with the existing
`TraceCallback` machinery, satisfying "runs off the CPU thread" when
installed underneath `spawn_trace_writer`'s writer thread, without any
change to `TraceRecord`'s wire format.

```rust
pub trait DisassemblyListener: Send {
    fn on_line(&mut self, line: DisassembledLine);
}

pub struct DisassemblingTraceCallback<C: TraceCallback, L: DisassemblyListener> {
    inner: C,
    disassembler: TraceDisassembler,
    listener: L,
}

impl<C: TraceCallback, L: DisassemblyListener> TraceCallback for DisassemblingTraceCallback<C, L> {
    fn record(&mut self, rec: TraceRecord) {
        if let Some(line) = self.disassembler.feed(&rec) {
            self.listener.on_line(line);
        }
        self.inner.record(rec);
    }
}
```

**Tests**: fake `DisassemblyListener` capturing lines, driven through
`DisassemblingTraceCallback` wrapping a capturing inner callback over a
short real program; assert both raw records and disassembled lines are
produced correctly and in order.

### Unit 7 — Documentation pass

Update the module-level doc comment in `trace.rs` and the
`BinaryTraceWriter`/`BinaryTraceReader` layout doc comments to describe the
new header + 16-byte record format.

### Unit 8 — `--trace-file` option on the standalone emulator binary

Adds a small, self-contained CLI option to `src/bin/emulator/` that writes a
whole-run binary trace via the existing `spawn_trace_writer` writer-thread
offload — no new plumbing needed beyond wiring the option through.

`src/bin/emulator/config.rs`: add a field to `AppConfig` (binary-only
concern, sits alongside `emulator: Config` rather than inside the library
`Config` type):

```rust
pub struct AppConfig {
    #[clap(flatten)]
    #[serde(flatten)]
    pub emulator: emma65::emulator::Config,

    /// Path to write a binary CPU execution trace to.
    #[clap(long = "trace-file")]
    pub trace_file: Option<emma65::emulator::ExpandedPathBuf>,
}
```

`ExpandedPathBuf` matches the existing convention for path-like config
elsewhere in this crate (transport paths use it) and gives `~/`-expansion
for free via its `FromStr`/`Deserialize` impls.

`src/bin/emulator/main.rs`: after `cpu.reset()` succeeds and before
`emma65::emulator::run(cpu)` takes ownership of `cpu`, open the file and
install the callback:

```rust
let trace_writer_handle = match &config.trace_file {
    Some(path) => {
        let file = std::fs::File::create(path.as_ref()).unwrap_or_else(|e| {
            eprintln!("error: failed to open trace file {}: {e}", path.display());
            std::process::exit(1);
        });
        let writer = emma65::emulator::BinaryTraceWriter::new(file);
        let (callback, handle, _dropped) =
            emma65::emulator::spawn_trace_writer(writer, 4096, emma65::emulator::OverflowPolicy::DropOnFull);
        cpu.set_trace_callback(Some(Box::new(callback)));
        Some(handle)
    }
    None => None,
};
```

`ChannelTraceCallback`'s sender is owned by the callback, which moves into
`cpu`, which moves into `run(cpu)`; when the run loop finishes and drops
`cpu`, the writer thread's `rx.recv()` returns `Err` and it flushes and
exits on its own — so after the existing `tokio::select!` loop, join the
handle to guarantee the trace file is fully flushed before the process
exits:

```rust
if let Some(handle) = trace_writer_handle {
    let _ = handle.join();
}
```
placed just before the final `print!("\r\n")` / `exit_code` return.

`DropOnFull` is the right default here (matches this binary's existing
"never let auxiliary I/O stall emulation" posture); no new CLI flag for
overflow policy — not asked for, keep it simple.

**Tests**: this is a binary-level, process-spawning change; no unit test
infrastructure exists for `src/bin/emulator/` today (confirm during
implementation — if none does, cover it via the manual verification step
below rather than inventing a new test harness for one flag).

## Files touched

- `src/emulator/cpu/trace.rs` (Units 1, 7)
- `src/emulator/cpu/mod.rs` (Unit 2)
- `src/emulator/mod.rs` (Unit 1, re-exports)
- `tests/exec_integration.rs` (Unit 3)
- `src/emulator/disasm/mod.rs` (Unit 4, plus `pub mod trace;`)
- `src/emulator/disasm/trace.rs` — new (Units 5, 6, 7)
- `src/bin/emulator/config.rs`, `src/bin/emulator/main.rs` (Unit 8)

## Verification

- `cargo build` and `cargo clippy` after each unit.
- `cargo test --workspace` after each unit (per project convention — `--lib`
  alone would miss the `tests/exec_integration.rs` suite).
- End-to-end manual check (after Unit 8 lands): run the standalone binary
  with `--trace-file <path>` against a short program, let it produce some
  output, exit it, then write a small throwaway script (or a temporary
  `#[test]`/example) that opens the resulting file with `BinaryTraceReader`,
  feeds the records through `TraceDisassembler`, and confirms the emitted
  disassembly is sane and matches what `Disassembler::disassemble_range`
  produces for the same region against the ROM/RAM image — this exercises
  writer → file → reader → trace-disassembler end to end without needing
  any debugger UI work.
- Per project convention, all new/changed public structs, enums, methods,
  and module declarations need doc comments before committing.
