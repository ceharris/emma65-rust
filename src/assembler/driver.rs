//! Multi-pass assembly driver: lays out statements across one or more
//! `.org`-delimited segments, resolves forward references, shrinks
//! zero-page-eligible operands, and reports final errors (undefined
//! symbols, segment overlaps, duplicate labels, non-convergence).
//!
//! See the "Multi-pass resolution and zero-page optimization" and
//! "`.org` / multiple segments" sections of `doc/assembler-plan.md` for the
//! algorithm this implements.

use std::collections::HashMap;

use super::error::Error;
use super::eval::evaluate;
use super::instructions::InstructionTable;
use super::operand::encode;
use super::statement::{ByteOperand, Directive, Statement, parse_program};
use crate::emulator::cpu::variant::CpuVariant;
use crate::location::Location;

/// A generous but finite cap on the number of layout passes, guarding
/// against a genuine non-convergence (see the plan doc) rather than looping
/// forever. Ordinary programs converge in two or three passes.
const DEFAULT_MAX_PASSES: usize = 25;

/// One contiguous output region, starting at `origin`.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub origin: u16,
    pub bytes: Vec<u8>,
}

impl Segment {
    fn address(&self) -> u16 {
        self.origin.wrapping_add(self.bytes.len() as u16)
    }
}

/// The driver's result: assembled segments in `.org` order, plus the final
/// symbol table (label and `=`/`.equ` values). A later unit adapts this into
/// the public `assemble()` API's `AssembledProgram`.
#[derive(Debug, Clone, PartialEq)]
pub struct AssembledOutput {
    pub segments: Vec<Segment>,
    pub symbols: HashMap<String, u32>,
}

/// Assembles `source` for `variant`, collecting all errors found rather than
/// stopping at the first one.
pub fn assemble(source: &str, variant: CpuVariant) -> Result<AssembledOutput, Vec<Error>> {
    assemble_with_pass_limit(source, variant, DEFAULT_MAX_PASSES)
}

fn assemble_with_pass_limit(
    source: &str,
    variant: CpuVariant,
    max_passes: usize,
) -> Result<AssembledOutput, Vec<Error>> {
    let table = InstructionTable::new(variant);
    let statements = parse_program(source, &table).map_err(|e| vec![e])?;
    check_duplicate_symbols(&statements)?;

    let mut symbols: HashMap<String, u32> = HashMap::new();
    let mut sizes: Vec<u8> = vec![0; statements.len()];
    let mut converged = false;
    for _ in 0..max_passes {
        let previous_sizes = sizes.clone();
        run_pass(&statements, &mut symbols, &mut sizes, &table, false)?;
        if sizes == previous_sizes {
            converged = true;
            break;
        }
    }
    if !converged {
        return Err(vec![Error::from(
            0,
            0,
            "assembly did not converge after too many passes (possible oscillating forward references)",
        )]);
    }

    let segments = run_pass(&statements, &mut symbols, &mut sizes, &table, true)?;
    check_overlaps(&segments)?;

    Ok(AssembledOutput { segments, symbols })
}

