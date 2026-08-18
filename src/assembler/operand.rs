//! Addressing-mode operand syntax parsing and opcode encoding.
//!
//! Splits into two steps: [`parse_operand`] reads the syntax after a mnemonic
//! into an [`OperandSyntax`] (what the user wrote), and [`encode`] resolves it
//! against a symbol table into actual opcode + operand bytes (or `None` when
//! the operand isn't fully resolvable yet — see the module doc on the
//! multi-pass design in `doc/assembler-plan.md`).

use std::collections::HashMap;

use super::error::Error;
use super::eval::evaluate;
use super::expr::{Expr, Operand};
use super::instructions::InstructionTable;
use super::parser::Parser;
use super::token::TokenType;
use crate::emulator::cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic};
use crate::location::Location;

/// The addressing-mode syntax written after a mnemonic, before it has been
/// resolved to a specific [`AddressingMode`] (that happens in [`encode`],
/// since e.g. a bare `expr` might end up `ZeroPage`, `Absolute`, or
/// `Relative` depending on what the mnemonic supports and the operand's
/// resolved value).
#[derive(Clone, Debug, PartialEq)]
pub enum OperandSyntax<'a> {
    /// No operand tokens at all: implied, or the accumulator-mode shorthand
    /// (e.g. bare `ASL`, equivalent to `ASL A`).
    None,
    /// A bare `A` operand: explicit accumulator-mode addressing.
    Accumulator,
    /// `#expr`
    Immediate(Expr<'a>),
    /// `(expr)`
    Indirect(Expr<'a>),
    /// `(expr,X)`
    IndirectX(Expr<'a>),
    /// `(expr),Y`
    IndirectY(Expr<'a>),
    /// `expr`
    Direct(Expr<'a>),
    /// `expr,X`
    DirectX(Expr<'a>),
    /// `expr,Y`
    DirectY(Expr<'a>),
    /// `zp_expr,rel_expr` — the two-operand form used only by `BBR*`/`BBS*`.
    ZeroPageRelative(Expr<'a>, Expr<'a>),
}

fn is_end_of_operand(parser: &Parser) -> bool {
    match parser.peek() {
        None => true,
        Some(token) => token.token_type() == TokenType::Newline,
    }
}

fn symbol_text_matches(parser: &Parser, offset: usize, letter: &str) -> bool {
    match parser.peek_at(offset) {
        Some(token) => match token.token_type() {
            TokenType::Symbol(_) => token.text().eq_ignore_ascii_case(letter),
            _ => false,
        },
        None => false,
    }
}

/// Parses the operand syntax following a mnemonic. `accumulator_supported`
/// and `zero_page_relative_supported` come from the mnemonic's entries in
/// the [`InstructionTable`] (whether it has an `Accumulator` or
/// `ZeroPageRelative` addressing mode), since those two syntaxes can't be
/// told apart from generic expression grammar alone.
pub fn parse_operand<'a>(
    parser: &mut Parser<'a>,
    accumulator_supported: bool,
    zero_page_relative_supported: bool,
) -> Result<OperandSyntax<'a>, Error> {
    if is_end_of_operand(parser) {
        return Ok(OperandSyntax::None);
    }

    if accumulator_supported && symbol_text_matches(parser, 0, "A") {
        // Only treat bare `A` as accumulator addressing when nothing follows
        // it — `A+1` or `A,X` is an expression that happens to start with a
        // symbol named `A`, not the accumulator shorthand.
        let end_after_a = match parser.peek_at(1) {
            None => true,
            Some(token) => token.token_type() == TokenType::Newline,
        };
        if end_after_a {
            parser.advance();
            return Ok(OperandSyntax::Accumulator);
        }
    }

    if let Some(token) = parser.peek() {
        if token.token_type() == TokenType::Hash {
            parser.advance();
            let expr = parser.parse_expr()?;
            return Ok(OperandSyntax::Immediate(expr));
        }
        if token.token_type() == TokenType::LeftParen {
            return parse_indirect_operand(parser);
        }
    }

    let first = parser.parse_expr()?;
    if let Some(token) = parser.peek()
        && token.token_type() == TokenType::Comma {
        parser.advance();
        return parse_after_comma(parser, first, zero_page_relative_supported);
    }
    Ok(OperandSyntax::Direct(first))
}

fn parse_indirect_operand<'a>(parser: &mut Parser<'a>) -> Result<OperandSyntax<'a>, Error> {
    let open = parser.advance().unwrap(); // consumes '('
    let inner = parser.parse_expr()?;
    match parser.advance() {
        Some(token) if token.token_type() == TokenType::Comma => {
            // (expr,X)
            expect_index_register(parser, "X")?;
            expect_token(parser, TokenType::RightParen, "expected closing parenthesis")?;
            Ok(OperandSyntax::IndirectX(inner))
        }
        Some(token) if token.token_type() == TokenType::RightParen => {
            // (expr) or (expr),Y
            if let Some(comma) = parser.peek()
                && comma.token_type() == TokenType::Comma {
                parser.advance();
                expect_index_register(parser, "Y")?;
                return Ok(OperandSyntax::IndirectY(inner));
            }
            Ok(OperandSyntax::Indirect(inner))
        }
        Some(token) => Err(Error::from(
            token.location.line, token.location.column,
            "expected ',' or closing parenthesis")),
        None => Err(Error::from(
            open.location.line, open.location.column,
            "expected closing parenthesis")),
    }
}

fn parse_after_comma<'a>(
    parser: &mut Parser<'a>,
    first: Expr<'a>,
    zero_page_relative_supported: bool,
) -> Result<OperandSyntax<'a>, Error> {
    if zero_page_relative_supported {
        let second = parser.parse_expr()?;
        return Ok(OperandSyntax::ZeroPageRelative(first, second));
    }
    if symbol_text_matches(parser, 0, "X") {
        parser.advance();
        return Ok(OperandSyntax::DirectX(first));
    }
    if symbol_text_matches(parser, 0, "Y") {
        parser.advance();
        return Ok(OperandSyntax::DirectY(first));
    }
    match parser.peek() {
        Some(token) => Err(Error::from(
            token.location.line, token.location.column,
            "expected 'X' or 'Y' index register")),
        None => Err(Error::from(0, 0, "expected 'X' or 'Y' index register")),
    }
}

