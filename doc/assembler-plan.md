# Bespoke assembler module (`src/assembler/`) — issue #474

## Context

Issue #474 ("Basic Assembler Support") is about adding source-editing and
assemble-to-memory capability to the debugger. Discovery (recorded on the
issue) surveyed CodeMirror-based editing, ca65/ld65 subprocess integration,
WLA-DX, and existing Rust assembler crates, and landed on a **minimal
bespoke assembler**, in the spirit of Mesen's inline assembler:

- Straight to memory, no linker, no relocation, no linking between
  separately-assembled units — but a single assembly can contain more
  than one `.org` directive, each starting a new output segment
  (offset + length).
- Directives: `.org`, `.byte` (incl. string-literal operands), `.word`,
  `.res`.
- Symbol definition via `FOO = expr`, `FOO .equ expr`, or a label on an
  instruction (`my_routine:  LDA #$55`).
- Expressions over symbols and literals (binary/octal/decimal/hex/char),
  with arithmetic, logical, bitwise, and shift operators.
- Forward references are essential → multi-pass resolution, with zero-page
  addressing-mode optimization once operand values are known.
- The `watch` module's expression parser (`src/watch/`) is called out as a
  *reference*, not something to reuse directly (different grammar,
  different evaluation target).

**Clarification from planning:** the expression grammar omits
relational/equality operators entirely (no `<`, `>`, `<=`, `>=`, `==`,
`!=`). Instead, `<` and `>` become unary *prefix* operators — the classic
6502-assembler LSB/MSB extractors (e.g. `LDA #<label`, `LDA #>label`).
`<<`/`>>` remain as the binary shift operators; the lexer disambiguates by
maximal munch (`<<` is always one token), and the parser only accepts bare
`<`/`>` in prefix position, so there's no grammar ambiguity.

This plan covers **only the library module** (`src/assembler/`) — turning
assembly source text into bytes-at-an-address plus a symbol table. Wiring
this into the debugger (a Tauri command, writing through `Bus`, a
CodeMirror editor) is explicitly out of scope and left for later issue
units, consistent with how the `watch` module was built as a standalone
library piece before `debugger/src-tauri/src/watchpoints.rs` consumed it.

## Design decisions

### Module shape mirrors `watch`, with two deliberate simplifications

`src/assembler/` will follow the same layered shape as `src/watch/`
(scanner → tokens → recursive-descent parser → AST, `mod.rs` as a narrow
public façade) since that architecture is proven in this codebase. Two
differences, both because the assembler's usage pattern differs from
watchpoints (compiled once, evaluated every CPU step, for the life of a
session):

1. **No separate bytecode/OpCode compile step.** Watch lowers `Expr<'a>`
   into an owned `Vec<OpCode>` because watchpoints must outlive their
   source string and get evaluated on every `Cpu::step()`. The assembler's
   parsed `Expr<'a>` trees only need to live for the duration of one
   `assemble(source: &str)` call (a handful of passes), so we evaluate the
   borrowed AST directly each pass. This drops an entire layer
   (`compiler.rs`, `OpCode`) relative to `watch`.
2. **No signed/unsigned opcode variants.** Watch threads a `signed: bool`
   through every node because it has signed comparison/shift/fetch
   operators. With relational operators gone and no memory-fetch operator,
   assembler expressions only need wrapping unsigned 32-bit arithmetic
   (unary `-` via `wrapping_neg`), so there's no signedness concept to
   propagate at all.

### Shared lexer primitives: extract `Text`/`Location` instead of duplicating

`src/watch/text.rs` (`Text<'a>` byte cursor) and `src/watch/location.rs`
(`Location { line, column }`) are fully generic — no watch-specific
logic — and neither is part of `emma65::watch`'s public surface (nothing
re-exports them). Duplicating ~110 lines of identical cursor code would go
against reusing what already exists. Unit 1 below promotes them to a
small crate-internal module reused by both `watch` and `assembler`.

### Precedence chain (loosest → tightest)

Same C-family shape as `watch::parser`, minus the equality/relational
tiers, plus prefix `<`/`>`:

```
||                              (logical or)
&&                              (logical and)
|                               (bitwise or)
^                               (bitwise xor)
&                               (bitwise and)
<<  >>                          (shift)
+  -                            (additive)
*  /  %                         (multiplicative)
unary: -  +  !  ~  <  >         (negate/identity/logical-not/bitwise-not/lsb/msb)
primary: number | char | symbol | ( expr )
```

