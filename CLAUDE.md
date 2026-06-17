# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build          # build
cargo test           # run all tests
cargo test <name>    # run a single test by name (partial match)
cargo clippy         # lint
```

## Architecture

`emma65` is a Rust crate with a library and a binary. The library currently contains one public module, `watch`, which implements a complete pipeline for evaluating **watchpoint expressions** — conditions used to break or watch memory/registers in the emma65 6502-family emulator.

The full pipeline is:

```
source &str → Scanner → Vec<Token> → Parser → Expr tree → Compiler → Vec<OpCode> → Evaluator → Operand (u32)
```

No external dependencies; zero-copy design throughout.

### Crate structure

- **`src/lib.rs`** — crate root; exposes `pub mod watch`
- **`src/main.rs`** — binary entry point
- **`src/watch/`** — watchpoint expression pipeline (see below)

### `watch` module (`src/watch/`)

All items are internal submodules of `emma65::watch`. The module re-exports its public API from `mod.rs`:

```rust
pub use self::context::WatchContext;
pub use self::error::{Error, WatchError};
pub use self::expr::Operand;
pub use self::parser::Mapper;
pub use self::session::{WatchCompiler, WatchEvaluator, Watchpoint};
```

The primary public entry points for emulator/debugger code are `WatchCompiler` and `WatchEvaluator`. The `WatchEvaluator` owns variable name→index mappings and runtime storage internally.

#### Submodules

- **`text`** — zero-copy cursor over a `&str`. `consume()` returns slice `[start..current]` and resets `start`. Used by `Scanner` to produce token text without allocating.

- **`scanner`** — tokenizes source text into `Vec<Token<'a>>`, borrowing text slices from the source. Handles decimal, hex (`0x`/`$`), octal (`0o`/`0q`/leading-`0`), and binary (`0b`) number literals. Tracks line/column for error reporting.

- **`token`** — `Token<'a>` holds a `TokenType`, a `&'a str` text slice, and a `Location`. `TokenType` includes 40+ variants for operators, literals (`Number(u32)`, `String(String)`, `Symbol(String)`), and punctuation.

- **`expr`** — AST node types. `Expr<'a>` has a `Token<'a>` and an `ExprType<'a>`:
  - `Number(Operand)`, `Register(Operand)`, `Flag(Operand)`, `Variable(Operand)` — leaf nodes
  - `Assign(Operand, Box<Expr>)` — walrus assignment; stores RHS into variable slot and yields the value
  - `UnaryOperator(UnaryOperatorType, Box<Expr>)` — includes `Fetch(OperandWidth)` for memory reads
  - `BinaryOperator(BinaryOperatorType, Box<Expr>, Box<Expr>)`
  - `signed: bool` field tracks whether the result should be treated as signed

- **`variables`** — `Variables` maps variable names to stable `Operand` IDs. `get_or_create` allocates a new ID on first use and is idempotent thereafter. Used internally by `WatchEvaluator`.

- **`parser`** — recursive descent. Precedence (lowest to highest): assignment (`:=`) → logical-or → logical-and → bitwise-or → bitwise-xor → bitwise-and → equality → relational → shift → term → factor → unary → primary. `Parser` has no lifetime parameter; parse state is held in a private `ParseState<'a, 'p>` created for each call. Public API:
  ```rust
  pub type Mapper = Box<dyn Fn(&str) -> Option<Operand>>;
  Parser::from(map_register, map_flag, map_symbol)  // accepts any Fn, boxed internally
  parser.parse(source: &'a str, vars: &mut Variables) -> Result<Option<Expr<'a>>, Error>
  ```
  Symbol resolution order: register mappers → symbol mappers → variables. Walrus LHS allocates or reuses a variable ID via `vars.get_or_create`.

- **`compiler`** — depth-first traversal of `Expr` tree, emitting a flat `Vec<OpCode>`. Signedness from the AST determines which opcode variant is emitted (e.g. `Divide` vs `DivideSigned`). Entry point: `compile(root: Expr) -> Vec<OpCode>`.

- **`evaluator`** — stack-based VM executing `&[OpCode]` against a `&dyn WatchContext` and a `&mut [Operand]` variable storage slice. Entry point: `eval(code: &[OpCode], context: &dyn WatchContext, vars: &mut [Operand]) -> Result<Operand, WatchError>`. Returns errors for division by zero.

- **`context`** — defines the `WatchContext` trait, which the emulator implements to give the evaluator access to machine state: `read_register_u32`/`_i32`, `read_flag`, `read_mem_u32`/`_i32`.

- **`session`** — high-level API over the full pipeline. `WatchCompiler` owns a `Parser`; `WatchEvaluator` owns watchpoints, `Variables`, and variable runtime storage. Public API:
  ```rust
  WatchCompiler::new(map_register, map_flag, map_symbol) -> WatchCompiler
  compiler.compile(source: &str, evaluator: &mut WatchEvaluator) -> Result<Watchpoint, Error>
  compiler.compile_all(source: &str, evaluator: &mut WatchEvaluator) -> (Vec<Watchpoint>, Vec<Error>)
  WatchEvaluator::new() -> WatchEvaluator
  evaluator.add(watchpoint: Watchpoint) -> usize
  evaluator.remove(index: usize) -> Watchpoint
  evaluator.clear()
  evaluator.watchpoints() -> &[Watchpoint]
  evaluator.evaluate_all(context: &dyn WatchContext) -> Result<Option<usize>, (usize, WatchError)>
  evaluator.variables() -> &[Operand]
  evaluator.set_variable(id: usize, value: Operand)
  watchpoint.source() -> &str
  ```
  `evaluate_all` evaluates watchpoints in order, returning `Ok(Some(index))` on first non-zero result, `Ok(None)` if none triggered, or `Err((index, error))` on failure.

- **`error`** / **`location`** — `Error` and `Location` structs carrying line/column for error reporting.

### Domain-specific operators

- **Memory read**: `B[addr]`, `W[addr]`, `D[addr]`, `b[addr]`, `w[addr]`, `d[addr]` — byte/word/dword fetch. Uppercase and lowercase are equivalent; signedness is controlled by a leading unary `+` or `-` (e.g. `+b[addr]` fetches a signed byte).
- **Flag read**: `` `flagname `` — reads a named CPU status flag.
- **Walrus** (`:=`) — assigns the RHS expression to a named variable and yields its value. Variables persist across `evaluate_all` calls via the evaluator's internal storage. Useful for snapshotting state (e.g. `prev_a := A`) to detect changes across steps.
- **`$hex`** — hexadecimal literal shorthand common in 6502 assembly.

### Signedness

The `signed` field on `Expr` nodes tracks whether results are signed. Unary `-`/`+` mark expressions signed; binary operators propagate signedness from operands. The compiler uses this to emit signed vs. unsigned opcode variants; the evaluator casts to `i32` for signed operations.

### Lifetime threading

`Token<'a>` borrows its text from the source `&'a str`. `Expr<'a>` carries `Token<'a>` values, so the source string must outlive the expression tree. `Parser` itself has no lifetime parameter — per-call parse state lives in a local `ParseState<'a, 'p>` that is dropped when `parse()` returns. After `compiler::compile` consumes the `Expr<'a>` tree, the resulting `Vec<OpCode>` has no lifetime parameters and can be stored freely (as `Watchpoint` does).