fn expect_index_register(parser: &mut Parser, letter: &str) -> Result<(), Error> {
    if symbol_text_matches(parser, 0, letter) {
        parser.advance();
        Ok(())
    } else {
        match parser.peek() {
            Some(token) => Err(Error::from(
                token.location.line, token.location.column,
                &format!("expected index register '{letter}'"))),
            None => Err(Error::from(0, 0, &format!("expected index register '{letter}'"))),
        }
    }
}

fn expect_token(parser: &mut Parser, expected: TokenType, message: &str) -> Result<(), Error> {
    match parser.advance() {
        Some(token) if token.token_type() == expected => Ok(()),
        Some(token) => Err(Error::from(token.location.line, token.location.column, message)),
        None => Err(Error::from(0, 0, message)),
    }
}

/// The result of resolving an [`OperandSyntax`] against a symbol table: the
/// addressing mode is always known (even when the operand value isn't yet —
/// see the driver's multi-pass design), but `bytes` is `None` until every
/// symbol the operand depends on is resolvable.
#[derive(Debug, PartialEq)]
pub struct EncodedInstruction {
    pub mode: AddressingMode,
    pub byte_len: u8,
    pub bytes: Option<Vec<u8>>,
}

/// Bundles the operand-independent inputs every `encode_*` helper needs, so
/// none of them has to take mnemonic/addr/symbols/table/location as five
/// separate parameters.
struct Context<'s> {
    mnemonic: Mnemonic,
    addr: u16,
    symbols: &'s HashMap<String, Operand>,
    table: &'s InstructionTable,
    location: Location,
}

