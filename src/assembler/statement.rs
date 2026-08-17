//! Line-oriented statement grammar: labels, symbol assignment, directives,
//! and instructions, plus `parse_program`, which turns a whole source string
//! into a flat, ordered list of statements for the driver (a later unit) to
//! lay out and encode.
//!
//! A source line is one of:
//! - blank (only a comment and/or newline — the scanner already strips
//!   comments, so this just means "nothing before the newline")
//! - `IDENT '=' expr` or `IDENT '.equ' expr` — symbol assignment, occupies
//!   the whole line
//! - `IDENT ':'` — a label, optionally followed by a directive or
//!   instruction on the same line (`my_routine:  LDA #$55`)
//! - a directive (`.org`/`.byte`/`.word`/`.res`)
//! - an instruction (mnemonic plus operand syntax)

use super::error::Error;
use super::expr::Expr;
use super::instructions::{InstructionTable, mnemonic_from_str};
use super::operand::{OperandSyntax, parse_operand};
use super::parser::Parser;
use super::token::{Token, TokenType};
use crate::emulator::cpu::opcodes::{AddressingMode, Mnemonic};
use crate::location::Location;

/// A `.byte` operand: either a numeric expression (truncated to one byte) or
/// a string literal (each character emitted as one byte).
#[derive(Clone, Debug, PartialEq)]
pub enum ByteOperand<'a> {
    Expr(Expr<'a>),
    String(String),
}

