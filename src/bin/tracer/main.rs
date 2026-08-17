//! `emma65-tracer`: decodes a binary trace file produced by the `emma65`
//! emulator into a human-readable disassembly listing.

mod format;

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::process::ExitCode;

use clap::Parser;
use emma65::disassembler::{TraceBusOp, TraceRow, TraceRowAssembler};
use emma65::emulator::bus::symbol::load_vice_labels;
use emma65::emulator::{BinaryTraceReader, ExpandedPathBuf, SymbolTable};

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

/// Streams `reader`'s records through a [`TraceRowAssembler`], writing one
/// formatted row per reconstructed instruction to `out`.
fn run(
    reader: BinaryTraceReader<Box<dyn Read>>,
    symbols: SymbolTable,
    verbose: bool,
    out: &mut dyn Write,
) -> io::Result<()> {
    let mut assembler = TraceRowAssembler::new(reader.variant(), symbols);

    writeln!(out, "{}", format::HEADER)?;
    writeln!(out, "{}", format::SEPARATOR)?;

    for item in reader {
        let rec = item?;
        if let Some(row) = assembler.feed(&rec) {
            emit(row, verbose, out)?;
        }
    }

    // Trace file truncated mid-instruction: emit whatever was reconstructed, best-effort.
    if let Some(row) = assembler.flush() {
        emit(row, verbose, out)?;
    }

    out.flush()
}

/// Writes one instruction's labels, its formatted row, and (in verbose mode)
/// its non-fetch bus operations.
fn emit(row: TraceRow, verbose: bool, out: &mut dyn Write) -> io::Result<()> {
    for label in &row.line.labels {
        writeln!(out, "{}", format::label_line(label))?;
    }
    writeln!(out, "{}", format::instruction_row(row.instr_id + 1, row.cycles, &row.regs, &row.line))?;

    if verbose {
        for op in row.non_fetch_bus_ops() {
            match *op {
                TraceBusOp::Read { addr, value } => {
                    writeln!(out, "{}", format::bus_op_row(addr, "RD", value))?;
                }
                TraceBusOp::Write { addr, value } => {
                    writeln!(out, "{}", format::bus_op_row(addr, "WR", value))?;
                }
            }
        }
    }

    Ok(())
}