/// Resolves `operand` for `mnemonic` at `addr` against `symbols`, choosing
/// among the addressing modes `table` says the mnemonic supports and
/// producing opcode + operand bytes once fully resolvable.
pub fn encode(
    mnemonic: Mnemonic,
    operand: &OperandSyntax,
    addr: u16,
    symbols: &HashMap<String, Operand>,
    table: &InstructionTable,
    location: Location,
) -> Result<EncodedInstruction, Error> {
    let ctx = Context { mnemonic, addr, symbols, table, location };
    match operand {
        OperandSyntax::None => encode_none(&ctx),
        OperandSyntax::Accumulator => encode_fixed(&ctx, AddressingMode::Accumulator),
        OperandSyntax::Immediate(expr) => encode_sized(&ctx, AddressingMode::Immediate, expr),
        OperandSyntax::Indirect(expr) =>
            encode_indirect_family(&ctx, expr, AddressingMode::Indirect, AddressingMode::ZeroPageIndirect),
        OperandSyntax::IndirectX(expr) =>
            encode_indirect_family(&ctx, expr, AddressingMode::AbsoluteIndirectX, AddressingMode::IndirectX),
        OperandSyntax::IndirectY(expr) => encode_sized(&ctx, AddressingMode::IndirectY, expr),
        OperandSyntax::Direct(expr) =>
            encode_direct(&ctx, expr, AddressingMode::ZeroPage, AddressingMode::Absolute),
        OperandSyntax::DirectX(expr) =>
            encode_direct(&ctx, expr, AddressingMode::ZeroPageX, AddressingMode::AbsoluteX),
        OperandSyntax::DirectY(expr) =>
            encode_direct(&ctx, expr, AddressingMode::ZeroPageY, AddressingMode::AbsoluteY),
        OperandSyntax::ZeroPageRelative(zp_expr, rel_expr) =>
            encode_zero_page_relative(&ctx, zp_expr, rel_expr),
    }
}

/// Opcode byte followed by zero-padding out to `op.byte_len` — covers `BRK`,
/// whose second byte is a signature/padding byte the CPU skips over without
/// reading (see `Cpu`'s `Mnemonic::Brk` handling) but which still must be
/// reserved in the output so later statements land at the right address.
/// Every other fixed-length (`Implied`/`Accumulator`) opcode is 1 byte, so
/// this is a no-op padding for them.
fn opcode_bytes(op: &DecodedOp) -> Vec<u8> {
    let mut bytes = vec![op.opcode];
    bytes.resize(op.byte_len as usize, 0);
    bytes
}

fn encode_none(ctx: &Context) -> Result<EncodedInstruction, Error> {
    if let Some(op) = ctx.table.get(ctx.mnemonic, AddressingMode::Implied) {
        return Ok(EncodedInstruction { mode: AddressingMode::Implied, byte_len: op.byte_len, bytes: Some(opcode_bytes(op)) });
    }
    if let Some(op) = ctx.table.get(ctx.mnemonic, AddressingMode::Accumulator) {
        return Ok(EncodedInstruction { mode: AddressingMode::Accumulator, byte_len: op.byte_len, bytes: Some(opcode_bytes(op)) });
    }
    Err(unsupported(ctx.mnemonic, ctx.location))
}

fn encode_fixed(ctx: &Context, mode: AddressingMode) -> Result<EncodedInstruction, Error> {
    match ctx.table.get(ctx.mnemonic, mode) {
        Some(op) => Ok(EncodedInstruction { mode, byte_len: op.byte_len, bytes: Some(opcode_bytes(op)) }),
        None => Err(unsupported(ctx.mnemonic, ctx.location)),
    }
}

