use super::expr::{BinaryOperatorType, Expr, ExprType, Operand, OperandWidth, UnaryOperatorType};


#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OpCode {
    PushImmediate(Operand),
    PushRegister(Operand),
    PushRegisterSigned(Operand),
    PushFlag(Operand),
    FetchByte,
    FetchByteSigned,
    FetchWord,
    FetchWordSigned,
    FetchDWord,
    FetchDWordSigned,
    Add,
    Subtract,
    Multiply,
    Divide,
    DivideSigned,
    Remainder,
    RemainderSigned,
    Negate,
    Equal,
    NotEqual,
    GreaterThan,
    GreaterThanSigned,
    GreaterOrEqual,
    GreaterOrEqualSigned,
    LessThan,
    LessThanSigned,
    LessOrEqual,
    LessOrEqualSigned,
    LeftShift,
    RightShift,
    RightShiftSigned,
    LogicalAnd,
    LogicalOr,
    LogicalNot,
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
    BitwiseNot,
    PushVariable(Operand),
    AssignAndPushVariable(Operand),
}

pub fn compile(root: Expr) -> Vec<OpCode> {
    let mut code: Vec<OpCode> = Vec::new();
    traverse(&root, false, &mut code);
    code
}

fn traverse(expr: &Expr, signed: bool, code: &mut Vec<OpCode>) {
    match expr.expr_type() {
        ExprType::Number(n) => code.push(OpCode::PushImmediate(*n)),
        ExprType::Flag(n) => code.push(OpCode::PushFlag(*n)),
        ExprType::Register(n) => code.push(if signed { OpCode::PushRegisterSigned(*n) } else { OpCode::PushRegister(*n) }),
        ExprType::Variable(id) => code.push(OpCode::PushVariable(*id)),
        ExprType::Assign(id, rhs) => {
            traverse(rhs, expr.is_signed(), code);
            code.push(OpCode::AssignAndPushVariable(*id));
        }
        ExprType::UnaryOperator(op_type, operand) => {
            traverse(operand, expr.is_signed(), code);
            match op_type {
                UnaryOperatorType::Identity => (),
                UnaryOperatorType::Grouping => (),
                UnaryOperatorType::Negate => code.push(OpCode::Negate),
                UnaryOperatorType::LogicalNot => code.push(OpCode::LogicalNot),
                UnaryOperatorType::BitwiseNot => code.push(OpCode::BitwiseNot),
                UnaryOperatorType::Fetch(width) => if signed {
                    match width {
                        OperandWidth::Byte => code.push(OpCode::FetchByteSigned),
                        OperandWidth::Word => code.push(OpCode::FetchWordSigned),
                        OperandWidth::DWord => code.push(OpCode::FetchDWordSigned),
                    }
                } else {
                    match width {
                        OperandWidth::Byte => code.push(OpCode::FetchByte),
                        OperandWidth::Word => code.push(OpCode::FetchWord),
                        OperandWidth::DWord => code.push(OpCode::FetchDWord),
                    }
                }
            }
        }
        ExprType::BinaryOperator(op_type, left, right) => {
            traverse(left, expr.is_signed(), code);
            traverse(right, expr.is_signed(), code);
            match op_type {
                BinaryOperatorType::Add => code.push(OpCode::Add),
                BinaryOperatorType::Subtract => code.push(OpCode::Subtract),
                BinaryOperatorType::Multiply => code.push(OpCode::Multiply),
                BinaryOperatorType::LogicalAnd => code.push(OpCode::LogicalAnd),
                BinaryOperatorType::LogicalOr => code.push(OpCode::LogicalOr),
                BinaryOperatorType::BitwiseAnd => code.push(OpCode::BitwiseAnd),
                BinaryOperatorType::BitwiseOr => code.push(OpCode::BitwiseOr),
                BinaryOperatorType::BitwiseXor => code.push(OpCode::BitwiseXor),
                BinaryOperatorType::LeftShift => code.push(OpCode::LeftShift),
                BinaryOperatorType::Equal => code.push(OpCode::Equal),
                BinaryOperatorType::NotEqual => code.push(OpCode::NotEqual),
                BinaryOperatorType::Divide => code.push(if expr.is_signed() { OpCode::DivideSigned } else { OpCode::Divide }),
                BinaryOperatorType::Remainder => code.push(if expr.is_signed() { OpCode::RemainderSigned } else { OpCode::Remainder}),
                BinaryOperatorType::RightShift => code.push(if expr.is_signed() { OpCode::RightShiftSigned } else { OpCode::RightShift }),
                BinaryOperatorType::GreaterThan => code.push(if expr.is_signed() { OpCode::GreaterThanSigned } else { OpCode::GreaterThan }),
                BinaryOperatorType::GreaterOrEqual => code.push(if expr.is_signed() { OpCode::GreaterOrEqualSigned } else { OpCode::GreaterOrEqual }),
                BinaryOperatorType::LessThan => code.push(if expr.is_signed() { OpCode::LessThanSigned } else { OpCode::LessThan }),
                BinaryOperatorType::LessOrEqual => code.push(if expr.is_signed() { OpCode::LessOrEqualSigned } else { OpCode::LessOrEqual }),
            }
        },
    }
}


