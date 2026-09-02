# Trace File Format

The binary format written by `BinaryTraceWriter` and read by
`BinaryTraceReader` when the CPU's [execution tracing](the-emulator-core.md#execution-tracing)
is active. Implemented in `src/emulator/cpu/trace.rs`. It's reference
material for writing an independent trace file reader (e.g. a script or
alternate viewer) — the bundled `emma65-tracer` binary already decodes it
into disassembly listings (see [Running the Tracer](running-the-tracer.md)),
and the debugger's Trace window reads it live without needing to touch this
format directly.

A trace file is a fixed 8-byte header followed by a sequence of fixed-width
16-byte records, all little-endian, with no trailer.

## Header

| Offset | Length | Field | Description |
|--------|--------|-------|--------------|
| 0 | 4 | magic | ASCII `E65T` |
| 4 | 1 | format version | Currently `2`. A reader must reject any other value. |
| 5 | 1 | CPU variant | `0` = `Cmos65C02`, `1` = `Wdc65C02` |
| 6 | 2 | reserved | Always zero |

The CPU variant identifies which 65C02 variant produced the trace — relevant
to a decoder because `Wdc65C02` adds 34 opcodes (`STP`, `WAI`, `BBR0`–`7`,
`BBS0`–`7`, `RMB0`–`7`, `SMB0`–`7`) beyond the base `Cmos65C02` set.

## Records

Each record is exactly 16 bytes:

| Offset | Length | Field | Description |
|--------|--------|-------|--------------|
| 0 | 8 | `instr_id` | u64. See [Instruction correlation](#instruction-correlation) below. |
| 8 | 1 | tag | `0` = Registers, `1` = Read, `2` = Write, `3` = Cycles |
| 9 | 7 | payload | Tag-dependent, zero-padded |

A reader determines a record's meaning from the tag byte alone; the payload
layout is fixed per tag, not length-prefixed.

### Registers (tag 0)

A snapshot of all CPU registers taken immediately before the instruction
identified by `instr_id` began executing. Emitted once per instruction, as
the first record for that `instr_id`.

| Payload offset | Length | Field |
|-----------------|--------|-------|
| 0 | 1 | `A` |
| 1 | 1 | `X` |
| 2 | 1 | `Y` |
| 3 | 1 | `S` |
| 4 | 2 | `PC` (u16) |
| 6 | 1 | `P` (status register, see below) |

The `P` byte is the eight processor status flags packed as
`N V - B D I Z C` (bit 7 down to bit 0), where `-` is the always-set UNUSED
bit — the same encoding pushed to the stack on real hardware:

| Bit | 7 | 6 | 5 | 4 | 3 | 2 | 1 | 0 |
|-----|---|---|---|---|---|---|---|---|
| Flag | N | V | UNUSED | B | D | I | Z | C |

### Read (tag 1) / Write (tag 2)

A single bus access performed while executing the instruction identified by
`instr_id`. A record is emitted for every bus read and write the CPU
performs — operand fetches, opcode fetches, stack pushes/pops, and each
device access — never for a device's side-effect-free `peek`.

| Payload offset | Length | Field |
|-----------------|--------|-------|
| 0 | 2 | `addr` (u16) |
| 2 | 1 | `value` |
| 3 | 4 | (padding) |

### Cycles (tag 3)

The total clock cycle count for the instruction identified by `instr_id`
(base cycles plus any addressing-mode or branch-taken extra cycles), known
only once the instruction has finished executing. Emitted once per
instruction, as the last record for that `instr_id` — after every
`Registers`, `Read`, and `Write` record sharing that id.

| Payload offset | Length | Field |
|-----------------|--------|-------|
| 0 | 1 | cycle count |
| 1 | 6 | (padding) |

## Instruction correlation

`instr_id` is a monotonically increasing counter (wrapping on overflow,
which in practice never happens) assigned once per `Cpu::step()` call. Every
record belonging to the same instruction — its `Registers` snapshot, each
bus `Read`/`Write` it performs, and its final `Cycles` total — shares the
same `instr_id`, in that relative order, though other instructions' records
never interleave with them since the CPU executes one instruction to
completion before the next begins. A reader reconstructing a
per-instruction view (as `emma65-tracer` does) can therefore group records
by run of equal `instr_id`.

## Seeking

Because every record is exactly 16 bytes, the byte offset of record `n`
(0-based, counting from the first record after the header) is
`8 + n * 16`. A reader with a seekable source can jump directly to any
record without scanning from the start — this is how the debugger's Trace
window serves windowed reads over a large live trace file.