`<`/`>` bind like unary minus — tightest, right before primary — matching
ca65/other 6502 assemblers' `#<label` / `#>label` convention. `Expr`
values evaluate to a wrapping `u32`; `<`/`>` mask `& 0xFF` /
`(v >> 8) & 0xFF` — no separate byte/word "fetch" node is needed since
there's no memory-read operator in this grammar (watch's `B[`/`W[`/`D[`
have no assembler equivalent — symbol/label values *are* the addresses;
nothing needs to be read back out of memory during assembly).

### Multi-pass resolution and zero-page optimization

The evaluator returns `Result<Option<u32>, Error>` per expression: `None`
means "contains an as-yet-undefined symbol," not an error — errors
(division by zero, etc.) only apply to expressions that *are* fully
resolvable this pass. The driver:

1. Pass 0: lay out every statement assuming the *widest* legal addressing
   mode for any instruction whose operand isn't yet fully resolvable
   (e.g. `Absolute` instead of `ZeroPage` when both exist for that
   mnemonic), assigning provisional addresses to labels as it goes. Layout
   tracks a *current segment* (started/reset by each `.org`) — an
   instruction's address is `segment.origin + running offset within that
   segment`, so multiple `.org`s just mean multiple segments accumulating
   independently in the same pass.
2. Each subsequent pass: re-evaluate every operand against the
   now-more-complete symbol table; shrink any instruction whose resolved
   operand now fits `ZeroPage` (only shrink, never grow — a value can only
   become "more known," not less). Any size change shifts every later
   address, so labels get updated too.
3. Repeat until no instruction changes size (fixed point) or a small pass
   cap is hit (a genuine non-convergence, e.g. mutually-shrinking
   addresses oscillating, is vanishingly unlikely for realistic code but
   needs a hard stop → reported as an error rather than looping forever).
4. Final pass: any operand still `None` is now a real error
   (`Error::UndefinedSymbol`).

Relative/`ZeroPageRelative` branch operands are not part of this
size-ambiguity — they're always encoded as a signed 8-bit PC-relative
offset (range-checked at final encode time), which is exactly what
`Disassembler::absolute_address` already computes in reverse
(`src/disassembler/mod.rs:207-215`) — worth a shared round-trip test.

### Reusing the CPU's opcode table instead of a second source of truth

`src/emulator/cpu/opcodes.rs` is the only place opcode/mnemonic/addressing
semantics are defined (`Mnemonic`, `AddressingMode`, `DecodedOp`,
`decode_table(variant) -> [DecodedOp; 256]`, all re-exported at
`emma65::emulator::{Mnemonic, AddressingMode, DecodedOp}`, `decode_table`
reachable via `emma65::emulator::cpu::opcodes::decode_table`). There is
currently no reverse (mnemonic, mode) → opcode-byte lookup anywhere in the
codebase. The assembler builds one **by inverting `decode_table()`** once
(e.g. a `HashMap<(Mnemonic, AddressingMode), DecodedOp>` built in a
`OnceLock`/at construction time), so the assembler and disassembler can
never drift apart — same guarantee the existing opcode tests
(`opcodes.rs:477-675`) already lean on. `Mnemonic` has `Display` (uppercase
mnemonic text, `opcodes.rs:90-138`) but no `FromStr`; the assembler adds
its own string→`Mnemonic` table (a natural inverse of the existing
`Display` match arms) rather than touching `opcodes.rs`.

Target `CpuVariant` is a constructor parameter (mirroring
`Disassembler::new(variant)`), and `DecodedOp::is_valid` is honored so the
34 WDC-only mnemonics (`STP`, `WAI`, and the `BBR0..7`/`BBS0..7`/
`RMB0..7`/`SMB0..7` families — `opcodes.rs:147-165`) are rejected when
assembling for plain `Cmos65C02`.

### Symbol table: reuse `emulator::bus::symbol::SymbolTable` for the result

`SymbolTable` (`src/emulator/bus/symbol.rs`, re-exported at
`emma65::emulator::SymbolTable`) already does exactly what the assembler's
*final* symbol set needs (`name -> u16` plus reverse lookup), and reusing
it means assembled output is immediately consumable by the disassembler
and, later, the debugger UI with zero glue code. During passes, symbol
*values* are still being resolved/shrunk, so the driver keeps its own
`HashMap<String, u32>` internally and only populates a `SymbolTable` for
the final `AssembledProgram` result.

### Diagnostics: improve on `watch::Error`'s shape

`watch::Error` is a flat `{line, column, message}` struct with only a
`Display` impl (no accessors) — fine for today's `.to_string()`-only
callers, but the issue's own stated end-goal (`@codemirror/lint` inline
diagnostics) needs a `Result`-shaped, structured error keyable by
line/column programmatically. The assembler's `Error` will keep the same
`{line, column, message}` shape (same terse call-site pattern) but expose
`line()`/`column()`/`message()` accessors from the start, so a future
CodeMirror-lint integration doesn't need a breaking change.

### `.org` / multiple segments

Revised from the original discovery framing ("single segment"): **one
assembly may contain multiple `.org` directives**, each closing out the
current segment (if any bytes were emitted into it) and opening a new one
at the given address. A byte-emitting statement (instruction or
`.byte`/`.word`/`.res`) before any `.org` is an error ("no active
segment") — labels are fine anywhere, resolved against whichever segment
is active when they're encountered. Two `.org`s landing on the same
address, or a later segment's range overlapping an earlier one's, is an
error — this is a self-consistency check within the assembly's own output
(labels/bytes silently colliding is almost certainly a mistake), not a
check against `Bus` memory-map semantics (see "Out of scope" below —
writing assembled output over an I/O device region is the caller's
problem, not the assembler's). No linking between segments or between
separately-assembled units is in scope — each segment is just an
independent `(origin, bytes)` pair in the result.

## Unit breakdown

One branch + PR per unit, stopping for review after each before starting
the next.

### Unit 1 — Shared lexer primitives + assembler scanner/tokens

- Promote `src/watch/text.rs` and `src/watch/location.rs` into a new
  crate-internal `src/text.rs` / `src/location.rs` (declared `mod text;
  mod location;` — not `pub` — in `src/lib.rs`); update `src/watch/mod.rs`
  and its submodules to use `crate::text::Text` / `crate::location::Location`
  instead of their own copies; delete the old `watch/text.rs`,
  `watch/location.rs`. No behavior change — pure move, existing `watch`
  tests must keep passing unmodified.
- New `src/assembler/mod.rs`, `src/assembler/token.rs`,
  `src/assembler/scanner.rs`: `TokenType` covering numbers (`$`/`0x` hex,
  `0o`/`0q` octal, leading-zero C-style octal, `0b` binary, decimal, `'c'`
  char literal — mirror `watch::scanner`'s number-literal helpers
  directly, they're generic), string literals (for `.byte "text"`),
  identifiers, `.directive` tokens, `;`-to-end-of-line comments (skipped,
  not tokenized), newline as a significant statement terminator (unlike
  watch, where `;` separates statements), `:` `=` `#` `,` `(` `)`, and the
  operator set above (including single-vs-double `<`/`<<`, `>`/`>>` via
  maximal munch).
