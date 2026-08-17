use super::token::Token;
use std::fmt;

/// The data type used for all assembler expressions.
///
/// * All operands are coerced to this type.
/// * All operators have this as the result type.
///
pub type Operand = u32;

/// Type identifiers used to represent the operation implied by a token representing
/// a unary operator.
#[derive(Clone, Debug, PartialEq)]
pub enum UnaryOperatorType {
    Identity,
    Negate,
    LogicalNot,
    BitwiseNot,
    /// `<expr` — extracts the low byte (LSB) of the operand's value.
    Lsb,
    /// `>expr` — extracts the high byte (MSB) of the operand's value.
    Msb,
}

/// Type identifiers used to represent the operation implied by a token representing
/// a binary operator.
#[derive(Clone, Debug, PartialEq)]
pub enum BinaryOperatorType {
    Add,
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
    Divide,
    LeftShift,
    LogicalAnd,
    LogicalOr,
    Multiply,
    Remainder,
    RightShift,
    Subtract,
}

/// An expression type parsed from a token subsequence.
#[derive(Clone)]
pub enum ExprType<'a> {
    /// A literal number whose value is given by the operand.
    Number(Operand),
    /// A reference to a named symbol, resolved during evaluation.
    Symbol(String),
    /// A unary operator of the specified type, whose operand is represented by the given expression.
    Unary(UnaryOperatorType, Box<Expr<'a>>),
    /// A binary operator of the specified type, whose left and right operands are given by the
    /// given expressions, respectively.
    Binary(BinaryOperatorType, Box<Expr<'a>>, Box<Expr<'a>>),
    /// A parenthesized subexpression.
    Grouping(Box<Expr<'a>>),
}

impl PartialEq for ExprType<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ExprType::Number(a), ExprType::Number(b)) => a == b,
            (ExprType::Symbol(a), ExprType::Symbol(b)) => a == b,
            (ExprType::Unary(a1, a2), ExprType::Unary(b1, b2)) => a1 == b1 && a2 == b2,
            (ExprType::Binary(a1, a2, a3), ExprType::Binary(b1, b2, b3)) => a1 == b1 && a2 == b2 && a3 == b3,
            (ExprType::Grouping(a), ExprType::Grouping(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Debug for ExprType<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprType::Number(value) => write!(f, "(number {:?})", value),
            ExprType::Symbol(name) => write!(f, "(symbol {:?})", name),
            ExprType::Unary(op_type, operand) => write!(f, "{:?} {:?}", op_type, operand),
            ExprType::Binary(op_type, left, right) => write!(f, "{:?} {:?} {:?}", op_type, left, right),
            ExprType::Grouping(inner) => write!(f, "(group {:?})", inner),
        }
    }
}

#[derive(Clone)]
pub struct Expr<'a> {
    token: Token<'a>,
    expr_type: ExprType<'a>,
}

impl<'a> Expr<'a> {

    pub fn number(token: &Token<'a>, value: Operand) -> Self {
        Self {
            token: token.clone(),
            expr_type: ExprType::Number(value),
        }
    }

    pub fn symbol(token: &Token<'a>, name: String) -> Self {
        Self {
            token: token.clone(),
            expr_type: ExprType::Symbol(name),
        }
    }

    pub fn unary(op: &Token<'a>, op_type: UnaryOperatorType, operand: Expr<'a>) -> Self {
        Self {
            token: op.clone(),
            expr_type: ExprType::Unary(op_type, Box::new(operand)),
        }
    }

    pub fn binary(op: &Token<'a>, op_type: BinaryOperatorType, left: Expr<'a>, right: Expr<'a>) -> Self {
        Self {
            token: op.clone(),
            expr_type: ExprType::Binary(op_type, Box::new(left), Box::new(right)),
        }
    }

    pub fn grouping(op: &Token<'a>, inner: Expr<'a>) -> Self {
        Self {
            token: op.clone(),
            expr_type: ExprType::Grouping(Box::new(inner)),
        }
    }

    pub fn token(&self) -> &Token<'a> {
        &self.token
    }

    pub fn expr_type(&self) -> &ExprType<'a> {
        &self.expr_type
    }
}

impl PartialEq for Expr<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.expr_type == other.expr_type
    }
}

impl fmt::Debug for Expr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.expr_type {
            ExprType::Unary(_, _) | ExprType::Binary(_, _, _) => {
                write!(f, "({} {:?})", self.token.text(), self.expr_type)
            }
            _ => write!(f, "{:?}", self.expr_type),
        }
    }
}
