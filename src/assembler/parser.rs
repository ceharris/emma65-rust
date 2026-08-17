use super::error::Error;
use super::expr::{BinaryOperatorType, Expr, UnaryOperatorType};
use super::scanner::Scanner;
use super::token::{Token, TokenType};

/// A recursive-descent parser that turns a token stream into an [`Expr`] tree.
///
/// Unlike `watch::parser::Parser`, this parser does not resolve symbols at parse
/// time — `Expr::Symbol` nodes carry the symbol's name and are resolved later,
/// once per assembler pass, since forward references mean a symbol's value may
/// not be known yet when its use is parsed.
pub struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    current: usize,
}

impl<'a> Parser<'a> {

    pub fn new(source: &'a str) -> Result<Self, Error> {
        let tokens = Scanner::new(source).scan()?;
        Ok(Self { tokens, current: 0 })
    }

    /// Parses a single expression starting at the current token position, leaving
    /// any trailing tokens (a comma, a closing delimiter, a newline, ...) unconsumed
    /// for the caller (the operand/statement grammar built in later units).
    pub fn parse_expr(&mut self) -> Result<Expr<'a>, Error> {
        self.parse_logical_or()
    }

    pub fn is_at_end(&self) -> bool {
        self.current == self.tokens.len()
    }

    pub fn peek(&self) -> Option<&Token<'a>> {
        if self.is_at_end() {
            None
        } else {
            Some(&self.tokens[self.current])
        }
    }

    fn parse_logical_or(&mut self) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_logical_and()?;
        while let Some(op) = self.match_token(&[TokenType::BarBar]) {
            let right = self.parse_logical_and()?;
            left = Expr::binary(&op, BinaryOperatorType::LogicalOr, left, right);
        }
        Ok(left)
    }

    fn parse_logical_and(&mut self) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_bitwise_or()?;
        while let Some(op) = self.match_token(&[TokenType::AmperAmper]) {
            let right = self.parse_bitwise_or()?;
            left = Expr::binary(&op, BinaryOperatorType::LogicalAnd, left, right);
        }
        Ok(left)
    }

    fn parse_bitwise_or(&mut self) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_bitwise_xor()?;
        while let Some(op) = self.match_token(&[TokenType::Bar]) {
            let right = self.parse_bitwise_xor()?;
            left = Expr::binary(&op, BinaryOperatorType::BitwiseOr, left, right);
        }
        Ok(left)
    }

    fn parse_bitwise_xor(&mut self) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_bitwise_and()?;
        while let Some(op) = self.match_token(&[TokenType::Caret]) {
            let right = self.parse_bitwise_and()?;
            left = Expr::binary(&op, BinaryOperatorType::BitwiseXor, left, right);
        }
        Ok(left)
    }

    fn parse_bitwise_and(&mut self) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_shift()?;
        while let Some(op) = self.match_token(&[TokenType::Amper]) {
            let right = self.parse_shift()?;
            left = Expr::binary(&op, BinaryOperatorType::BitwiseAnd, left, right);
        }
        Ok(left)
    }

    fn parse_shift(&mut self) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_term()?;
        while let Some(op) = self.match_token(&[TokenType::LesserLesser, TokenType::GreaterGreater]) {
            let right = self.parse_term()?;
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right);
        }
        Ok(left)
    }

    fn parse_term(&mut self) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_factor()?;
        while let Some(op) = self.match_token(&[TokenType::Plus, TokenType::Minus]) {
            let right = self.parse_factor()?;
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right);
        }
        Ok(left)
    }

    fn parse_factor(&mut self) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_unary()?;
        while let Some(op) = self.match_token(&[TokenType::Star, TokenType::Slash, TokenType::Percent]) {
            let right = self.parse_unary()?;
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right);
        }
        Ok(left)
    }

    fn binary_operator(token_type: &TokenType) -> BinaryOperatorType {
        match token_type {
            TokenType::Amper => BinaryOperatorType::BitwiseAnd,
            TokenType::AmperAmper => BinaryOperatorType::LogicalAnd,
            TokenType::Bar => BinaryOperatorType::BitwiseOr,
            TokenType::BarBar => BinaryOperatorType::LogicalOr,
            TokenType::Caret => BinaryOperatorType::BitwiseXor,
            TokenType::GreaterGreater => BinaryOperatorType::RightShift,
            TokenType::LesserLesser => BinaryOperatorType::LeftShift,
            TokenType::Minus => BinaryOperatorType::Subtract,
            TokenType::Percent => BinaryOperatorType::Remainder,
            TokenType::Plus => BinaryOperatorType::Add,
            TokenType::Slash => BinaryOperatorType::Divide,
            TokenType::Star => BinaryOperatorType::Multiply,
            _ => panic!("{token_type:?} is not a binary operator"),
        }
    }

    fn parse_unary(&mut self) -> Result<Expr<'a>, Error> {
        if let Some(op) = self.match_token(&[
            TokenType::Minus, TokenType::Plus, TokenType::Bang, TokenType::Tilde,
            TokenType::Lesser, TokenType::Greater,
        ]) {
            let operand = self.parse_unary()?;
            Ok(Expr::unary(&op, Self::unary_operator(op.token_type()), operand))
        } else {
            self.parse_primary()
        }
    }

    fn unary_operator(token_type: &TokenType) -> UnaryOperatorType {
        match token_type {
            TokenType::Bang => UnaryOperatorType::LogicalNot,
            TokenType::Greater => UnaryOperatorType::Msb,
            TokenType::Lesser => UnaryOperatorType::Lsb,
            TokenType::Minus => UnaryOperatorType::Negate,
            TokenType::Plus => UnaryOperatorType::Identity,
            TokenType::Tilde => UnaryOperatorType::BitwiseNot,
            _ => panic!("{token_type:?} is not a unary operator"),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr<'a>, Error> {
        match self.peek().cloned() {
            Some(token) => match token.token_type() {
                TokenType::Symbol(name) => {
                    self.advance();
                    Ok(Expr::symbol(&token, name.clone()))
                }
                TokenType::Number(n) => {
                    self.advance();
                    Ok(Expr::number(&token, *n))
                }
                TokenType::LeftParen => self.parse_grouping(),
                _ => Err(Error::from(token.location.line, token.location.column,
                                     "misplaced or unrecognized token")),
            },
            None => Err(Error::from(0, 0, "expected operand")),
        }
    }

    fn parse_grouping(&mut self) -> Result<Expr<'a>, Error> {
        let op = self.advance().unwrap(); // consume the opening parenthesis
        let inner = self.parse_expr()?;
        match self.advance() {
            Some(token) if token.token_type() == TokenType::RightParen => {
                Ok(Expr::grouping(&op, inner))
            }
            Some(token) => Err(Error::from(
                token.location.line, token.location.column,
                "expected closing parenthesis")),
            None => Err(Error::from(
                op.location.line, op.location.column,
                "expected closing parenthesis")),
        }
    }

    fn match_token(&mut self, types: &[TokenType]) -> Option<Token<'a>> {
        if !self.is_at_end() && types.iter().any(|t| t == self.tokens[self.current].token_type()) {
            self.advance()
        } else {
            None
        }
    }

    fn advance(&mut self) -> Option<Token<'a>> {
        if self.is_at_end() {
            None
        } else {
            let token = self.tokens[self.current].clone();
            self.current += 1;
            Some(token)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::expr::ExprType;
    use super::*;

    fn parse(source: &str) -> Result<Expr<'_>, Error> {
        let mut parser = Parser::new(source)?;
        let expr = parser.parse_expr()?;
        Ok(expr)
    }

    #[test]
    fn parse_number() {
        let expr = parse("42").unwrap();
        assert_eq!(expr.expr_type(), &ExprType::Number(42));
    }

    #[test]
    fn parse_symbol() {
        let expr = parse("foo").unwrap();
        assert_eq!(expr.expr_type(), &ExprType::Symbol(String::from("foo")));
    }

    #[test]
    fn parse_grouping() {
        let expr = parse("(42)").unwrap();
        match expr.expr_type() {
            ExprType::Grouping(inner) => assert_eq!(inner.expr_type(), &ExprType::Number(42)),
            _ => panic!("expected Grouping, got {:?}", expr),
        }
        assert!(parse("(").is_err());
        assert!(parse("()").is_err());
        assert!(parse("(42").is_err());
        assert!(parse("(42 foo").is_err());
    }

    fn validate_unary(operator_text: &str, expected_operator_type: &UnaryOperatorType) {
        let source = format!("{operator_text}42");
        let expr = parse(&source).unwrap();
        match expr.expr_type() {
            ExprType::Unary(operator_type, operand) => {
                assert_eq!(operator_type, expected_operator_type);
                assert_eq!(operand.expr_type(), &ExprType::Number(42));
            }
            _ => panic!("expected Unary, got {:?}", expr),
        }
    }

    #[test]
    fn parse_unary_operators() {
        validate_unary("!", &UnaryOperatorType::LogicalNot);
        validate_unary("-", &UnaryOperatorType::Negate);
        validate_unary("+", &UnaryOperatorType::Identity);
        validate_unary("~", &UnaryOperatorType::BitwiseNot);
        validate_unary("<", &UnaryOperatorType::Lsb);
        validate_unary(">", &UnaryOperatorType::Msb);
    }

    #[test]
    fn parse_unary_prefix_on_symbol() {
        let expr = parse("<label").unwrap();
        match expr.expr_type() {
            ExprType::Unary(UnaryOperatorType::Lsb, operand) => {
                assert_eq!(operand.expr_type(), &ExprType::Symbol(String::from("label")));
            }
            _ => panic!("expected Unary Lsb, got {:?}", expr),
        }
    }

    fn validate_binary(operator_text: &str, expected_operator_type: &BinaryOperatorType) {
        let source = format!("foo {operator_text} 42");
        let expr = parse(&source).unwrap();
        match expr.expr_type() {
            ExprType::Binary(operator_type, left, right) => {
                assert_eq!(operator_type, expected_operator_type);
                assert_eq!(left.expr_type(), &ExprType::Symbol(String::from("foo")));
                assert_eq!(right.expr_type(), &ExprType::Number(42));
            }
            _ => panic!("expected Binary, got {:?}", expr),
        }
    }

    #[test]
    fn parse_binary_operators() {
        validate_binary("&", &BinaryOperatorType::BitwiseAnd);
        validate_binary("&&", &BinaryOperatorType::LogicalAnd);
        validate_binary("|", &BinaryOperatorType::BitwiseOr);
        validate_binary("||", &BinaryOperatorType::LogicalOr);
        validate_binary("^", &BinaryOperatorType::BitwiseXor);
        validate_binary(">>", &BinaryOperatorType::RightShift);
        validate_binary("<<", &BinaryOperatorType::LeftShift);
        validate_binary("-", &BinaryOperatorType::Subtract);
        validate_binary("%", &BinaryOperatorType::Remainder);
        validate_binary("+", &BinaryOperatorType::Add);
        validate_binary("/", &BinaryOperatorType::Divide);
        validate_binary("*", &BinaryOperatorType::Multiply);
    }

    #[test]
    fn parse_precedence() {
        // multiplication binds tighter than addition
        let expr = parse("1 + 2 * 3").unwrap();
        match expr.expr_type() {
            ExprType::Binary(BinaryOperatorType::Add, left, right) => {
                assert_eq!(left.expr_type(), &ExprType::Number(1));
                match right.expr_type() {
                    ExprType::Binary(BinaryOperatorType::Multiply, l, r) => {
                        assert_eq!(l.expr_type(), &ExprType::Number(2));
                        assert_eq!(r.expr_type(), &ExprType::Number(3));
                    }
                    _ => panic!("expected Multiply, got {:?}", right),
                }
            }
            _ => panic!("expected Add, got {:?}", expr),
        }
    }

    #[test]
    fn parse_lesser_vs_left_shift_disambiguation() {
        let expr = parse("1 << 2").unwrap();
        match expr.expr_type() {
            ExprType::Binary(BinaryOperatorType::LeftShift, _, _) => {}
            _ => panic!("expected LeftShift, got {:?}", expr),
        }
        let expr = parse("<1").unwrap();
        match expr.expr_type() {
            ExprType::Unary(UnaryOperatorType::Lsb, _) => {}
            _ => panic!("expected Lsb, got {:?}", expr),
        }
    }

    #[test]
    fn parse_leaves_trailing_tokens_unconsumed() {
        let mut parser = Parser::new("42, X").unwrap();
        let expr = parser.parse_expr().unwrap();
        assert_eq!(expr.expr_type(), &ExprType::Number(42));
        assert!(!parser.is_at_end());
        assert_eq!(parser.peek().unwrap().token_type(), &TokenType::Comma);
    }

    #[test]
    fn parse_expected_operand_error() {
        assert!(parse("").is_err());
        assert!(parse("+").is_err());
    }

    #[test]
    fn parse_misplaced_token_error() {
        let result = parse(")");
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("misplaced"));
    }
}
