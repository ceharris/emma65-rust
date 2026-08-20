# Plan: SymbolTable multi-source redesign

Tracked by issue #490.

## Context

This work grew out of the issue #474 assembler-debugger integration
(`doc/assembler-debugger-integration-plan.md`), which named a known,
accepted limitation and a "deferred mitigation" for it: `assemble_and_load`
merges assembled symbols into the bus's `SymbolTable` additively (no
`clear()`, to preserve ROM-loaded labels), but `SymbolTable::insert` never
evicts a name's old `by_address` entry when the name moves to a new
address. Editing a label's address in the Assembler panel and re-assembling
therefore leaves a stale ghost/duplicate entry in the Memory/Disassembly
symbol gutter. The named mitigation (track names contributed by the panel's
last assemble, `remove()` them before the next merge) was never actually
implemented — `debugger/src-tauri/src/assembler.rs:82` today is a bare
`bus.symbol_table_mut().insert_from(&program.symbols)`, so this bug is live.

Discussing the fix surfaced a broader design gap: `SymbolTable` conflates
three genuinely distinct sources of symbols with no way to distinguish or
selectively replace them:

- **File** — VICE `.lbl` labels, loaded either at bus-configuration time
  (`finch.rs`/`vireo.rs`/`phoebe.rs`/`memory.rs` device config modules, via
  `symbol::load_vice_labels`) or via the debugger's Load Memory UI
  (`debugger/src-tauri/src/memory.rs`) — the same code path either way.
- **Assembler** — produced by `emma65::assembler::assemble()`.
- **User** — symbols explicitly defined by a person, via a UI interaction
  that does not exist yet. Symbols hand-inserted by disassembler unit tests
  (`src/disassembler/trace.rs` and friends) are the "headless" case of this
  same source — test code standing in for a person defining a symbol
  directly.

This plan redesigns `SymbolTable` (`src/emulator/bus/symbol.rs`) around an
explicit `SymbolSource` tag per entry, fixes the ghost-duplicate bug at its
root as a consequence of the new data model (not a bolted-on workaround),
and wires the two existing callers (`memory.rs`, `debugger/src-tauri/src/assembler.rs`)
to use it correctly. It does **not** build any user-facing "define a
symbol" UI or Tauri command — that's future scope, laid on top of the
`User` source this plan makes fully supported at the library level.

## Decisions locked in (do not revisit mid-implementation)