/// Detects two symbol-defining statements (a `Label` or `SymbolAssign`)
/// using the same name. This is a purely syntactic check, independent of
/// address resolution, so it runs once up front rather than per pass.
fn check_duplicate_symbols(statements: &[(Statement, Location)]) -> Result<(), Vec<Error>> {
    let mut seen: HashMap<&str, Location> = HashMap::new();
    let mut errors = Vec::new();
    for (statement, location) in statements {
        let name = match statement {
            Statement::Label(name) => Some(name.as_str()),
            Statement::SymbolAssign(name, _) => Some(name.as_str()),
            _ => None,
        };
        if let Some(name) = name {
            if seen.contains_key(name) {
                errors.push(Error::from(
                    location.line,
                    location.column,
                    &format!("symbol '{name}' is already defined"),
                ));
            } else {
                seen.insert(name, *location);
            }
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

/// Detects segment address ranges that overlap, or two segments sharing the
/// same origin even when neither has emitted any bytes yet.
fn check_overlaps(segments: &[Segment]) -> Result<(), Vec<Error>> {
    let mut errors = Vec::new();
    for i in 0..segments.len() {
        for j in (i + 1)..segments.len() {
            let a = &segments[i];
            let b = &segments[j];
            let a_start = a.origin as u32;
            let a_end = a_start + a.bytes.len() as u32;
            let b_start = b.origin as u32;
            let b_end = b_start + b.bytes.len() as u32;
            if a_start == b_start || (a_start < b_end && b_start < a_end) {
                errors.push(Error::from(
                    0,
                    0,
                    &format!("segment at ${a_start:04X} overlaps segment at ${b_start:04X}"),
                ));
            }
        }
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

/// Walks every statement once, in order, updating `symbols` and `sizes` in
/// place and returning the resulting segments. On a non-final pass an
/// operand that isn't yet resolvable is tolerated (filled with a zero
/// placeholder so layout can continue); on the final pass it becomes a real
/// `Error`. Structural errors (a byte-emitting statement before any `.org`,
/// an `.org`/`.res` address that depends on a forward reference) are always
/// real errors, since more passes can never fix them.
fn run_pass<'a>(
    statements: &[(Statement<'a>, Location)],
    symbols: &mut HashMap<String, u32>,
    sizes: &mut [u8],
    table: &InstructionTable,
    final_pass: bool,
) -> Result<Vec<Segment>, Vec<Error>> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut current: Option<usize> = None;
    let mut errors: Vec<Error> = Vec::new();

    for (i, (statement, location)) in statements.iter().enumerate() {
        let location = *location;
        match statement {
            Statement::Label(name) => match current {
                Some(seg) => {
                    let addr = segments[seg].address();
                    symbols.insert(name.clone(), addr as u32);
                }
                None => errors.push(no_active_segment_error(location, "label")),
            },
            Statement::SymbolAssign(name, expr) => match evaluate(expr, symbols) {
                Ok(Some(value)) => {
                    symbols.insert(name.clone(), value);
                }
                Ok(None) => {
                    if final_pass {
                        errors.push(undefined_symbol_error(location));
                    }
                }
                Err(e) => errors.push(e),
            },
            Statement::Directive(Directive::Org(expr)) => match evaluate(expr, symbols) {
                Ok(Some(value)) => {
                    segments.push(Segment { origin: value as u16, bytes: Vec::new() });
                    current = Some(segments.len() - 1);
                }
                Ok(None) => errors.push(Error::from(
                    location.line,
                    location.column,
                    "'.org' address must not depend on a forward reference",
                )),
                Err(e) => errors.push(e),
            },
            Statement::Directive(Directive::Byte(operands)) => match current {
                None => errors.push(no_active_segment_error(location, "'.byte'")),
                Some(seg) => {
                    for operand in operands {
                        match operand {
                            ByteOperand::String(s) => segments[seg].bytes.extend(s.bytes()),
                            ByteOperand::Expr(expr) => match evaluate(expr, symbols) {
                                Ok(Some(value)) => {
                                    if value > 0xFF {
                                        errors.push(Error::from(
                                            location.line,
                                            location.column,
                                            "'.byte' operand out of range",
                                        ));
                                    }
                                    segments[seg].bytes.push(value as u8);
                                }
                                Ok(None) => {
                                    if final_pass {
                                        errors.push(undefined_symbol_error(location));
                                    }
                                    segments[seg].bytes.push(0);
                                }
                                Err(e) => errors.push(e),
                            },
                        }
                    }
                }
            },
            Statement::Directive(Directive::Word(exprs)) => match current {
                None => errors.push(no_active_segment_error(location, "'.word'")),
                Some(seg) => {
                    for expr in exprs {
                        match evaluate(expr, symbols) {
                            Ok(Some(value)) => segments[seg].bytes.extend((value as u16).to_le_bytes()),
                            Ok(None) => {
                                if final_pass {
                                    errors.push(undefined_symbol_error(location));
                                }
                                segments[seg].bytes.extend([0, 0]);
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                }
            },
            Statement::Directive(Directive::Res(expr)) => match current {
                None => errors.push(no_active_segment_error(location, "'.res'")),
                Some(seg) => match evaluate(expr, symbols) {
                    Ok(Some(value)) => {
                        let new_len = segments[seg].bytes.len() + value as usize;
                        segments[seg].bytes.resize(new_len, 0);
                    }
                    Ok(None) => errors.push(Error::from(
                        location.line,
                        location.column,
                        "'.res' size must not depend on a forward reference",
                    )),
                    Err(e) => errors.push(e),
                },
            },
            Statement::Instruction(mnemonic, operand) => match current {
                None => errors.push(no_active_segment_error(location, "instruction")),
                Some(seg) => {
                    let addr = segments[seg].address();
                    match encode(*mnemonic, operand, addr, symbols, table, location) {
                        Ok(encoded) => {
                            sizes[i] = encoded.byte_len;
                            match encoded.bytes {
                                Some(bytes) => segments[seg].bytes.extend(bytes),
                                None => {
                                    if final_pass {
                                        errors.push(undefined_symbol_error(location));
                                    }
                                    let new_len = segments[seg].bytes.len() + encoded.byte_len as usize;
                                    segments[seg].bytes.resize(new_len, 0);
                                }
                            }
                        }
                        Err(e) => errors.push(e),
                    }
                }
            },
        }
    }

    if errors.is_empty() { Ok(segments) } else { Err(errors) }
}

fn undefined_symbol_error(location: Location) -> Error {
    Error::from(location.line, location.column, "undefined symbol")
}

fn no_active_segment_error(location: Location, what: &str) -> Error {
    Error::from(location.line, location.column, &format!("{what} used before any '.org' directive"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assemble_ok(source: &str) -> AssembledOutput {
        assemble(source, CpuVariant::Wdc65C02).unwrap_or_else(|errors| {
            panic!("expected assembly to succeed, got errors: {errors:?}");
        })
    }

    fn assemble_err(source: &str) -> Vec<Error> {
        assemble(source, CpuVariant::Wdc65C02).expect_err("expected assembly to fail")
    }

    #[test]
    fn assemble_simple_program() {
        let output = assemble_ok(".org $8000\nLDA #$01\nSTA $10\n");
        assert_eq!(output.segments.len(), 1);
        assert_eq!(output.segments[0].origin, 0x8000);
        assert_eq!(output.segments[0].bytes, vec![0xA9, 0x01, 0x85, 0x10]);
    }

    #[test]
    fn assemble_backward_label_reference() {
        let output = assemble_ok(".org $8000\nloop:\nLDA #1\nBNE loop\n");
        // BNE at 0x8002, 2-byte instruction -> pc_after 0x8004, target 0x8000 -> displacement -4
        assert_eq!(output.segments[0].bytes, vec![0xA9, 0x01, 0xD0, 0xFC]);
        assert_eq!(output.symbols.get("loop"), Some(&0x8000));
    }

    #[test]
    fn assemble_forward_reference_shrinks_to_zero_page() {
        // "forward" isn't known until after LDA references it; the driver
        // must discover across passes that it resolves to a zero-page value
        // and shrink the instruction from Absolute down to ZeroPage.
        let output = assemble_ok(".org $0200\nLDA forward\nforward = $10\n");
        assert_eq!(output.segments[0].bytes, vec![0xA5, 0x10]);
    }

    #[test]
    fn assemble_forward_label_reference_in_word_directive() {
        let output = assemble_ok(".org $8000\n.word target\nNOP\ntarget:\nNOP\n");
        // target = $8000 (segment origin) + 2 (.word) + 1 (first NOP) = $8003
        assert_eq!(&output.segments[0].bytes[0..2], &[0x03, 0x80]);
        assert_eq!(output.symbols.get("target"), Some(&0x8003));
    }

    #[test]
    fn assemble_byte_directive_with_mixed_operands() {
        let output = assemble_ok(".org $8000\n.byte 1, \"AB\", 2\n");
        assert_eq!(output.segments[0].bytes, vec![1, b'A', b'B', 2]);
    }

    #[test]
    fn assemble_word_directive() {
        let output = assemble_ok(".org $8000\n.word $1234, $5678\n");
        assert_eq!(output.segments[0].bytes, vec![0x34, 0x12, 0x78, 0x56]);
    }

    #[test]
    fn assemble_res_directive_reserves_zero_bytes_and_advances_address() {
        let output = assemble_ok(".org $8000\n.res 4\nhere:\nNOP\n");
        assert_eq!(output.segments[0].bytes, vec![0, 0, 0, 0, 0xEA]);
        assert_eq!(output.symbols.get("here"), Some(&0x8004));
    }

    #[test]
    fn assemble_multiple_non_overlapping_segments() {
        let output = assemble_ok(".org $8000\nLDA #1\n.org $9000\nLDA #2\n");
        assert_eq!(output.segments.len(), 2);
        assert_eq!(output.segments[0], Segment { origin: 0x8000, bytes: vec![0xA9, 0x01] });
        assert_eq!(output.segments[1], Segment { origin: 0x9000, bytes: vec![0xA9, 0x02] });
    }

    #[test]
    fn assemble_overlapping_segments_error() {
        let errors = assemble_err(".org $8000\n.byte 1,2,3,4,5\n.org $8003\nLDA #1\n");
        assert!(errors.iter().any(|e| e.message().contains("overlaps")));
    }

    #[test]
    fn assemble_duplicate_org_address_error() {
        let errors = assemble_err(".org $8000\n.org $8000\n");
        assert!(errors.iter().any(|e| e.message().contains("overlaps")));
    }

    #[test]
    fn assemble_byte_before_org_error() {
        let errors = assemble_err(".byte 1\n");
        assert!(errors.iter().any(|e| e.message().contains("before any '.org'")));
    }

    #[test]
    fn assemble_duplicate_label_error() {
        let errors = assemble_err(".org $8000\nfoo:\nfoo:\n");
        assert!(errors.iter().any(|e| e.message().contains("already defined")));
    }

    #[test]
    fn assemble_undefined_symbol_error() {
        let errors = assemble_err(".org $8000\nLDA missing\n");
        assert!(errors.iter().any(|e| e.message().contains("undefined symbol")));
    }

    #[test]
    fn assemble_res_forward_reference_error() {
        let errors = assemble_err(".org $8000\n.res forward\nforward = 4\n");
        assert!(errors.iter().any(|e| e.message().contains("forward reference")));
    }

    #[test]
    fn assemble_org_forward_reference_error() {
        let errors = assemble_err(".org forward\nforward = $8000\n");
        assert!(errors.iter().any(|e| e.message().contains("forward reference")));
    }

    #[test]
    fn assemble_non_convergence_reported_when_pass_limit_too_low() {
        // This program needs two passes to shrink LDA's forward reference to
        // zero page; capping at one pass must surface as an error rather
        // than silently emitting the wrong (unconverged) bytes.
        let errors = assemble_with_pass_limit(
            ".org $0200\nLDA forward\nforward = $10\n",
            CpuVariant::Wdc65C02,
            1,
        )
        .expect_err("expected non-convergence error");
        assert!(errors.iter().any(|e| e.message().contains("did not converge")));
    }
}