/// Handles a family where one syntax form (e.g. `(expr,X)`) maps to exactly
/// one of two mutually-exclusive addressing modes depending on which the
/// mnemonic supports — never both (e.g. `IndirectX` for zero-page-indexed
/// ops vs. `AbsoluteIndirectX` for `JMP`).
fn encode_indirect_family(
    ctx: &Context,
    expr: &Expr,
    wide_mode: AddressingMode,
    narrow_mode: AddressingMode,
) -> Result<EncodedInstruction, Error> {
    if ctx.table.get(ctx.mnemonic, narrow_mode).is_some() {
        return encode_sized(ctx, narrow_mode, expr);
    }
    if ctx.table.get(ctx.mnemonic, wide_mode).is_some() {
        return encode_sized(ctx, wide_mode, expr);
    }
    Err(unsupported(ctx.mnemonic, ctx.location))
}

fn encode_sized(ctx: &Context, mode: AddressingMode, expr: &Expr) -> Result<EncodedInstruction, Error> {
    let op = ctx.table.get(ctx.mnemonic, mode).ok_or_else(|| unsupported(ctx.mnemonic, ctx.location))?;
    let byte_len = op.byte_len;
    let opcode = op.opcode;
    match evaluate(expr, ctx.symbols)? {
        None => Ok(EncodedInstruction { mode, byte_len, bytes: None }),
        Some(value) => {
            let mut bytes = vec![opcode];
            bytes.extend(operand_value_bytes(mode, value, expr_location(expr))?);
            Ok(EncodedInstruction { mode, byte_len, bytes: Some(bytes) })
        }
    }
}

/// Handles the `Direct`/`DirectX`/`DirectY` family, where a bare `expr`
/// (optionally indexed) can resolve to either a zero-page or absolute
/// addressing mode — or, for branch mnemonics that support neither,
/// `Relative`.
fn encode_direct(
    ctx: &Context,
    expr: &Expr,
    zp_mode: AddressingMode,
    abs_mode: AddressingMode,
) -> Result<EncodedInstruction, Error> {
    let zp = ctx.table.get(ctx.mnemonic, zp_mode);
    let abs = ctx.table.get(ctx.mnemonic, abs_mode);
    if zp.is_none() && abs.is_none() {
        if ctx.table.get(ctx.mnemonic, AddressingMode::Relative).is_some() {
            return encode_relative(ctx, expr);
        }
        return Err(unsupported(ctx.mnemonic, ctx.location));
    }

    let value = evaluate(expr, ctx.symbols)?;
    let use_zp = matches!((value, zp), (Some(v), Some(_)) if v <= 0xFF);
    if use_zp {
        return encode_sized(ctx, zp_mode, expr);
    }
    if abs.is_some() {
        return encode_sized(ctx, abs_mode, expr);
    }
    // Only a zero-page form exists, and the resolved value doesn't fit.
    match value {
        None => Ok(EncodedInstruction { mode: zp_mode, byte_len: zp.unwrap().byte_len, bytes: None }),
        Some(_) => Err(operand_range_error(zp_mode, expr_location(expr))),
    }
}

fn encode_relative(ctx: &Context, expr: &Expr) -> Result<EncodedInstruction, Error> {
    let op = ctx.table.get(ctx.mnemonic, AddressingMode::Relative).ok_or_else(|| unsupported(ctx.mnemonic, ctx.location))?;
    let byte_len = op.byte_len;
    match evaluate(expr, ctx.symbols)? {
        None => Ok(EncodedInstruction { mode: AddressingMode::Relative, byte_len, bytes: None }),
        Some(target) => {
            let pc_after = ctx.addr.wrapping_add(byte_len as u16);
            let displacement = relative_displacement(target as u16, pc_after)
                .ok_or_else(|| branch_out_of_range(expr_location(expr)))?;
            Ok(EncodedInstruction { mode: AddressingMode::Relative, byte_len, bytes: Some(vec![op.opcode, displacement]) })
        }
    }
}

