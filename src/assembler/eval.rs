use std::collections::HashMap;

use super::error::Error;
use super::expr::{BinaryOperatorType, Expr, ExprType, Operand, UnaryOperatorType};

/// Evaluates `expr` against the given symbol table.
///
/// Returns `Ok(None)` when the expression (transitively) references a symbol
/// not yet present in `symbols` — during the assembler's multi-pass
/// resolution this means "not yet resolvable," not an error. Only once no
/// further passes will add symbols does an unresolved reference become a
/// real error, which is decided by the driver (a later unit), not here.
/// Errors returned here are limited to expressions that are otherwise fully
/// resolvable this pass (e.g. division by zero).
pub fn evaluate(expr: &Expr, symbols: &HashMap<String, Operand>) -> Result<Option<Operand>, Error> {
    match expr.expr_type() {
        ExprType::Number(n) => Ok(Some(*n)),
        ExprType::Symbol(name) => Ok(symbols.get(name).copied()),
        ExprType::Grouping(inner) => evaluate(inner, symbols),
        ExprType::Unary(op_type, operand) => match evaluate(operand, symbols)? {
            None => Ok(None),
            Some(v) => Ok(Some(apply_unary(op_type, v))),
        },
        ExprType::Binary(op_type, left, right) => {
            let l = evaluate(left, symbols)?;
            let r = evaluate(right, symbols)?;
            match (l, r) {
                (Some(l), Some(r)) => apply_binary(op_type, l, r, expr).map(Some),
                _ => Ok(None),
            }
        }
    }
}

fn apply_unary(op_type: &UnaryOperatorType, v: Operand) -> Operand {
    match op_type {
        UnaryOperatorType::Identity => v,
        UnaryOperatorType::Negate => v.wrapping_neg(),
        UnaryOperatorType::LogicalNot => (v == 0) as Operand,
        UnaryOperatorType::BitwiseNot => !v,
        UnaryOperatorType::Lsb => v & 0xFF,
        UnaryOperatorType::Msb => (v >> 8) & 0xFF,
    }
}

fn apply_binary(op_type: &BinaryOperatorType, l: Operand, r: Operand, expr: &Expr) -> Result<Operand, Error> {
    match op_type {
        BinaryOperatorType::Add => Ok(l.wrapping_add(r)),
        BinaryOperatorType::Subtract => Ok(l.wrapping_sub(r)),
        BinaryOperatorType::Multiply => Ok(l.wrapping_mul(r)),
        BinaryOperatorType::Divide => l.checked_div(r).ok_or_else(|| division_by_zero(expr)),
        BinaryOperatorType::Remainder => l.checked_rem(r).ok_or_else(|| division_by_zero(expr)),
        BinaryOperatorType::LeftShift => Ok(l.wrapping_shl(r)),
        BinaryOperatorType::RightShift => Ok(l.wrapping_shr(r)),
        BinaryOperatorType::BitwiseAnd => Ok(l & r),
        BinaryOperatorType::BitwiseOr => Ok(l | r),
        BinaryOperatorType::BitwiseXor => Ok(l ^ r),
        BinaryOperatorType::LogicalAnd => Ok(((l != 0) && (r != 0)) as Operand),
        BinaryOperatorType::LogicalOr => Ok(((l != 0) || (r != 0)) as Operand),
    }
}