/// A directive and its operand(s), independent of any preceding label.
#[derive(Clone, Debug, PartialEq)]
pub enum Directive<'a> {
    /// `.org expr` — starts a new output segment at `expr`.
    Org(Expr<'a>),
    /// `.byte op, op, ...` — emits one byte per numeric operand, or one byte
    /// per character for a string operand.
    Byte(Vec<ByteOperand<'a>>),
    /// `.word expr, expr, ...` — emits each operand as two little-endian bytes.
    Word(Vec<Expr<'a>>),
    /// `.res expr` — reserves `expr` zero bytes.
    Res(Expr<'a>),
}

/// One parsed statement. A single source line can produce up to two of
/// these (a label followed by a directive or instruction).
#[derive(Clone, Debug, PartialEq)]
pub enum Statement<'a> {
    Label(String),
    SymbolAssign(String, Expr<'a>),
    Directive(Directive<'a>),
    Instruction(Mnemonic, OperandSyntax<'a>),
}

/// Parses the entire `source` into a flat, ordered list of statements, each
/// paired with the source location of its first token. `table` resolves
/// whether a mnemonic supports accumulator/zero-page-relative addressing,
/// needed to disambiguate operand syntax (see [`super::operand::parse_operand`]).
pub fn parse_program<'a>(
    source: &'a str,
    table: &InstructionTable,
) -> Result<Vec<(Statement<'a>, Location)>, Error> {
    let mut parser = Parser::new(source)?;
    let mut statements = Vec::new();
    while !parser.is_at_end() {
        statements.extend(parse_line(&mut parser, table)?);
    }
    Ok(statements)
}

/// Parses one source line (through its trailing newline, if any) into zero,
/// one, or two statements.
fn parse_line<'a>(
    parser: &mut Parser<'a>,
    table: &InstructionTable,
) -> Result<Vec<(Statement<'a>, Location)>, Error> {
    let mut statements = Vec::new();

    if is_end_of_line(parser) {
        consume_newline(parser);
        return Ok(statements);
    }

    if is_symbol_assign_form(parser) {
        statements.push(parse_symbol_assign(parser)?);
        expect_end_of_line(parser)?;
        return Ok(statements);
    }

    if is_label_form(parser) {
        statements.push(parse_label(parser)?);
        if is_end_of_line(parser) {
            expect_end_of_line(parser)?;
            return Ok(statements);
        }
    }

    match parser.peek().map(|t| t.token_type().clone()) {
        Some(TokenType::Directive(_)) => statements.push(parse_directive_statement(parser)?),
        Some(TokenType::Symbol(_)) => statements.push(parse_instruction(parser, table)?),
        _ => return Err(unexpected_token_error(parser)),
    }

    expect_end_of_line(parser)?;
    Ok(statements)
}

fn is_symbol_assign_form(parser: &Parser) -> bool {
    if !matches!(parser.peek().map(|t| t.token_type()), Some(TokenType::Symbol(_))) {
        return false;
    }
    match parser.peek_at(1).map(|t| t.token_type()) {
        Some(TokenType::Equal) => true,
        Some(TokenType::Directive(name)) => name.eq_ignore_ascii_case("equ"),
        _ => false,
    }
}

fn is_label_form(parser: &Parser) -> bool {
    matches!(parser.peek().map(|t| t.token_type()), Some(TokenType::Symbol(_)))
        && matches!(parser.peek_at(1).map(|t| t.token_type()), Some(TokenType::Colon))
}

fn symbol_name(token: &Token) -> String {
    match token.token_type() {
        TokenType::Symbol(name) => name.clone(),
        _ => unreachable!("caller guaranteed a Symbol token"),
    }
}

fn parse_label<'a>(parser: &mut Parser<'a>) -> Result<(Statement<'a>, Location), Error> {
    let name_token = parser.advance().unwrap();
    let location = name_token.location;
    let name = symbol_name(&name_token);
    parser.advance(); // ':'
    Ok((Statement::Label(name), location))
}

fn parse_symbol_assign<'a>(parser: &mut Parser<'a>) -> Result<(Statement<'a>, Location), Error> {
    let name_token = parser.advance().unwrap();
    let location = name_token.location;
    let name = symbol_name(&name_token);
    parser.advance(); // '=' or '.equ'
    let expr = parser.parse_expr()?;
    Ok((Statement::SymbolAssign(name, expr), location))
}

fn parse_directive_statement<'a>(parser: &mut Parser<'a>) -> Result<(Statement<'a>, Location), Error> {
    let token = parser.advance().unwrap();
    let location = token.location;
    let name = match token.token_type() {
        TokenType::Directive(name) => name.clone(),
        _ => unreachable!("caller guaranteed a Directive token"),
    };
    let directive = match name.to_ascii_lowercase().as_str() {
        "org" => Directive::Org(parser.parse_expr()?),
        "byte" => Directive::Byte(parse_byte_operands(parser)?),
        "word" => Directive::Word(parse_expr_list(parser)?),
        "res" => Directive::Res(parser.parse_expr()?),
        other => {
            return Err(Error::from(
                location.line,
                location.column,
                &format!("unknown directive '.{other}'"),
            ));
        }
    };
    Ok((Statement::Directive(directive), location))
}

fn parse_byte_operands<'a>(parser: &mut Parser<'a>) -> Result<Vec<ByteOperand<'a>>, Error> {
    let mut operands = vec![parse_byte_operand(parser)?];
    while let Some(token) = parser.peek()
        && token.token_type() == TokenType::Comma
    {
        parser.advance();
        operands.push(parse_byte_operand(parser)?);
    }
    Ok(operands)
}

fn parse_byte_operand<'a>(parser: &mut Parser<'a>) -> Result<ByteOperand<'a>, Error> {
    if let Some(token) = parser.peek()
        && let TokenType::String(s) = token.token_type()
    {
        let s = s.clone();
        parser.advance();
        return Ok(ByteOperand::String(s));
    }
    Ok(ByteOperand::Expr(parser.parse_expr()?))
}

fn parse_expr_list<'a>(parser: &mut Parser<'a>) -> Result<Vec<Expr<'a>>, Error> {
    let mut exprs = vec![parser.parse_expr()?];
    while let Some(token) = parser.peek()
        && token.token_type() == TokenType::Comma
    {
        parser.advance();
        exprs.push(parser.parse_expr()?);
    }
    Ok(exprs)
}