fn encode_zero_page_relative(ctx: &Context, zp_expr: &Expr, rel_expr: &Expr) -> Result<EncodedInstruction, Error> {
    let op = ctx.table.get(ctx.mnemonic, AddressingMode::ZeroPageRelative)
        .ok_or_else(|| unsupported(ctx.mnemonic, ctx.location))?;
    let byte_len = op.byte_len;
    let opcode = op.opcode;
    let zp_value = evaluate(zp_expr, ctx.symbols)?;
    let rel_value = evaluate(rel_expr, ctx.symbols)?;
    match (zp_value, rel_value) {
        (Some(zp), Some(target)) => {
            if zp > 0xFF {
                return Err(operand_range_error(AddressingMode::ZeroPage, expr_location(zp_expr)));
            }
            let pc_after = ctx.addr.wrapping_add(byte_len as u16);
            let displacement = relative_displacement(target as u16, pc_after)
                .ok_or_else(|| branch_out_of_range(expr_location(rel_expr)))?;
            Ok(EncodedInstruction {
                mode: AddressingMode::ZeroPageRelative,
                byte_len,
                bytes: Some(vec![opcode, zp as u8, displacement]),
            })
        }
        _ => Ok(EncodedInstruction { mode: AddressingMode::ZeroPageRelative, byte_len, bytes: None }),
    }
}

/// Computes the signed 8-bit PC-relative displacement from `pc_after` to
/// `target`, or `None` if `target` is out of branch range. Mirrors
/// `Disassembler::absolute_address`'s forward calculation
/// (`pc_after.wrapping_add(displacement as i8 as u16)`) in reverse, and uses
/// the same round-trip as the correctness check rather than a signed-range
/// comparison, since wrapping address arithmetic makes a plain range check
/// unreliable near the top/bottom of the address space.
fn relative_displacement(target: u16, pc_after: u16) -> Option<u8> {
    let displacement = target.wrapping_sub(pc_after) as u8;
    if pc_after.wrapping_add(displacement as i8 as u16) == target {
        Some(displacement)
    } else {
        None
    }
}

fn operand_value_bytes(mode: AddressingMode, value: Operand, location: Location) -> Result<Vec<u8>, Error> {
    match mode {
        AddressingMode::Immediate |
        AddressingMode::ZeroPage | AddressingMode::ZeroPageX | AddressingMode::ZeroPageY |
        AddressingMode::IndirectX | AddressingMode::IndirectY | AddressingMode::ZeroPageIndirect => {
            if value > 0xFF {
                return Err(operand_range_error(mode, location));
            }
            Ok(vec![value as u8])
        }
        AddressingMode::Absolute | AddressingMode::AbsoluteX | AddressingMode::AbsoluteY |
        AddressingMode::Indirect | AddressingMode::AbsoluteIndirectX => {
            Ok((value as u16).to_le_bytes().to_vec())
        }
        _ => panic!("operand_value_bytes: unsupported mode {mode:?}"),
    }
}

fn expr_location(expr: &Expr) -> Location {
    expr.token().location
}

fn unsupported(mnemonic: Mnemonic, location: Location) -> Error {
    Error::from(location.line, location.column,
                &format!("addressing mode not supported by {mnemonic}"))
}

fn operand_range_error(mode: AddressingMode, location: Location) -> Error {
    let description = match mode {
        AddressingMode::Immediate => "immediate operand",
        AddressingMode::ZeroPage | AddressingMode::ZeroPageX | AddressingMode::ZeroPageY |
        AddressingMode::IndirectX | AddressingMode::IndirectY | AddressingMode::ZeroPageIndirect =>
            "zero-page operand",
        _ => "operand",
    };
    Error::from(location.line, location.column, &format!("{description} out of range"))
}

