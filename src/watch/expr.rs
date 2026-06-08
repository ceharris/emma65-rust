use std::fmt;
use super::token::Token;

/// The data type used for all watch expressions. 
/// 
/// * All operands are coerced to this type. 
/// * All operators have this as the result type.
/// 
pub type Operand = u32;

/// The width of a return value for a memory fetch operator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FetchWidth {
    /// A single byte
    Byte,
    /// A word; two consecutive bytes in the architecture's byte order
    Word,
    /// A double word; four consecutive bytes in the architecture's byte order
    DWord,
}

/// Type identifiers used to represent the operation implied by a token representing
/// a binary operator.
/// 
#[derive(Clone, Debug, PartialEq)]
pub enum BinaryOperatorType {
    Add,
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
    Equal,
    Divide,
    GreaterThan,
    GreaterOrEqual,
    LeftShift,
    LessThan,
    LessOrEqual,
    LogicalAnd,
    LogicalOr,
    Multiply,
    Remainder,
    RightShift,
    Subtract,
    NotEqual,
}

/// Type identifiers used to represent the operation implied by a token representing
/// a unary operator.
///
#[derive(Clone, Debug, PartialEq)]
pub enum UnaryOperatorType {
    Identity,
    Negate,
    LogicalNot,
    BitwiseNot,
    Grouping,
    Fetch(FetchWidth),
}

/// An expression type parsed from a token subsequence.
#[derive(Clone)]
pub enum ExprType<'a> {
    /// A number whose value is given by the operand
    Number(Operand),
    /// A register whose identity is given by the operand 
    Register(Operand),
    /// A flag whose identity is given by the operand
    Flag(Operand),
    /// A variable whose identity is given by the operand
    Variable(Operand),
    /// A variable assignment, whose left-hand side is the variable whose identity is given by the
    /// operand and whose value is represented by the given expression.
    Assign(Operand, Box<Expr<'a>>),
    /// A unary operator of the specified type, whose operand is represented by the given expression.
    UnaryOperator(UnaryOperatorType, Box<Expr<'a>>),
    /// A binary operator of the specified type, whose left and right operands are given by the 
    /// given expressions, respectively.
    BinaryOperator(BinaryOperatorType, Box<Expr<'a>>, Box<Expr<'a>>),
}

impl PartialEq for ExprType<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ExprType::Number(a), ExprType::Number(b)) => a == b,
            (ExprType::Register(a), ExprType::Register(b)) => a == b,
            (ExprType::Flag(a), ExprType::Flag(b)) => a == b,
            (ExprType::Variable(a), ExprType::Variable(b)) => a == b,
            (ExprType::Assign(a, ae), ExprType::Assign(b, be)) => a == b && ae == be,
            (ExprType::UnaryOperator(a1, a2), ExprType::UnaryOperator(b1, b2)) => a1 == b1 && a2 == b2,
            (ExprType::BinaryOperator(a1, a2, a3), ExprType::BinaryOperator(b1, b2, b3)) => a1 == b1 && a2 == b2 && a3 == b3,
            _ => false,
        }
    }
}

impl fmt::Debug for ExprType<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExprType::Number(value) => write!(f, "(number {:?})", value),
            ExprType::Register(reg) => write!(f, "(register {:?})", reg),
            ExprType::Flag(flag) => write!(f, "(flag {:?})", flag),
            ExprType::Variable(id) => write!(f, "(variable {:?})", id),
            ExprType::Assign(id, rhs) => write!(f, "(assign {:?} {:?})", id, rhs),
            ExprType::UnaryOperator(op_type, operand) => write!(f, "{:?} {:?}", op_type, operand),
            ExprType::BinaryOperator(op_type, left, right) => write!(f, "{:?} {:?} {:?}", op_type, left, right),
        }
    }
}

#[derive(Clone)]
pub struct Expr<'a> {
    token: Token<'a>,
    expr_type: ExprType<'a>,
    signed: bool,
}

impl<'a> Expr<'a> {

    pub fn number(token: &Token<'a>, value: u32) -> Self {
        Self {
            token: token.clone(),
            expr_type: ExprType::Number(value),
            signed: false,
        }
    }

    pub fn register(token: &Token<'a>, reg: Operand) -> Self {
        Self {
            token: token.clone(),
            expr_type: ExprType::Register(reg),
            signed: false,
        }
    }

    pub fn flag(token: &Token<'a>, flag: Operand) -> Self {
        Self {
            token: token.clone(),
            expr_type: ExprType::Flag(flag),
            signed: false,
        }
    }

    pub fn variable(token: &Token<'a>, id: Operand) -> Self {
        Self {
            token: token.clone(),
            expr_type: ExprType::Variable(id),
            signed: false,
        }
    }

    pub fn assign(token: &Token<'a>, id: Operand, rhs: Expr<'a>) -> Self {
        let signed = rhs.is_signed();
        Self {
            token: token.clone(),
            expr_type: ExprType::Assign(id, Box::new(rhs)),
            signed,
        }
    }

    pub fn unary(op: &Token<'a>, op_type: UnaryOperatorType, operand: Expr<'a>, signed: bool) -> Self {
        Self {
            token: op.clone(),
            expr_type: ExprType::UnaryOperator(op_type, Box::new(operand)),
            signed,
        }
    }

    pub fn binary(op: &Token<'a>, op_type: BinaryOperatorType, left: Expr<'a>, right: Expr<'a>, signed: bool) -> Self {
        Self {
            token: op.clone(),
            expr_type: ExprType::BinaryOperator(op_type, Box::new(left), Box::new(right)),
            signed,
        }
    }

    pub fn expr_type(&self) -> &ExprType<'a> {
        &self.expr_type
    }

    pub fn is_signed(&self) -> bool {
        self.signed
    }
}

#[cfg(test)]
impl<'a> Expr<'a> {
    pub fn token(&self) -> &Token<'a> {
        &self.token
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
            ExprType::UnaryOperator(_, _) => {
                write!(f, "({} {:?})", self.token.text(), self.expr_type)
            }
            ExprType::BinaryOperator(_, _, _) => {
                write!(f, "({} {:?})", self.token.text(), self.expr_type)
            }
            _ => write!(f, "{:?}", self.expr_type),
        }
    }
}