fn parse_instruction<'a>(
    parser: &mut Parser<'a>,
    table: &InstructionTable,
) -> Result<(Statement<'a>, Location), Error> {
    let token = parser.advance().unwrap();
    let location = token.location;
    let name = symbol_name(&token);
    let mnemonic = mnemonic_from_str(&name).ok_or_else(|| {
        Error::from(location.line, location.column, &format!("unknown mnemonic '{name}'"))
    })?;
    let accumulator_supported = table.get(mnemonic, AddressingMode::Accumulator).is_some();
    let zero_page_relative_supported = table.get(mnemonic, AddressingMode::ZeroPageRelative).is_some();
    let operand = parse_operand(parser, accumulator_supported, zero_page_relative_supported)?;
    Ok((Statement::Instruction(mnemonic, operand), location))
}

fn is_end_of_line(parser: &Parser) -> bool {
    match parser.peek() {
        None => true,
        Some(token) => token.token_type() == TokenType::Newline,
    }
}

fn consume_newline(parser: &mut Parser) {
    if let Some(token) = parser.peek()
        && token.token_type() == TokenType::Newline
    {
        parser.advance();
    }
}

fn expect_end_of_line(parser: &mut Parser) -> Result<(), Error> {
    if is_end_of_line(parser) {
        consume_newline(parser);
        Ok(())
    } else {
        let token = parser.peek().unwrap();
        Err(Error::from(token.location.line, token.location.column, "expected end of line"))
    }
}

