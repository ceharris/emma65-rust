//! `emma65-tracer`: decodes a binary trace file produced by the `emma65`
//! emulator into a human-readable disassembly listing.

mod format;

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::process::ExitCode;

use clap::Parser;
use emma65::emulator::bus::symbol::load_vice_labels;
use emma65::emulator::disasm::trace::TraceDisassembler;
use emma65::emulator::{BinaryTraceReader, DisassembledLine, ExpandedPathBuf, Registers, SymbolTable, TraceKind};

/// Command-line arguments for `emma65-tracer`.
#[derive(Parser)]
#[clap(name = "emma65-tracer")]
struct Args {
    /// Path to the trace file to read. Reads from stdin if omitted.
    input: Option<ExpandedPathBuf>,
    /// Path to write decoded output to. Writes to stdout if omitted.
    #[clap(long)]
    output: Option<ExpandedPathBuf>,
    /// Path to a VICE-format label file to load into the symbol table. May be repeated.
    #[clap(long = "symbol-file")]
    symbol_files: Vec<ExpandedPathBuf>,
    /// Include bus read/write details for each instruction.
    #[clap(long)]
    verbose: bool,
}

/// A bus operation observed while an instruction was executing.
enum BusOp {
    /// A bus read of the given value from the given address.
    Read(u16, u8),
    /// A bus write of the given value to the given address.
    Write(u16, u8),
}

/// The trace records collected so far for one in-progress instruction.
struct Pending {
    /// The instruction id shared by all records belonging to this instruction.
    instr_id: u64,
    /// The register snapshot taken immediately before this instruction executed.
    regs: Registers,
    /// Bus reads and writes observed for this instruction, in stream order.
    ops: Vec<BusOp>,
    /// The reconstructed disassembly, once its opcode/operand bytes are complete.
    line: Option<DisassembledLine>,
    /// The instruction's total cycle count, once known.
    cycles: Option<u8>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    let mut symbols = SymbolTable::new();
    for path in &args.symbol_files {
        match load_vice_labels(path).await {
            Ok(table) => symbols.insert_from(&table),
            Err(e) => {
                eprintln!("error: failed to load symbol file {path}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let input: Box<dyn Read> = match &args.input {
        Some(path) => match File::open(path) {
            Ok(f) => Box::new(f),
            Err(e) => {
                eprintln!("error: failed to open input file {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => Box::new(io::stdin()),
    };

    let reader = match BinaryTraceReader::new(input) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: failed to read trace file: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut output: Box<dyn Write> = match &args.output {
        Some(path) => match File::create(path) {
            Ok(f) => Box::new(BufWriter::new(f)),
            Err(e) => {
                eprintln!("error: failed to create output file {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => Box::new(BufWriter::new(io::stdout())),
    };

    if let Err(e) = run(reader, symbols, args.verbose, &mut *output) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

/// Streams `reader`'s records through a [`TraceDisassembler`], correlating
/// each instruction's `Registers`/`Read`/`Write`/`Cycles` records (which
/// arrive contiguously, tagged with a shared `instr_id`) and writing one
/// formatted row per instruction to `out`.
fn run(
    reader: BinaryTraceReader<Box<dyn Read>>,
    symbols: SymbolTable,
    verbose: bool,
    out: &mut dyn Write,
) -> io::Result<()> {
    let mut td = TraceDisassembler::new(reader.variant(), symbols);
    let mut pending: Option<Pending> = None;

    writeln!(out, "{}", format::HEADER)?;
    writeln!(out, "{}", format::SEPARATOR)?;

    for item in reader {
        let rec = item?;
        match rec.kind {
            TraceKind::Registers(regs) => {
                // A new instruction has begun; drop any unfinished prior
                // `Pending` (mirrors `TraceDisassembler`'s own behavior).
                pending = Some(Pending { instr_id: rec.instr_id, regs, ops: Vec::new(), line: None, cycles: None });
            }
            TraceKind::Read { addr, value } => {
                if let Some(p) = pending.as_mut()
                    && p.instr_id == rec.instr_id
                {
                    p.ops.push(BusOp::Read(addr, value));
                }
            }
            TraceKind::Write { addr, value } => {
                if let Some(p) = pending.as_mut()
                    && p.instr_id == rec.instr_id
                {
                    p.ops.push(BusOp::Write(addr, value));
                }
            }
            TraceKind::Cycles(cycles) => {
                if let Some(p) = pending.as_mut()
                    && p.instr_id == rec.instr_id
                {
                    p.cycles = Some(cycles);
                }
            }
        }

        if let Some(line) = td.feed(&rec)
            && let Some(p) = pending.as_mut()
            && p.instr_id == rec.instr_id
        {
            p.line = Some(line);
        }

        if matches!(rec.kind, TraceKind::Cycles(_)) && pending.as_ref().is_some_and(|p| p.instr_id == rec.instr_id) {
            emit(pending.take().unwrap(), verbose, out)?;
        }
    }

    // Trace file truncated mid-instruction: emit whatever was reconstructed, best-effort.
    if let Some(p) = pending.take() {
        emit(p, verbose, out)?;
    }

    out.flush()
}

/// Writes one instruction's labels, its formatted row, and (in verbose mode)
/// its non-fetch bus operations. Silently skipped if no disassembly was
/// ever reconstructed for it (a malformed or truncated trace).
fn emit(p: Pending, verbose: bool, out: &mut dyn Write) -> io::Result<()> {
    let Some(line) = p.line else { return Ok(()) };

    for label in &line.labels {
        writeln!(out, "{}", format::label_line(label))?;
    }
    writeln!(out, "{}", format::instruction_row(p.instr_id + 1, p.cycles, &p.regs, &line))?;

    if verbose {
        // Opcode/operand fetch reads are always the first N reads for an
        // instruction (N = the number of bytes the instruction decoded to);
        // everything else is a genuine bus op worth showing.
        let mut fetch_remaining = line.raw_bytes.len();
        for op in &p.ops {
            match *op {
                BusOp::Read(_, _) if fetch_remaining > 0 => {
                    fetch_remaining -= 1;
                }
                BusOp::Read(addr, value) => {
                    writeln!(out, "{}", format::bus_op_row(addr, "RD", value))?;
                }
                BusOp::Write(addr, value) => {
                    writeln!(out, "{}", format::bus_op_row(addr, "WR", value))?;
                }
            }
        }
    }

    Ok(())
}