fn branch_out_of_range(location: Location) -> Error {
    Error::from(location.line, location.column, "branch target out of range")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::cpu::variant::CpuVariant;

    fn parse_operand_source<'a>(
        source: &'a str,
        accumulator_supported: bool,
        zero_page_relative_supported: bool,
    ) -> Result<OperandSyntax<'a>, Error> {
        let mut parser = Parser::new(source).unwrap();
        parse_operand(&mut parser, accumulator_supported, zero_page_relative_supported)
    }

    // --- parse_operand ---

    #[test]
    fn parse_none_operand() {
        assert_eq!(parse_operand_source("", false, false).unwrap(), OperandSyntax::None);
        assert_eq!(parse_operand_source("\n", false, false).unwrap(), OperandSyntax::None);
    }

    #[test]
    fn parse_accumulator_operand() {
        assert_eq!(parse_operand_source("A", true, false).unwrap(), OperandSyntax::Accumulator);
        assert_eq!(parse_operand_source("a", true, false).unwrap(), OperandSyntax::Accumulator);
    }

    #[test]
    fn parse_a_as_direct_when_accumulator_not_supported() {
        let result = parse_operand_source("A", false, false).unwrap();
        match result {
            OperandSyntax::Direct(expr) => assert_eq!(expr.token().text(), "A"),
            other => panic!("expected Direct, got {other:?}"),
        }
    }

    #[test]
    fn parse_a_followed_by_more_is_direct_not_accumulator() {
        let result = parse_operand_source("A+1", true, false).unwrap();
        match result {
            OperandSyntax::Direct(_) => {}
            other => panic!("expected Direct, got {other:?}"),
        }
    }

    #[test]
    fn parse_immediate_operand() {
        let result = parse_operand_source("#$42", false, false).unwrap();
        match result {
            OperandSyntax::Immediate(expr) => assert_eq!(expr.token().text(), "$42"),
            other => panic!("expected Immediate, got {other:?}"),
        }
    }

    #[test]
    fn parse_direct_operand() {
        let result = parse_operand_source("label", false, false).unwrap();
        match result {
            OperandSyntax::Direct(expr) => assert_eq!(expr.token().text(), "label"),
            other => panic!("expected Direct, got {other:?}"),
        }
    }

    #[test]
    fn parse_direct_x_and_y_operands() {
        assert!(matches!(parse_operand_source("$50,X", false, false).unwrap(), OperandSyntax::DirectX(_)));
        assert!(matches!(parse_operand_source("$50,x", false, false).unwrap(), OperandSyntax::DirectX(_)));
        assert!(matches!(parse_operand_source("$50,Y", false, false).unwrap(), OperandSyntax::DirectY(_)));
        assert!(matches!(parse_operand_source("$50,y", false, false).unwrap(), OperandSyntax::DirectY(_)));
    }

    #[test]
    fn parse_indirect_operands() {
        assert!(matches!(parse_operand_source("($20)", false, false).unwrap(), OperandSyntax::Indirect(_)));
        assert!(matches!(parse_operand_source("($20,X)", false, false).unwrap(), OperandSyntax::IndirectX(_)));
        assert!(matches!(parse_operand_source("($20),Y", false, false).unwrap(), OperandSyntax::IndirectY(_)));
    }

    #[test]
    fn parse_zero_page_relative_operand() {
        let result = parse_operand_source("$50,label", false, true).unwrap();
        match result {
            OperandSyntax::ZeroPageRelative(zp, rel) => {
                assert_eq!(zp.token().text(), "$50");
                assert_eq!(rel.token().text(), "label");
            }
            other => panic!("expected ZeroPageRelative, got {other:?}"),
        }
    }

    #[test]
    fn parse_indirect_missing_close_paren_errors() {
        assert!(parse_operand_source("($20", false, false).is_err());
    }

    #[test]
    fn parse_comma_without_index_register_errors() {
        let result = parse_operand_source("$50,Z", false, false);
        assert!(result.is_err());
    }

    #[test]
    fn parse_leaves_trailing_comment_context_unconsumed() {
        let mut parser = Parser::new("$50,X ; index by X\n").unwrap();
        let result = parse_operand(&mut parser, false, false).unwrap();
        assert!(matches!(result, OperandSyntax::DirectX(_)));
        // the comment is stripped by the scanner; only the newline remains
        assert!(!parser.is_at_end());
    }

    // --- encode ---

    fn no_symbols() -> HashMap<String, Operand> {
        HashMap::new()
    }

    fn table(variant: CpuVariant) -> InstructionTable {
        InstructionTable::new(variant)
    }

    fn encode_source(
        mnemonic: Mnemonic,
        source: &str,
        addr: u16,
        symbols: &HashMap<String, Operand>,
        table: &InstructionTable,
        accumulator_supported: bool,
        zero_page_relative_supported: bool,
    ) -> Result<EncodedInstruction, Error> {
        let mut parser = Parser::new(source).unwrap();
        let operand = parse_operand(&mut parser, accumulator_supported, zero_page_relative_supported)?;
        encode(mnemonic, &operand, addr, symbols, table, Location::from(1, 1))
    }

    #[test]
    fn encode_implied() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Nop, "", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Implied);
        assert_eq!(result.bytes, Some(vec![0xEA]));
    }

    #[test]
    fn encode_accumulator_shorthand_with_no_operand() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Asl, "", 0x0200, &no_symbols(), &t, true, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Accumulator);
        assert_eq!(result.bytes, Some(vec![0x0A]));
    }

    #[test]
    fn encode_accumulator_explicit() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Asl, "A", 0x0200, &no_symbols(), &t, true, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Accumulator);
        assert_eq!(result.bytes, Some(vec![0x0A]));
    }

    #[test]
    fn encode_immediate() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Lda, "#$42", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Immediate);
        assert_eq!(result.bytes, Some(vec![0xA9, 0x42]));
    }

    #[test]
    fn encode_immediate_out_of_range_errors() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Lda, "#$1FF", 0x0200, &no_symbols(), &t, false, false);
        assert!(result.is_err());
    }

    #[test]
    fn encode_direct_picks_zero_page_when_value_fits() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Lda, "$50", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::ZeroPage);
        assert_eq!(result.bytes, Some(vec![0xA5, 0x50]));
    }

    #[test]
    fn encode_direct_picks_absolute_when_value_does_not_fit() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Lda, "$1234", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Absolute);
        assert_eq!(result.bytes, Some(vec![0xAD, 0x34, 0x12]));
    }

    #[test]
    fn encode_direct_picks_widest_mode_when_unresolved() {
        let t = table(CpuVariant::Cmos65C02);
        // "forward" is not in the symbol table yet.
        let result = encode_source(Mnemonic::Lda, "forward", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Absolute);
        assert_eq!(result.byte_len, 3);
        assert_eq!(result.bytes, None);
    }

    #[test]
    fn encode_direct_shrinks_once_forward_reference_resolves_small() {
        let t = table(CpuVariant::Cmos65C02);
        let mut symbols = no_symbols();
        symbols.insert("forward".to_string(), 0x50);
        let result = encode_source(Mnemonic::Lda, "forward", 0x0200, &symbols, &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::ZeroPage);
        assert_eq!(result.bytes, Some(vec![0xA5, 0x50]));
    }

    #[test]
    fn encode_direct_x_and_y() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Lda, "$50,X", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::ZeroPageX);
        assert_eq!(result.bytes, Some(vec![0xB5, 0x50]));

        let result = encode_source(Mnemonic::Lda, "$1234,Y", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::AbsoluteY);
        assert_eq!(result.bytes, Some(vec![0xB9, 0x34, 0x12]));
    }

    #[test]
    fn encode_indirect_x_zero_page() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Lda, "($20,X)", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::IndirectX);
        assert_eq!(result.bytes, Some(vec![0xA1, 0x20]));
    }

    #[test]
    fn encode_indirect_x_absolute_for_jmp() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Jmp, "($0300,X)", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::AbsoluteIndirectX);
        assert_eq!(result.bytes, Some(vec![0x7C, 0x00, 0x03]));
    }

    #[test]
    fn encode_indirect_y() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Lda, "($20),Y", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::IndirectY);
        assert_eq!(result.bytes, Some(vec![0xB1, 0x20]));
    }

    #[test]
    fn encode_zero_page_indirect() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Lda, "($20)", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::ZeroPageIndirect);
        assert_eq!(result.bytes, Some(vec![0xB2, 0x20]));
    }

    #[test]
    fn encode_indirect_absolute_for_jmp() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Jmp, "($0300)", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Indirect);
        assert_eq!(result.bytes, Some(vec![0x6C, 0x00, 0x03]));
    }

    #[test]
    fn encode_relative_branch_forward() {
        let t = table(CpuVariant::Cmos65C02);
        let mut symbols = no_symbols();
        symbols.insert("target".to_string(), 0x0206);
        // BEQ at 0x0200, 2-byte instruction => pc_after = 0x0202, displacement +4
        let result = encode_source(Mnemonic::Beq, "target", 0x0200, &symbols, &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Relative);
        assert_eq!(result.bytes, Some(vec![0xF0, 0x04]));
    }

    #[test]
    fn encode_relative_branch_backward() {
        let t = table(CpuVariant::Cmos65C02);
        let mut symbols = no_symbols();
        symbols.insert("target".to_string(), 0x0200);
        // BRA at 0x0200, 2-byte instruction => pc_after = 0x0202, displacement -2
        let result = encode_source(Mnemonic::Bra, "target", 0x0200, &symbols, &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Relative);
        assert_eq!(result.bytes, Some(vec![0x80, 0xFE]));
    }

    #[test]
    fn encode_relative_branch_out_of_range_errors() {
        let t = table(CpuVariant::Cmos65C02);
        let mut symbols = no_symbols();
        symbols.insert("target".to_string(), 0x1000);
        let result = encode_source(Mnemonic::Bra, "target", 0x0200, &symbols, &t, false, false);
        assert!(result.is_err());
    }

    #[test]
    fn encode_relative_unresolved_leaves_bytes_none() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Bra, "forward", 0x0200, &no_symbols(), &t, false, false).unwrap();
        assert_eq!(result.mode, AddressingMode::Relative);
        assert_eq!(result.byte_len, 2);
        assert_eq!(result.bytes, None);
    }

    #[test]
    fn encode_zero_page_relative_bbr() {
        let t = table(CpuVariant::Wdc65C02);
        let mut symbols = no_symbols();
        symbols.insert("bar".to_string(), 0x50);
        symbols.insert("foo".to_string(), 0x207);
        // BBR0 at 0x0200, 3-byte instruction => pc_after = 0x0203, displacement +4
        let result = encode_source(Mnemonic::Bbr0, "bar,foo", 0x0200, &symbols, &t, false, true).unwrap();
        assert_eq!(result.mode, AddressingMode::ZeroPageRelative);
        assert_eq!(result.bytes, Some(vec![0x0F, 0x50, 0x04]));
    }

    #[test]
    fn encode_zero_page_relative_unresolved_leaves_bytes_none() {
        let t = table(CpuVariant::Wdc65C02);
        let result = encode_source(Mnemonic::Bbr0, "bar,foo", 0x0200, &no_symbols(), &t, false, true).unwrap();
        assert_eq!(result.mode, AddressingMode::ZeroPageRelative);
        assert_eq!(result.byte_len, 3);
        assert_eq!(result.bytes, None);
    }

    #[test]
    fn encode_unsupported_mode_errors() {
        let t = table(CpuVariant::Cmos65C02);
        // LDA has no accumulator-mode opcode.
        let result = encode(Mnemonic::Lda, &OperandSyntax::Accumulator, 0x0200, &no_symbols(), &t, Location::from(1, 1));
        assert!(result.is_err());
    }

    #[test]
    fn encode_wdc_only_mnemonic_unsupported_under_cmos_variant() {
        let t = table(CpuVariant::Cmos65C02);
        let result = encode_source(Mnemonic::Stp, "", 0x0200, &no_symbols(), &t, false, false);
        assert!(result.is_err());
    }
}
