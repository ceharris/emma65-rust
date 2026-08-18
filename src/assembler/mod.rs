//! Support for assembling 6502 assembly language source into bytes.
//!
//! `assemble(source)` turns 6502 assembly source text into one or more
//! output segments (each an origin plus a byte vector) and a symbol table,
//! ready to be written into emulator memory (e.g. via `Bus::patch`) or
//! disassembled back for verification. See `doc/assembler-plan.md` for the
//! full design.

mod driver;
mod error;
mod eval;
mod expr;
mod instructions;
mod operand;
mod parser;
mod scanner;
mod statement;
mod token;

use crate::emulator::bus::SymbolTable;

pub use driver::Segment;
pub use error::Error;

/// The result of successfully assembling a source program: one or more
/// `.org`-delimited output segments, in source order, plus the final symbol
/// table (labels and `=`/`.equ` assignments).
#[derive(Debug, Clone)]
pub struct AssembledProgram {
    pub segments: Vec<Segment>,
    pub symbols: SymbolTable,
}

/// Assembles `source`, returning the resulting segments and symbol table, or
/// every error found (not just the first) if assembly fails.
pub fn assemble(source: &str) -> Result<AssembledProgram, Vec<Error>> {
    let output = driver::assemble(source)?;
    let mut symbols = SymbolTable::new();
    for (name, address) in output.symbols {
        symbols.insert(name, address as u16);
    }
    Ok(AssembledProgram { segments: output.segments, symbols })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disassembler::Disassembler;
    use crate::emulator::cpu::opcodes::Mnemonic;
    use crate::emulator::cpu::variant::CpuVariant;
    use crate::emulator::{AddressRange, Bus, RomWritePolicy};

    fn assemble_ok(source: &str) -> AssembledProgram {
        assemble(source).unwrap_or_else(|errors| {
            panic!("expected assembly to succeed, got errors: {errors:?}");
        })
    }

    /// Builds a `Bus` with a single RAM region covering `segment`'s bytes,
    /// patches them in, and attaches `symbols` — enough to hand to a
    /// `Disassembler` for a round-trip check.
    fn bus_for_segment(segment: &Segment, symbols: &SymbolTable) -> Bus {
        let start = segment.origin;
        let end = start.wrapping_add(segment.bytes.len().max(1) as u16 - 1);
        let mut bus = Bus::config()
            .rom_write_policy(RomWritePolicy::Ignore)
            .symbol_table(symbols)
            .ram(AddressRange::new(start, end))
            .expect("failed to configure RAM region")
            .build();
        for (i, byte) in segment.bytes.iter().enumerate() {
            bus.patch(start.wrapping_add(i as u16), *byte);
        }
        bus
    }

    #[test]
    fn assemble_end_to_end_program_with_forward_refs_and_symbol_arithmetic() {
        let source = "\
.org $8000
start:
  LDA #<message
  LDX #>message
  JSR print
  BNE done
  LDA offset + 1
done:
  RTS
print:
  RTS
offset = $10
message:
  .byte \"HI\", 0
";
        let program = assemble_ok(source);
        assert_eq!(program.segments.len(), 1);
        let segment = &program.segments[0];
        assert_eq!(segment.origin, 0x8000);

        let message_addr = program.symbols.address_for("message").unwrap();
        let print_addr = program.symbols.address_for("print").unwrap();
        let done_addr = program.symbols.address_for("done").unwrap();

        let disassembler = Disassembler::new(CpuVariant::Cmos65C02);
        let bus = bus_for_segment(segment, &program.symbols);
        let end = segment.origin.wrapping_add(segment.bytes.len() as u16);
        // Only the first 7 lines are real instructions; the rest of the
        // segment is the `.byte "HI", 0` data, which `disassemble_range`
        // (having no notion of code vs. data) will happily "decode" too.
        let lines = disassembler.disassemble_range(&bus, segment.origin, end, segment.bytes.len());
        let mnemonics: Vec<Mnemonic> = lines.iter().take(7).map(|l| l.mnemonic).collect();
        assert_eq!(
            mnemonics,
            vec![
                Mnemonic::Lda, // #<message
                Mnemonic::Ldx, // #>message
                Mnemonic::Jsr, // print
                Mnemonic::Bne, // done
                Mnemonic::Lda, // offset + 1, shrinks to zero page since offset resolves to $10
                Mnemonic::Rts, // done:
                Mnemonic::Rts, // print:
            ]
        );
        assert_eq!(lines[0].raw_bytes, vec![0xA9, (message_addr & 0xFF) as u8]);
        assert_eq!(lines[1].raw_bytes, vec![0xA2, (message_addr >> 8) as u8]);
        assert_eq!(lines[2].raw_bytes, vec![0x20, (print_addr & 0xFF) as u8, (print_addr >> 8) as u8]);
        assert_eq!(lines[4].raw_bytes, vec![0xA5, 0x11]);
        assert_eq!(lines[5].addr, done_addr);
        assert_eq!(&segment.bytes[segment.bytes.len() - 3..], b"HI\0");
    }

    #[test]
    fn assemble_multiple_org_segments_round_trip_through_disassembler() {
        let program = assemble_ok(".org $8000\nLDA #$01\nSTA $10\n.org $9000\nJMP $8000\n");
        assert_eq!(program.segments.len(), 2);

        let disassembler = Disassembler::new(CpuVariant::Cmos65C02);
        for segment in &program.segments {
            let bus = bus_for_segment(segment, &program.symbols);
            let lines = disassembler.disassemble_range(
                &bus,
                segment.origin,
                segment.origin.wrapping_add(segment.bytes.len() as u16),
                segment.bytes.len(),
            );
            let total_len: usize = lines.iter().map(|l| l.raw_bytes.len()).sum();
            assert_eq!(total_len, segment.bytes.len());
            let recombined: Vec<u8> = lines.iter().flat_map(|l| l.raw_bytes.clone()).collect();
            assert_eq!(&recombined, &segment.bytes);
        }

        assert_eq!(program.segments[0].bytes, vec![0xA9, 0x01, 0x85, 0x10]);
        assert_eq!(program.segments[1].bytes, vec![0x4C, 0x00, 0x80]);
    }

    #[test]
    fn assemble_all_four_directives_and_setcpu() {
        let program = assemble_ok(
            ".setcpu \"wdc65c02\"\n.org $8000\n.byte 1, \"AB\", 2\n.word $1234\n.res 2\nSTP\n",
        );
        let segment = &program.segments[0];
        assert_eq!(segment.bytes, vec![1, b'A', b'B', 2, 0x34, 0x12, 0, 0, 0xDB]);
    }

    #[test]
    fn assemble_lsb_msb_extraction_round_trips_against_known_address() {
        let program = assemble_ok(".org $8000\ntarget = $1234\nLDA #<target\nLDA #>target\n");
        let segment = &program.segments[0];
        assert_eq!(segment.bytes, vec![0xA9, 0x34, 0xA9, 0x12]);
    }

    #[test]
    fn assembled_program_symbols_are_queryable_by_name_and_address() {
        let program = assemble_ok(".org $8000\nstart:\nNOP\n");
        assert_eq!(program.symbols.address_for("start"), Some(0x8000));
        assert!(program.symbols.names_for(0x8000).any(|n| n == "start"));
    }

    #[test]
    fn assemble_collects_multiple_errors_rather_than_stopping_at_first() {
        let errors = assemble(".org $8000\nLDA missing1\nLDA missing2\n").unwrap_err();
        assert!(errors.len() >= 2, "expected at least 2 errors, got {errors:?}");
    }

    #[test]
    fn assemble_error_exposes_line_column_and_message() {
        let errors = assemble(".org $8000\nLDA missing\n").unwrap_err();
        let e = &errors[0];
        assert_eq!(e.line(), 2);
        assert!(e.message().contains("undefined symbol"));
    }

    #[test]
    fn assemble_full_addressing_mode_family_round_trip() {
        let source = "\
.org $0200
zp = $10
abs = $1234
LDA #$01
LDA zp
LDA zp,X
LDA abs
LDA abs,X
LDA abs,Y
LDA (zp,X)
LDA (zp),Y
LDA (zp)
";
        let program = assemble_ok(source);
        let segment = &program.segments[0];
        let disassembler = Disassembler::new(CpuVariant::Cmos65C02);
        let bus = bus_for_segment(segment, &program.symbols);
        let lines = disassembler.disassemble_range(
            &bus,
            segment.origin,
            segment.origin.wrapping_add(segment.bytes.len() as u16),
            32,
        );
        assert!(lines.iter().all(|l| l.mnemonic == Mnemonic::Lda && l.is_valid));
        assert_eq!(lines.len(), 9);
    }
}