fn division_by_zero(expr: &Expr) -> Error {
    let location = expr.token().location;
    Error::from(location.line, location.column, "division by zero")
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::parser::Parser;

    fn no_symbols() -> HashMap<String, Operand> {
        HashMap::new()
    }

    fn eval_source(source: &str, symbols: &HashMap<String, Operand>) -> Result<Option<Operand>, Error> {
        let mut parser = Parser::new(source).unwrap();
        let expr = parser.parse_expr().unwrap();
        evaluate(&expr, symbols)
    }

    #[test]
    fn evaluate_number_literal() {
        assert_eq!(eval_source("42", &no_symbols()).unwrap(), Some(42));
        assert_eq!(eval_source("0x2a", &no_symbols()).unwrap(), Some(42));
        assert_eq!(eval_source("0b101010", &no_symbols()).unwrap(), Some(42));
        assert_eq!(eval_source("0o52", &no_symbols()).unwrap(), Some(42));
        assert_eq!(eval_source("'*'", &no_symbols()).unwrap(), Some(42));
    }

    #[test]
    fn evaluate_resolved_symbol() {
        let mut symbols = no_symbols();
        symbols.insert(String::from("foo"), 42);
        assert_eq!(eval_source("foo", &symbols).unwrap(), Some(42));
    }

    #[test]
    fn evaluate_unresolved_symbol_is_none_not_error() {
        assert_eq!(eval_source("foo", &no_symbols()).unwrap(), None);
    }

    #[test]
    fn evaluate_unresolved_symbol_propagates_through_operators() {
        assert_eq!(eval_source("foo + 1", &no_symbols()).unwrap(), None);
        assert_eq!(eval_source("1 + foo", &no_symbols()).unwrap(), None);
        assert_eq!(eval_source("-foo", &no_symbols()).unwrap(), None);
        assert_eq!(eval_source("(foo)", &no_symbols()).unwrap(), None);
        assert_eq!(eval_source("<foo", &no_symbols()).unwrap(), None);
    }

    #[test]
    fn evaluate_grouping() {
        assert_eq!(eval_source("(1 + 2) * 3", &no_symbols()).unwrap(), Some(9));
    }

    #[test]
    fn evaluate_precedence() {
        assert_eq!(eval_source("1 + 2 * 3", &no_symbols()).unwrap(), Some(7));
        assert_eq!(eval_source("2 | 1 & 0", &no_symbols()).unwrap(), Some(2));
    }

    #[test]
    fn evaluate_unary_operators() {
        assert_eq!(eval_source("+1", &no_symbols()).unwrap(), Some(1));
        assert_eq!(eval_source("-1", &no_symbols()).unwrap(), Some(1u32.wrapping_neg()));
        assert_eq!(eval_source("!0", &no_symbols()).unwrap(), Some(1));
        assert_eq!(eval_source("!1", &no_symbols()).unwrap(), Some(0));
        assert_eq!(eval_source("~0", &no_symbols()).unwrap(), Some(!0u32));
    }

    #[test]
    fn evaluate_lsb_msb_roundtrip_known_word() {
        let mut symbols = no_symbols();
        symbols.insert(String::from("label"), 0x1234);
        assert_eq!(eval_source("<label", &symbols).unwrap(), Some(0x34));
        assert_eq!(eval_source(">label", &symbols).unwrap(), Some(0x12));
    }

    #[test]
    fn evaluate_binary_arithmetic() {
        assert_eq!(eval_source("7 + 3", &no_symbols()).unwrap(), Some(10));
        assert_eq!(eval_source("7 - 3", &no_symbols()).unwrap(), Some(4));
        assert_eq!(eval_source("7 * 3", &no_symbols()).unwrap(), Some(21));
        assert_eq!(eval_source("7 / 3", &no_symbols()).unwrap(), Some(2));
        assert_eq!(eval_source("7 % 3", &no_symbols()).unwrap(), Some(1));
    }

    #[test]
    fn evaluate_binary_bitwise_and_shift() {
        assert_eq!(eval_source("0xff & 0x0f", &no_symbols()).unwrap(), Some(0x0f));
        assert_eq!(eval_source("0xf0 | 0x0f", &no_symbols()).unwrap(), Some(0xff));
        assert_eq!(eval_source("0xff ^ 0x0f", &no_symbols()).unwrap(), Some(0xf0));
        assert_eq!(eval_source("1 << 4", &no_symbols()).unwrap(), Some(0x10));
        assert_eq!(eval_source("0x10 >> 4", &no_symbols()).unwrap(), Some(1));
    }

    #[test]
    fn evaluate_binary_logical() {
        assert_eq!(eval_source("1 && 1", &no_symbols()).unwrap(), Some(1));
        assert_eq!(eval_source("1 && 0", &no_symbols()).unwrap(), Some(0));
        assert_eq!(eval_source("0 || 1", &no_symbols()).unwrap(), Some(1));
        assert_eq!(eval_source("0 || 0", &no_symbols()).unwrap(), Some(0));
    }

    #[test]
    fn evaluate_division_by_zero_is_error() {
        let result = eval_source("1 / 0", &no_symbols());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("division by zero"));
    }

    #[test]
    fn evaluate_remainder_by_zero_is_error() {
        let result = eval_source("1 % 0", &no_symbols());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("division by zero"));
    }

    #[test]
    fn evaluate_division_by_unresolved_symbol_is_none_not_error() {
        // the divisor isn't known to be zero yet, so this must not error prematurely
        assert_eq!(eval_source("1 / foo", &no_symbols()).unwrap(), None);
    }
}