- Add to `src/lib.rs`: `pub mod assembler;`.
- Tests: token-level scanning coverage per literal radix, comment
  stripping, and the `<` vs `<<` / `>` vs `>>` disambiguation.

### Unit 2 — Expression parser and evaluator

- `src/assembler/expr.rs`: `Expr<'a>` AST (Number, Symbol, Unary, Binary,
  Grouping — no Register/Flag/Variable/Assign/Fetch, those are
  watch-specific) with the precedence chain above.
- `src/assembler/parser.rs`: recursive-descent parser, `fn parse_expr(&mut
  self) -> Result<Expr<'a>, Error>`, borrowing from source like
  `watch::parser`.
- `src/assembler/eval.rs`: `fn evaluate(expr: &Expr, symbols: &HashMap<String,
  u32>) -> Result<Option<u32>, Error>` — the `Option` encodes "not yet
  resolvable," per the multi-pass design above. Division/remainder by zero
  → `Error`; unknown identifiers are *not* errors here (only at final-pass
  time, decided by the Unit 4 driver).
- Tests: literal evaluation across all radices/operators/precedence,
  resolvable vs. unresolvable symbol propagation, `<`/`>` extraction
  round-tripping against known 16-bit values.

### Unit 3 — Instruction table and per-line addressing-mode encoding