- **Data model**: replace the current `Vec<Option<Symbol>>` +
  `by_name: HashMap<String, usize>` + `by_address: HashMap<u16, Vec<usize>>`
  tombstoning scheme entirely with:

  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Hash)]
  pub enum SymbolSource {
      File(PathBuf),   // canonicalized, absolute
      Assembler,
      User,
  }

  pub struct SymbolTable {
      by_name: HashMap<String, Vec<(SymbolSource, u16)>>,
      by_address: HashMap<u16, Vec<(String, SymbolSource)>>,
  }
  ```

  A name has at most one live entry per source (a `Vec` of at most a
  handful of `(SymbolSource, u16)` pairs — practically almost always 1).
  Re-inserting the same `(name, source)` pair updates that entry in place
  and relocates its `by_address` reverse pointer; there is no tombstoning
  and no `Symbol` struct — `by_name` is genuine storage, `by_address` is a
  pure reverse index kept in sync on every insert/remove. This is what
  fixes the ghost-duplicate bug: a name moving to a new address is a real
  in-place update of its one entry for that source, not an append.

- **Resolution precedence**: `User` > `Assembler` > `File(_)`.
  `address_for(name)` returns the highest-precedence live entry. Ties
  within the same tier (e.g. two different label files both defining the
  same name — only possible for `File`, since a name has at most one
  `Assembler` and one `User` entry) resolve arbitrarily but deterministically
  (whichever entry is encountered first in the per-name `Vec`) — this is a
  documented, unengineered edge case, same precedent as the assembler's
  `(Nop, Implied)` opcode-collision handling in `instructions.rs`. Do not
  add a tie-break policy for this.

- **`names_for(address)` shows every source, no shadowing.** If two
  different sources both have a live entry at the same address (whether or
  not they share a name), all of them are surfaced — `names_for` answers
  "what symbols live at this address", not "what does resolution pick".
  Only `address_for` applies precedence.

- **Backward-compatible `insert`/`remove` signatures, defaulting to
  `User`.** `insert(name, address)` delegates to a new
  `insert_tagged(name, address, SymbolSource::User)`; `remove(name)`
  delegates to a new `remove_tagged(name, &SymbolSource::User)`. This is
  not just churn-avoidance — it's the semantically correct default, since
  every existing direct-`insert`/`remove` call site (disassembler unit
  tests, `bus/mod.rs`'s own test) is exactly the "headless user-defined"
  case described above. **No existing test call site needs to change.**
  New method name is `insert_tagged`/`remove_tagged`, not
  `insert_from_source` — `insert_from(&SymbolTable)` already exists for a
  different purpose (merging another table) and a `insert_from_*` name
  would be easy to misread as related.

- **`clear_source(&SymbolSource)`** replaces the never-built "deferred
  mitigation" bookkeeping with a real table-level primitive: strips every
  live entry tagged with the given source (exact match — for `File`, the
  exact canonicalized path). `clear()` (full wipe, any source) stays as-is
  for callers that genuinely want to discard everything.

- **`insert_from(&mut self, other: &SymbolTable)`** keeps its existing
  signature and copies every `(name, source, address)` triple from `other`,
  preserving each entry's own tag (via `insert_tagged` internally) — no
  override parameter needed, since the tables passed to it
  (`load_vice_labels`'s result, `AssembledProgram.symbols`) are already
  correctly tagged at construction.

- **`load_vice_labels` canonicalizes its path** (`tokio::fs::canonicalize`,
  composing with `ExpandedPathBuf`'s existing `~`-expansion upstream in the
  config layer) before tagging every parsed entry `File(canonical_path)`.
  `parse_vice_labels` (the sync, directly-unit-tested inner function) gains
  a `source: SymbolSource` parameter rather than doing its own I/O — keeps
  it filesystem-free and easy to test with inline strings, same as today.

- **New enumeration API**, foundation for a future Symbols panel / VICE
  export (neither built now):
  - `file_sources(&self) -> impl Iterator<Item = &Path>` — unique
    canonicalized paths currently contributing at least one live symbol.
  - `has_file_source(&self, path: &Path) -> bool`.
  - `iter(&self) -> impl Iterator<Item = (&str, &SymbolSource, u16)>` — full
    table enumeration.
  These are query-only. **Do not** wire double-load prevention into
  `memory.rs` or the device-config loaders in this plan — that's a future
  caller decision once a UI exists to act on it.

- **No `by_source` reverse index.** `clear_source`/`file_sources`/`iter`
  scan `by_name` directly. Table sizes are small (a full ROM's worth of
  labels is at most a few thousand entries) and these operations are
  user-triggered (Load Memory, Assemble & Load), not hot-path — an extra
  index here is one more invariant to keep in sync for no measured benefit.

## Verified findings (traced, not assumed)

- `src/emulator/bus/symbol.rs:33-42`: `insert()`'s actual defect is
  narrower than "never evicts anything" — `by_address`'s stale `usize`
  indices are already harmless today, since every reader
  (`names_for`/`address_for`) filters through `symbols[idx].as_ref()`. The
  real gap: when a name moves to a new address, `insert()` pushes a new
  `Symbol` and repoints `by_name`, but the *old* slot in `symbols` is never
  set to `None` — so `names_for(old_address)` keeps yielding it forever.
- `debugger/src-tauri/src/assembler.rs:82`: confirmed the "deferred
  mitigation" named in `doc/assembler-debugger-integration-plan.md` was
  never implemented — this line is a bare `insert_from`, no tracking, no
  `remove()`. The bug is live today, not just theoretical.
- `debugger/src-tauri/src/memory.rs:147-151`: `load_memory`'s existing
  `clear()` + `insert_from` (full wipe on every reload) is the pattern
  `clear_source(File(path))` replaces — confirmed no other caller relies on
  a full-table wipe from this code path.
- `src/assembler/mod.rs:40`: `symbols.insert(name, address as u16)` is the
  one production call site that must change to
  `insert_tagged(name, address as u16, SymbolSource::Assembler)` — every
  other production `insert`/`remove` call in the crate either goes through
  `insert_from` (tag-preserving, no change needed) or is a test.
- `src/emulator/bus/mod.rs:683-690` (`symbol_table_inserts_from_source`
  test) and all of `src/emulator/bus/symbol.rs`'s existing unit tests use
  the 2-arg `insert`/1-arg `remove` — confirmed these compile and pass
  unchanged under the new default-to-`User` delegation.
- `src/emulator/config/path.rs:11-20`: `ExpandedPathBuf` only does
  leading-`~/` expansion, not full canonicalization — `load_vice_labels`
  canonicalizing on top of whatever path it's handed (already
  `~`-expanded by the config layer, or a raw absolute path from the
  debugger's file picker) is a real, additional step, not redundant with
  existing behavior.
- No other call site in the codebase constructs a `SymbolSource` or calls
  `insert_tagged`/`clear_source` today (grepped `src/`, `debugger/src-tauri/src/`)
  — confirmed `Assembler` (Unit 1) and both debugger callers (Unit 2) are
  the complete set of production sites needing changes.

## Work Units

### Unit 1 — Core `SymbolTable` redesign (library)

Files: `src/emulator/bus/symbol.rs`, `src/emulator/mod.rs` (re-export
`SymbolSource` alongside `SymbolTable`), `src/assembler/mod.rs:40`.

- Replace the data model and every method per the "Decisions locked in"
  section above: `insert`/`insert_tagged`, `remove`/`remove_tagged`,
  `clear`/`clear_source`, `insert_from`, `address_for` (precedence-resolved),
  `names_for` (unchanged signature/semantics beyond no-shadowing, which was
  already true — just now also correctly excludes moved-away entries),
  `len`/`is_empty` (same external contract: count of live entries; drop the
  doc comment explaining tombstoning, since there isn't any anymore), new
  `file_sources`/`has_file_source`/`iter`.
- `parse_vice_labels(contents: &str, source: SymbolSource) -> Result<SymbolTable, &'static str>`
  gains the `source` parameter; `load_vice_labels` canonicalizes the given
  path and passes `SymbolSource::File(canonical)` through.
