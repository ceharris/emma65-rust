# Running the Tracer

`emma65-tracer` decodes a binary trace file — recorded via `emma65
--trace-file <path>` or the debugger's Trace window — into a human-readable,
disassembled instruction listing.

```
emma65-tracer [--output <path>] [--symbol-file <path>]... [--verbose] [<input>]
```

- `<input>` — path to the trace file; reads from stdin if omitted
- `--output <path>` — path to write decoded output; writes to stdout if omitted
- `--symbol-file <path>` — a VICE-format label file to resolve addresses to
  symbol names; may be repeated to load labels from multiple files
- `--verbose` — additionally print the bus reads and writes performed by each
  instruction