fn unexpected_token_error(parser: &Parser) -> Error {
    match parser.peek() {
        Some(token) => Error::from(token.location.line, token.location.column, "unexpected token"),
        None => Error::from(0, 0, "unexpected end of input"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::cpu::variant::CpuVariant;

    fn table() -> InstructionTable {
        InstructionTable::new(CpuVariant::Wdc65C02)
    }

    fn parse_line_source<'a>(
        source: &'a str,
        table: &InstructionTable,
    ) -> Result<Vec<(Statement<'a>, Location)>, Error> {
        let mut parser = Parser::new(source).unwrap();
        parse_line(&mut parser, table)
    }

    #[test]
    fn parse_blank_line() {
        let t = table();
        assert_eq!(parse_line_source("", &t).unwrap(), vec![]);
        assert_eq!(parse_line_source("\n", &t).unwrap(), vec![]);
        assert_eq!(parse_line_source("  \n", &t).unwrap(), vec![]);
    }

    #[test]
    fn parse_label_only() {
        let t = table();
        let statements = parse_line_source("loop:\n", &t).unwrap();
        assert_eq!(statements.len(), 1);
        assert_eq!(statements[0].0, Statement::Label(String::from("loop")));
    }

    #[test]
    fn parse_label_followed_by_instruction() {
        let t = table();
        let statements = parse_line_source("loop: LDA #1\n", &t).unwrap();
        assert_eq!(statements.len(), 2);
        assert_eq!(statements[0].0, Statement::Label(String::from("loop")));
        match &statements[1].0 {
            Statement::Instruction(Mnemonic::Lda, OperandSyntax::Immediate(_)) => {}
            other => panic!("expected Instruction(Lda, Immediate), got {other:?}"),
        }
    }

    #[test]
    fn parse_label_followed_by_directive() {
        let t = table();
        let statements = parse_line_source("here: .byte 1\n", &t).unwrap();
        assert_eq!(statements.len(), 2);
        assert_eq!(statements[0].0, Statement::Label(String::from("here")));
        assert!(matches!(&statements[1].0, Statement::Directive(Directive::Byte(_))));
    }

    #[test]
    fn parse_symbol_assign_with_equal() {
        let t = table();
        let statements = parse_line_source("FOO = 42\n", &t).unwrap();
        assert_eq!(statements.len(), 1);
        match &statements[0].0 {
            Statement::SymbolAssign(name, expr) => {
                assert_eq!(name, "FOO");
                assert_eq!(expr.token().text(), "42");
            }
            other => panic!("expected SymbolAssign, got {other:?}"),
        }
    }

    #[test]
    fn parse_symbol_assign_with_equ() {
        let t = table();
        let statements = parse_line_source("FOO .equ 42\n", &t).unwrap();
        assert_eq!(statements.len(), 1);
        match &statements[0].0 {
            Statement::SymbolAssign(name, expr) => {
                assert_eq!(name, "FOO");
                assert_eq!(expr.token().text(), "42");
            }
            other => panic!("expected SymbolAssign, got {other:?}"),
        }
    }

    #[test]
    fn parse_org_directive() {
        let t = table();
        let statements = parse_line_source(".org $8000\n", &t).unwrap();
        assert_eq!(statements.len(), 1);
        match &statements[0].0 {
            Statement::Directive(Directive::Org(expr)) => assert_eq!(expr.token().text(), "$8000"),
            other => panic!("expected Directive(Org), got {other:?}"),
        }
    }

    #[test]
    fn parse_byte_directive_with_mixed_operands() {
        let t = table();
        let statements = parse_line_source(".byte 1, \"AB\", 2\n", &t).unwrap();
        match &statements[0].0 {
            Statement::Directive(Directive::Byte(operands)) => {
                assert_eq!(operands.len(), 3);
                assert!(matches!(&operands[0], ByteOperand::Expr(_)));
                assert_eq!(operands[1], ByteOperand::String(String::from("AB")));
                assert!(matches!(&operands[2], ByteOperand::Expr(_)));
            }
            other => panic!("expected Directive(Byte), got {other:?}"),
        }
    }

    #[test]
    fn parse_word_directive_with_multiple_operands() {
        let t = table();
        let statements = parse_line_source(".word $1234, $5678\n", &t).unwrap();
        match &statements[0].0 {
            Statement::Directive(Directive::Word(exprs)) => assert_eq!(exprs.len(), 2),
            other => panic!("expected Directive(Word), got {other:?}"),
        }
    }

    #[test]
    fn parse_res_directive() {
        let t = table();
        let statements = parse_line_source(".res 4\n", &t).unwrap();
        match &statements[0].0 {
            Statement::Directive(Directive::Res(expr)) => assert_eq!(expr.token().text(), "4"),
            other => panic!("expected Directive(Res), got {other:?}"),
        }
    }

    #[test]
    fn parse_instruction_alone() {
        let t = table();
        let statements = parse_line_source("NOP\n", &t).unwrap();
        assert_eq!(statements.len(), 1);
        assert_eq!(statements[0].0, Statement::Instruction(Mnemonic::Nop, OperandSyntax::None));
    }

    #[test]
    fn parse_unknown_mnemonic_errors() {
        let t = table();
        let result = parse_line_source("FROB #1\n", &t);
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("unknown mnemonic"));
    }

    #[test]
    fn parse_unknown_directive_errors() {
        let t = table();
        let result = parse_line_source(".bogus 1\n", &t);
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("unknown directive"));
    }

    #[test]
    fn parse_trailing_garbage_errors() {
        let t = table();
        let result = parse_line_source("LDA #1 extra\n", &t);
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("expected end of line"));
    }

    #[test]
    fn parse_program_multiple_lines() {
        let t = table();
        let statements = parse_program("start:\n  LDA #1\n  STA $10\n", &t).unwrap();
        assert_eq!(statements.len(), 3);
        assert_eq!(statements[0].0, Statement::Label(String::from("start")));
        assert!(matches!(&statements[1].0, Statement::Instruction(Mnemonic::Lda, _)));
        assert!(matches!(&statements[2].0, Statement::Instruction(Mnemonic::Sta, _)));
    }

    #[test]
    fn parse_program_skips_blank_lines_and_comments() {
        let t = table();
        let statements = parse_program("\n; a comment\nNOP\n\n", &t).unwrap();
        assert_eq!(statements.len(), 1);
        assert_eq!(statements[0].0, Statement::Instruction(Mnemonic::Nop, OperandSyntax::None));
    }
}