- Update `src/assembler/mod.rs:40` to `insert_tagged(name, address as u16, SymbolSource::Assembler)`.
- Existing unit tests in `symbol.rs` keep their current `insert(name, addr)`/
  `remove(name)` call sites unchanged (still valid via the `User`-default
  delegation) — only their assertions/doc comments need adjusting where
  they referenced tombstoning internals that no longer exist.
- New tests:
  - **Ghost-duplicate regression** (the actual bug this plan fixes):
    `insert("foo", 0xDEAD)` then `insert("foo", 0xBEEF)` (same source,
    moved address) → `names_for(0xDEAD)` no longer contains `"foo"`.
  - Multi-source coexistence: `insert_tagged("foo", 0x1000, File(a))` +
    `insert_tagged("foo", 0x2000, Assembler)` → `address_for("foo") ==
    Some(0x2000)` (Assembler beats File), `names_for(0x1000)` **and**
    `names_for(0x2000)` both contain `"foo"` (no shadowing).
  - Full precedence: add a `User` entry for the same name at a third
    address, confirm it now wins `address_for`.
  - `clear_source` removes only the targeted source's entries; other
    sources for the same and different names survive untouched.
  - `file_sources`/`has_file_source`: two different `File(path)`-tagged
    entries → both unique paths enumerated; `clear_source(File(a))` drops
    `has_file_source(a)` to `false` while `has_file_source(b)` stays `true`.
  - `insert_from` preserves each copied entry's original tag (build a
    table with mixed sources, merge into a fresh table, confirm precedence
    output matches).
  - `load_vice_labels`/`parse_vice_labels`: loading a real tempfile tags
    every entry with the tempfile's canonicalized path (compare against
    `tokio::fs::canonicalize` of the same path directly, not just "some
    path").
- `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`
  clean.

### Unit 2 — Wire the debugger's two callers (bug fix lands here)

Files: `debugger/src-tauri/src/memory.rs`, `debugger/src-tauri/src/assembler.rs`.

- `assembler.rs`'s `assemble_and_patch`: replace the bare
  `bus.symbol_table_mut().insert_from(&program.symbols)` with
  `bus.symbol_table_mut().clear_source(&SymbolSource::Assembler)` followed
  by `insert_from`. This is the actual fix for the live ghost-duplicate bug
  — a re-assemble now fully replaces the previous assemble's symbols
  (whatever their addresses were) before merging the new ones, while ROM
  (`File`) and any future `User` symbols are untouched.
- `memory.rs`'s `load_memory`: replace `bus_table.clear(); bus_table.insert_from(table);`
  with a loop over the freshly-loaded table's own `file_sources()`
  (there should be exactly one — the file just loaded) calling
  `bus_table.clear_source(&SymbolSource::File(path.to_path_buf()))` for
  each, then `insert_from(table)`. This means reloading the *same* labels
  file replaces its own prior contribution cleanly, while a *different*
  labels file's previously-loaded labels, any assembled symbols, and any
  future user-defined symbols all survive — a behavior change from today's
  blanket `clear()`, and the intended one.
- New/updated tests:
  - `assembler.rs`: reassemble-after-label-move regression — assemble
    source defining `START` at one address, then assemble a modified
    source with `START` moved elsewhere; confirm `names_for(old_address)`
    no longer contains `"START"` and a pre-existing `File`-sourced symbol
    (inserted directly on the test's bus before either assemble) survives
    both.
  - `memory.rs`: loading the same labels file twice (second load with
    different label content, e.g. via a mutated tempfile) leaves no ghost
    from the first load; loading two different labels files back-to-back
    leaves both contributions intact; a symbol from a prior
    `SymbolSource::Assembler`/`SymbolSource::User` entry survives a
    `load_memory` call untouched.
- `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings` clean. No
  frontend changes — this unit is backend-only, no UAT needed beyond CI.

## Workflow

Two sequential units, one branch + PR each, following this repo's
established per-unit workflow (`feedback_issue_462_workflow`): create the
unit's branch, implement, verify per that unit's checklist above, commit,
push, open a PR referencing issue #490, then **stop and await explicit
instruction** before starting the next unit. Do not batch both units into
one PR.

This plan doc should be committed to `main` before Unit 1 starts, mirroring
`doc/assembler-plan.md` and `doc/assembler-debugger-integration-plan.md`.
