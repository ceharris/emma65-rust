use std::fmt;
use super::token::Token;

pub type Operand = u32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OperandWidth {
    Byte,
    Word,
    DWord,
}

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

#[derive(Clone, Debug, PartialEq)]
pub enum UnaryOperatorType {
    Identity,
    Negate,
    LogicalNot,
    BitwiseNot,
    Grouping,
    Fetch(OperandWidth),
}


#[derive(Clone)]
pub enum ExprType<'a> {
    Number(Operand),
    Register(Operand),
    Flag(Operand),
    Variable(Operand),
    Assign(Operand, Box<Expr<'a>>),
    UnaryOperator(UnaryOperatorType, Box<Expr<'a>>),
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

    pub fn token(&self) -> &Token<'a> {
        &self.token
    }

    pub fn expr_type(&self) -> &ExprType<'a> {
        &self.expr_type
    }

    pub fn is_signed(&self) -> bool {
        self.signed
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