#[cfg(test)]
mod tests {

    use super::*;
    use super::super::expr::Operand;
    use super::super::parser::Parser;

    fn register_mapper(name: &str) -> Option<Operand> {
        match name {
            "A" => Some(0),
            "PC" => Some(1),
            _ => None,
        }
    }

    fn flag_mapper(name: &str) -> Option<Operand> {
        match name {
            "C" => Some(0),
            _ => None,
        }
    }

    fn symbol_mapper(name: &str) -> Option<Operand> {
        match name {
            "foo" => Some(0x55),
            "bar" => Some(0xaa),
            _ => None,
        }
    }

    fn expr_for_source(source_text: &str) -> Expr<'_> {
        let mut vars = super::super::variables::Variables::new();
        Parser::from(register_mapper, flag_mapper, symbol_mapper).parse(source_text, &mut vars).unwrap().unwrap()
    }

    #[test]
    fn compile_constant() {
        let expr = expr_for_source("42");
        let code = compile(expr);
        assert_eq!(code.len(), 1);
        assert_eq!(code[0], OpCode::PushImmediate(42));
    }

    #[test]
    fn compile_register() {
        let expr = expr_for_source("A");
        let code = compile(expr);
        assert_eq!(code.len(), 1);
        assert_eq!(code[0], OpCode::PushRegister(0));
    }

    #[test]
    fn compile_register_signed() {
        let expr = expr_for_source("+A");
        let code = compile(expr);
        assert_eq!(code.len(), 1);
        assert_eq!(code[0], OpCode::PushRegisterSigned(0));
    }

    #[test]
    fn compile_flag() {
        let expr = expr_for_source("`C");
        let code = compile(expr);
        assert_eq!(code.len(), 1);
        assert_eq!(code[0], OpCode::PushFlag(0));
    }

    #[test]
    fn compile_fetch_memory_byte() {
        let expr = expr_for_source("b[42]");
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::FetchByte);
    }

    #[test]
    fn compile_fetch_memory_byte_signed() {
        let expr = expr_for_source("+b[42]");
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::FetchByteSigned);
    }

    #[test]
    fn compile_fetch_memory_word() {
        let expr = expr_for_source("w[42]");
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::FetchWord);
    }

    #[test]
    fn compile_fetch_memory_word_signed() {
        let expr = expr_for_source("+w[42]");
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::FetchWordSigned);
    }

    #[test]
    fn compile_fetch_memory_dword() {
        let expr = expr_for_source("d[42]");
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::FetchDWord);
    }

    #[test]
    fn compile_fetch_memory_dword_signed() {
        let expr = expr_for_source("+d[42]");
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::FetchDWordSigned);
    }

    #[test]
    fn compile_grouping() {
        let expr = expr_for_source("(42)");
        let code = compile(expr);
        assert_eq!(code.len(), 1);
        assert_eq!(code[0], OpCode::PushImmediate(42));
    }

    #[test]
    fn compile_identity() {
        let expr = expr_for_source("+42");
        let code = compile(expr);
        assert_eq!(code.len(), 1);
        assert_eq!(code[0], OpCode::PushImmediate(42));
    }

    #[test]
    fn compile_negate() {
        let expr = expr_for_source("-42");
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::Negate);
    }

    #[test]
    fn compile_logical_not() {
        let expr = expr_for_source("!42");
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::LogicalNot);
    }

    #[test]
    fn compile_bitwise_not() {
        let expr = expr_for_source("~42");
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::BitwiseNot);
    }

    #[test]
    fn compile_add() {
        let expr = expr_for_source("42 + 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::Add);
    }

    #[test]
    fn compile_subtract() {
        let expr = expr_for_source("42 - 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::Subtract);
    }

    #[test]
    fn compile_multiply() {
        let expr = expr_for_source("42 * 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::Multiply);
    }

    #[test]
    fn compile_divide() {
        let expr = expr_for_source("42 / 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::Divide);
    }

    #[test]
    fn compile_divide_signed() {
        let expr = expr_for_source("+42 / 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::DivideSigned);
    }

    #[test]
    fn compile_remainder() {
        let expr = expr_for_source("42 % 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::Remainder);
    }

    #[test]
    fn compile_remainder_signed() {
        let expr = expr_for_source("+42 % 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::RemainderSigned);
    }

    #[test]
    fn compile_left_shift() {
        let expr = expr_for_source("42 << 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::LeftShift);
    }

    #[test]
    fn compile_right_shift() {
        let expr = expr_for_source("42 >> 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::RightShift);
    }

    #[test]
    fn compile_right_shift_signed() {
        let expr = expr_for_source("+42 >> 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::RightShiftSigned);
    }

    #[test]
    fn compile_bitwise_and() {
        let expr = expr_for_source("42 & 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::BitwiseAnd);
    }

    #[test]
    fn compile_bitwise_or() {
        let expr = expr_for_source("42 | 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::BitwiseOr);
    }

    #[test]
    fn compile_bitwise_xor() {
        let expr = expr_for_source("42 ^ 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::BitwiseXor);
    }

    #[test]
    fn compile_greater_than() {
        let expr = expr_for_source("42 > 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::GreaterThan);
    }

    #[test]
    fn compile_greater_than_signed() {
        let expr = expr_for_source("+42 > 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::GreaterThanSigned);
    }

    #[test]
    fn compile_greater_or_equal() {
        let expr = expr_for_source("42 >= 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::GreaterOrEqual);
    }

    #[test]
    fn compile_greater_or_equal_signed() {
        let expr = expr_for_source("+42 >= 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::GreaterOrEqualSigned);
    }

    #[test]
    fn compile_less_than() {
        let expr = expr_for_source("42 < 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::LessThan);
    }

    #[test]
    fn compile_less_than_signed() {
        let expr = expr_for_source("+42 < 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::LessThanSigned);
    }

    #[test]
    fn compile_less_or_equal() {
        let expr = expr_for_source("42 <= 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::LessOrEqual);
    }

    #[test]
    fn compile_less_or_equal_signed() {
        let expr = expr_for_source("+42 <= 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::LessOrEqualSigned);
    }

    #[test]
    fn compile_equal() {
        let expr = expr_for_source("42 == 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::Equal);
    }

    #[test]
    fn compile_not_equal() {
        let expr = expr_for_source("42 != 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::NotEqual);
    }

    #[test]
    fn compile_logical_and() {
        let expr = expr_for_source("42 && 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::LogicalAnd);
    }

    #[test]
    fn compile_logical_or() {
        let expr = expr_for_source("42 || 43");
        let code = compile(expr);
        assert_eq!(code.len(), 3);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::PushImmediate(43));
        assert_eq!(code[2], OpCode::LogicalOr);
    }

    #[test]
    fn compile_expression() {
        let expr = expr_for_source("PC == $04c2 && (A == foo || A == bar) && `C");
        let code = compile(expr);
        assert_eq!(code.len(), 13);
        assert_eq!(code[0], OpCode::PushRegister(1));
        assert_eq!(code[1], OpCode::PushImmediate(0x04c2));
        assert_eq!(code[2], OpCode::Equal);
        assert_eq!(code[3], OpCode::PushRegister(0));
        assert_eq!(code[4], OpCode::PushImmediate(0x55));
        assert_eq!(code[5], OpCode::Equal);
        assert_eq!(code[6], OpCode::PushRegister(0));
        assert_eq!(code[7], OpCode::PushImmediate(0xaa));
        assert_eq!(code[8], OpCode::Equal);
        assert_eq!(code[9], OpCode::LogicalOr);
        assert_eq!(code[10], OpCode::LogicalAnd);
        assert_eq!(code[11], OpCode::PushFlag(0));
        assert_eq!(code[12], OpCode::LogicalAnd);
    }

    fn expr_for_source_with_vars<'a>(source_text: &'a str, vars: &mut super::super::variables::Variables) -> super::super::expr::Expr<'a> {
        Parser::from(register_mapper, flag_mapper, symbol_mapper).parse(source_text, vars).unwrap().unwrap()
    }

    #[test]
    fn compile_walrus_assign() {
        let mut vars = super::super::variables::Variables::new();
        let expr = expr_for_source_with_vars("x := 42", &mut vars);
        let id = vars.get("x").unwrap();
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushImmediate(42));
        assert_eq!(code[1], OpCode::AssignAndPushVariable(id));
    }

    #[test]
    fn compile_variable_read() {
        let mut vars = super::super::variables::Variables::new();
        expr_for_source_with_vars("x := 0", &mut vars);
        let expr = expr_for_source_with_vars("x", &mut vars);
        let id = vars.get("x").unwrap();
        let code = compile(expr);
        assert_eq!(code.len(), 1);
        assert_eq!(code[0], OpCode::PushVariable(id));
    }

    #[test]
    fn compile_walrus_with_register_rhs() {
        let mut vars = super::super::variables::Variables::new();
        let expr = expr_for_source_with_vars("x := A", &mut vars);
        let id = vars.get("x").unwrap();
        let code = compile(expr);
        assert_eq!(code.len(), 2);
        assert_eq!(code[0], OpCode::PushRegister(0));
        assert_eq!(code[1], OpCode::AssignAndPushVariable(id));
    }

}