- `src/assembler/instructions.rs`: string → `Mnemonic` table (inverse of
  `opcodes.rs`'s `Display` impl); `(Mnemonic, AddressingMode) ->
  DecodedOp` reverse map built once from `decode_table(variant)`.
- `src/assembler/operand.rs`: parses the addressing-mode *syntax* after a
  mnemonic (`#expr` immediate, `(expr,X)` / `(expr),Y` / `(expr)` /
  `(expr,X)` indirect forms, bare `A` accumulator, implied/nothing, plain
  `expr` / `expr,X` / `expr,Y`, and the two-expression `zp_expr,
  rel_expr` form for `BBR*`/`BBS*`). Given an operand's resolved-or-not
  value (from Unit 2's `Option<u32>`) and which modes the mnemonic
  actually supports (from the reverse table), pick `ZeroPage*` when the
  value is known and fits `u8` and that mode exists for the mnemonic,
  else the wider mode; encode the final opcode byte + little-endian
  operand bytes.
- Tests: every addressing-mode family round-tripped through
  `Disassembler` (assemble bytes → disassemble → compare mnemonic/operand
  text), reusing the disassembler's existing per-mode test fixtures
  (`src/disassembler/mod.rs:218-628`) as a cross-check source.

### Unit 4 — Directives, statement grammar, multi-pass driver

- `src/assembler/statement.rs`: line-oriented statement grammar —
  `Label(name)`, `SymbolAssign(name, Expr)` (`FOO = expr` or `FOO .equ
  expr`), `Directive(Org(Expr) | Byte(Vec<ByteOperand>) | Word(Vec<Expr>) |
  Res(Expr))` where `ByteOperand` is `Expr | StringLiteral`, and
  `Instruction(Mnemonic, OperandSyntax)`. A line may carry a label *and* a
  directive/instruction.
- `src/assembler/driver.rs`: owns the pass loop described above — layout
  across one or more `.org`-delimited segments, shrink-to-zero-page
  convergence, final-pass undefined-symbol errors, segment-overlap
  detection, and building the final `HashMap<String, u32>` →
  `SymbolTable`.
- Tests: forward-reference resolution (label used before defined),
  zero-page shrink converging correctly when a forward-referenced label
  turns out to be `< $100`, `.byte`/`.word`/`.res` byte-layout
  correctness, string-literal `.byte` operands, non-convergence safety
  valve, multiple non-overlapping `.org` segments in one source, and
  errors for overlapping segments / bytes emitted before any `.org` /
  duplicate labels.

### Unit 5 — Public API and end-to-end tests

- `src/assembler/mod.rs` public façade (mirroring `watch::mod.rs`'s
  narrow surface): `pub struct Segment { pub origin: u16, pub bytes:
  Vec<u8> }`, `pub struct AssembledProgram { pub segments: Vec<Segment>,
  pub symbols: SymbolTable }` (segments in `.org` order), `pub struct
  Error { .. }` with `line()`/`column()`/`message()`, `pub fn
  assemble(source: &str, variant: CpuVariant) -> Result<AssembledProgram,
  Vec<Error>>` (collect *all* errors across the source, don't stop at the
  first — mirrors `WatchCompiler::compile_all`'s best-effort recovery).
- End-to-end tests assembling small multi-instruction programs (including
  forward refs, symbol arithmetic, `<`/`>` on a forward-referenced label,
  all four directives, and a program with two `.org` segments) and
  diffing against hand-computed expected bytes; at least one full
  assemble → per-segment `Disassembler::disassemble_range` → re-compare
  round trip.
- `cargo clippy --all-targets` clean.

## Explicitly out of scope (future issue units)

- Any `debugger/src-tauri` Tauri command or frontend/CodeMirror work.
  `Bus::patch(addr, value)` (`src/emulator/bus/mod.rs:202`) already writes
  to any address, bypassing ROM-write restrictions, which is exactly what
  writing assembled segments into memory needs — no new `Bus` query/
  validation API is required. If a segment-at-a-time convenience (taking
  an origin + byte slice instead of looping per-byte) turns out to be
  worth having, that's a small additive helper on `Bus`, not a
  prerequisite for this module. An assembled program that overlaps an
  I/O-device-mapped region is a user error the assembler/`Bus` don't need
  to detect — `Bus::patch` still calls `device.patch(addr, value)` for
  device regions (per existing `Bus::patch` semantics), so it does
  *something* well-defined either way.
- A VICE-label *writer* (only a reader, `load_vice_labels`, exists today)
  for exporting assembler-produced symbols in that format.
- Source-level step debugging / `.dbg`-style debug info.

## Verification

- `cargo test` (unit tests per module above) and `cargo test --workspace`
  before each unit's PR.
- `cargo clippy` clean.
- Round-trip tests against `Disassembler` are the main functional
  correctness signal, since the assembler and disassembler share one
  opcode source of truth (`opcodes.rs::decode_table`) by construction.
