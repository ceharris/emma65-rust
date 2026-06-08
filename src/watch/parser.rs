use super::error::{Error};
use super::expr::{BinaryOperatorType, Expr, Operand, FetchWidth, UnaryOperatorType};
use super::scanner::Scanner;
use super::token::{Token, TokenType};
use super::variables::Variables;

pub type Mapper = Box<dyn Fn(&str) -> Option<Operand>>;

pub struct Parser {
    map_register: Mapper,
    map_flag: Mapper,
    map_symbol: Mapper,
}

impl Parser {

    pub fn from(
        map_register: impl Fn(&str) -> Option<Operand> + 'static,
        map_flag: impl Fn(&str) -> Option<Operand> + 'static,
        map_symbol: impl Fn(&str) -> Option<Operand> + 'static,
    ) -> Self {
        Self {
            map_register: Box::new(map_register),
            map_flag: Box::new(map_flag),
            map_symbol: Box::new(map_symbol),
        }
    }

    pub fn parse<'a>(&self, source: &'a str, vars: &mut Variables) -> Result<Option<Expr<'a>>, Error> {
        let tokens = Scanner::new(source).scan()?;
        if tokens.is_empty() {
            return Ok(None);
        }
        let mut state = ParseState { tokens, current: 0, parser: self };
        let expr = state.parse_statement(vars)?;
        if !state.is_at_end() {
            let token = state.peek().unwrap();
            Err(Error::from(token.location.line, token.location.column, "unexpected token"))
        } else {
            Ok(Some(expr))
        }
    }
}

struct ParseState<'a, 'p> {
    tokens: Vec<Token<'a>>,
    current: usize,
    parser: &'p Parser,
}

impl<'a, 'p> ParseState<'a, 'p> {

    fn parse_statement(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let expr = self.parse_assignment(vars)?;
        self.match_token(&[TokenType::Semicolon]);
        Ok(expr)
    }

    fn parse_assignment(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        if let (Some(name_token), true) = (
            self.peek().cloned(),
            self.peek_next().map_or(false, |t| t.token_type() == &TokenType::Walrus),
        ) {
            if let TokenType::Symbol(name) = name_token.token_type().clone() {
                self.advance(); // consume symbol
                self.advance(); // consume :=
                let id = vars.get_or_create(&name);
                let rhs = self.parse_next(vars)?;
                return Ok(Expr::assign(&name_token, id, rhs));
            }
        }
        self.parse_logical_or(vars)
    }

    fn parse_next(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        self.parse_assignment(vars)
    }

    fn parse_logical_or(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_logical_and(vars)?;
        while let Some(op) = self.match_token(&[TokenType::BarBar]) {
            let right = self.parse_logical_and(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn parse_logical_and(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_bitwise_or(vars)?;
        while let Some(op) = self.match_token(&[TokenType::AmperAmper]) {
            let right = self.parse_bitwise_or(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn parse_bitwise_or(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_bitwise_xor(vars)?;
        while let Some(op) = self.match_token(&[TokenType::Bar]) {
            let right = self.parse_bitwise_xor(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn parse_bitwise_xor(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_bitwise_and(vars)?;
        while let Some(op) = self.match_token(&[TokenType::Caret]) {
            let right = self.parse_bitwise_and(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn parse_bitwise_and(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_equality(vars)?;
        while let Some(op) = self.match_token(&[TokenType::Amper]) {
            let right = self.parse_equality(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn parse_equality(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_relational(vars)?;
        while let Some(op) = self.match_token(&[TokenType::EqualEqual, TokenType::BangEqual, TokenType::Equal]) {
            let right = self.parse_relational(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn parse_relational(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_shift(vars)?;
        while let Some(op) = self.match_token(&[TokenType::Greater, TokenType::GreaterEqual, TokenType::Lesser, TokenType::LesserEqual]) {
            let right = self.parse_shift(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn parse_shift(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_term(vars)?;
        while let Some(op) = self.match_token(&[TokenType::GreaterGreater, TokenType::LesserLesser]) {
            let right = self.parse_term(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn parse_term(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_factor(vars)?;
        while let Some(op) = self.match_token(&[TokenType::Plus, TokenType::Minus]) {
            let right = self.parse_factor(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn parse_factor(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let mut left = self.parse_unary(vars)?;
        while let Some(op) = self.match_token(&[TokenType::Percent, TokenType::Slash, TokenType::Star]) {
            let right = self.parse_unary(vars)?;
            let signed = left.is_signed() || right.is_signed();
            left = Expr::binary(&op, Self::binary_operator(op.token_type()), left, right, signed);
        }
        Ok(left)
    }

    fn binary_operator(token_type: &TokenType) -> BinaryOperatorType {
        match token_type {
            TokenType::Amper => BinaryOperatorType::BitwiseAnd,
            TokenType::AmperAmper => BinaryOperatorType::LogicalAnd,
            TokenType::BangEqual => BinaryOperatorType::NotEqual,
            TokenType::Bar => BinaryOperatorType::BitwiseOr,
            TokenType::BarBar => BinaryOperatorType::LogicalOr,
            TokenType::Caret => BinaryOperatorType::BitwiseXor,
            TokenType::Equal | TokenType::EqualEqual => BinaryOperatorType::Equal,
            TokenType::Greater => BinaryOperatorType::GreaterThan,
            TokenType::GreaterEqual => BinaryOperatorType::GreaterOrEqual,
            TokenType::GreaterGreater => BinaryOperatorType::RightShift,
            TokenType::Lesser => BinaryOperatorType::LessThan,
            TokenType::LesserEqual => BinaryOperatorType::LessOrEqual,
            TokenType::LesserLesser => BinaryOperatorType::LeftShift,
            TokenType::Minus => BinaryOperatorType::Subtract,
            TokenType::Percent => BinaryOperatorType::Remainder,
            TokenType::Plus => BinaryOperatorType::Add,
            TokenType::Slash => BinaryOperatorType::Divide,
            TokenType::Star => BinaryOperatorType::Multiply,
            _ => panic!("{token_type:?} is not a binary operator"),
        }
    }

    fn parse_unary(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        if let Some(op) = self.match_token(&[TokenType::Bang, TokenType::Tilde]) {
            let operand = self.parse_unary(vars)?;
            Ok(Expr::unary(&op, Self::unary_operator(op.token_type()), operand, false))
        }
        else if let Some(op) = self.match_token(&[TokenType::Minus, TokenType::Plus]) {
            let operand = self.parse_unary(vars)?;
            Ok(Expr::unary(&op, Self::unary_operator(op.token_type()), operand, true))
        }
        else {
            self.parse_primary(vars)
        }
    }

    fn unary_operator(token_type: &TokenType) -> UnaryOperatorType {
        match token_type {
            TokenType::Bang => UnaryOperatorType::LogicalNot,
            TokenType::Minus => UnaryOperatorType::Negate,
            TokenType::Plus => UnaryOperatorType::Identity,
            TokenType::Tilde => UnaryOperatorType::BitwiseNot,
            _ => panic!("{token_type:?} is not a unary operator"),
        }
    }

    fn parse_primary(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        match self.peek().cloned() {
            Some(token) => {
                match token.token_type() {
                    TokenType::Symbol(name) => {
                        self.advance();
                        self.resolve_symbol(name, &token, vars)
                    },
                    TokenType::Number(n) => {
                        self.advance();
                        Ok(Expr::number(&token, *n))
                    },
                    TokenType::LeftParen => self.parse_grouping(vars),
                    TokenType::LeftBBracket
                    | TokenType::LeftWBracket
                    | TokenType::LeftDBracket => self.parse_memory_operator(vars),
                    TokenType::Backtick => self.parse_flag_operator(),
                    _ => Err(Error::from(token.location.line, token.location.column,
                                         "misplaced or unrecognized token")),
                }
            }
            None => Err(Error::from(0, 0, "expected operand")),
        }
    }

    fn resolve_symbol(&self, name: &str, token: &Token<'a>, vars: &Variables) -> Result<Expr<'a>, Error> {
        match (self.parser.map_register)(name) {
            Some(operand) => Ok(Expr::register(token, operand)),
            None => match (self.parser.map_symbol)(name) {
                Some(operand) => Ok(Expr::number(token, operand)),
                None => match vars.get(name) {
                    Some(id) => Ok(Expr::variable(token, id)),
                    None => Err(Error::from(token.location.line, token.location.column,
                                            &format!("unresolved symbol '{}'", name)))
                }
            }
        }
    }

    fn parse_grouping(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let op = self.advance().unwrap();  // consume the opening parenthesis
        let operand = self.parse_next(vars)?;
        match self.advance() {
            Some(token) if token.token_type() == &TokenType::RightParen => {
                let signed = operand.is_signed();
                Ok(Expr::unary(&op, UnaryOperatorType::Grouping, operand, signed))
            }
            Some(token) => Err(Error::from(
                token.location.line, token.location.column,
                "expected closing parenthesis")),
            None => Err(Error::from(
                op.location.line, op.location.column,
                "expected closing parenthesis")),
        }
    }

    fn parse_memory_operator(&mut self, vars: &mut Variables) -> Result<Expr<'a>, Error> {
        let op = self.advance().unwrap();  // consume the bracket operator
        let operand = self.parse_next(vars)?;
        match self.advance() {
            Some(token) if token.token_type() == &TokenType::RightBracket => {
                let width = Self::memory_operand_width(op.token_type());
                Ok(Expr::unary(&op, UnaryOperatorType::Fetch(width), operand, false))
            }
            Some(token) => Err(Error::from(
                token.location.line, token.location.column,
                "expected closing bracket")),
            None => Err(Error::from(
                op.location.line, op.location.column,
                "expected closing bracket")),
        }
    }

    fn memory_operand_width(token_type: &TokenType) -> FetchWidth {
        match token_type {
            TokenType::LeftBBracket => FetchWidth::Byte,
            TokenType::LeftWBracket => FetchWidth::Word,
            TokenType::LeftDBracket => FetchWidth::DWord,
            _ => panic!("{token_type:?} is not a memory operator"),
        }
    }

    fn parse_flag_operator(&mut self) -> Result<Expr<'a>, Error> {
        let op = self.advance().unwrap(); // consume backtick
        match self.advance() {
            Some(token) => match token.token_type() {
                TokenType::Symbol(name) =>
                    match (self.parser.map_flag)(name) {
                        Some(flag) => Ok(Expr::flag(&token, flag)),
                        None => Err(Error::from(token.location.line, token.location.column,
                                                &format!("unrecognized flag '{}'", name)))
                    }
                _ => Err(Error::from(
                    token.location.line, token.location.column,
                    "expected flag name after '`'")),
            }
            None => Err(Error::from(
                op.location.line, op.location.column,
                "expected flag name after '`'")),
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
        }
        else {
            let token = self.tokens[self.current].clone();
            self.current += 1;
            Some(token)
        }
    }

    fn peek(&self) -> Option<&Token<'a>> {
        if self.is_at_end() {
            None
        }
        else {
            Some(&self.tokens[self.current])
        }
    }

    fn peek_next(&self) -> Option<&Token<'a>> {
        if self.current + 1 >= self.tokens.len() {
            None
        }
        else {
            Some(&self.tokens[self.current + 1])
        }
    }

    fn is_at_end(&self) -> bool {
        self.current == self.tokens.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::expr::ExprType;

    const REGISTERS: [(&'static str, Operand); 2] = [("A", 1), ("PC", 2)];
    const FLAGS: [(&'static str, Operand) ; 2] = [("C", 1), ("Z", 2)];
    const SYMBOLS: [(&'static str, Operand) ; 2] = [("foo", 42), ("bar", 69)];

    fn register_mapper(s: &str) -> Option<Operand> {
        let operand = REGISTERS.iter().find_map(|(name, value)| if s.eq_ignore_ascii_case(name) { Some(value) } else { None });
        match operand {
            Some(operand) => Some(*operand),
            None => None,
        }
    }

    fn flag_mapper(s: &str) -> Option<Operand> {
        let operand = FLAGS.iter().find_map(|(name, value)| if s.eq_ignore_ascii_case(name) { Some(value) } else { None });
        match operand {
            Some(operand) => Some(*operand),
            None => None,
        }
    }

    fn symbol_mapper(s: &str) -> Option<Operand> {
        let operand = SYMBOLS.iter().find_map(|(name, value)| if s.eq_ignore_ascii_case(name) { Some(value) } else { None });
        match operand {
            Some(operand) => Some(*operand),
            None => None,
        }
    }

    pub fn parser() -> Parser {
        Parser::from(register_mapper, flag_mapper, symbol_mapper)
    }

    pub fn no_vars() -> Variables {
        Variables::new()
    }

    #[test]
    fn parse_empty() {
        assert_eq!(parser().parse("", &mut no_vars()).unwrap(), None);
    }

    #[test]
    fn parse_non_empty() {
        parser().parse("a", &mut no_vars()).unwrap().unwrap();
    }

    #[test]
    fn parse_symbol() {
        let symbol_text = "foo";
        let expr = parser().parse(symbol_text, &mut no_vars()).unwrap().unwrap();
        assert_eq!(expr.token().token_type(), &TokenType::Symbol(String::from(symbol_text)));
        assert_eq!(expr.expr_type(), &ExprType::Number(42));
        assert!(!expr.is_signed());
    }

    #[test]
    fn parse_number() {
        let number = 42;
        let number_text = number.to_string();
        let expr = parser().parse(&number_text, &mut no_vars()).unwrap().unwrap();
        assert_eq!(expr.token().token_type(), &TokenType::Number(number));
        assert_eq!(expr.expr_type(), &ExprType::Number(number));
        assert!(!expr.is_signed());
    }

    #[test]
    fn parse_grouping() {
        let source = "(42)";
        let result = parser().parse(source, &mut no_vars()).unwrap();
        let expr = result.unwrap();
        assert_eq!(expr.token().token_type(), &TokenType::LeftParen);
        match expr.expr_type() {
            ExprType::UnaryOperator(UnaryOperatorType::Grouping, operand) => {
                assert_eq!(operand.token().token_type(), &TokenType::Number(42));
                assert_eq!(operand.expr_type(), &ExprType::Number(42));
            }
            _ => panic!("expected UnaryOperator, got {:?}", expr),
        }
        let result = parser().parse("()", &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("misplaced"));
        let result = parser().parse("(", &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("operand"));
        let result = parser().parse("(42 foo", &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("closing"));
        let result = parser().parse("(42", &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("closing"));
    }

    #[test]
    fn parse_group_signedness() {

    }

    fn validate_memory_operator(operator_width: &str, expected_operand_width: &FetchWidth) {
        let source = format!("{operator_width}[0]");
        let result = parser().parse(&source, &mut no_vars()).unwrap();
        let expr = result.unwrap();
        match expr.expr_type() {
            ExprType::UnaryOperator(operator_type, operand) => {
                match operator_type {
                    UnaryOperatorType::Fetch(w) => {
                        assert_eq!(w, expected_operand_width);
                    }
                    _ => panic!("expected operator type: {operator_type:?}")
                }
                assert_eq!(operand.expr_type(), &ExprType::Number(0));
            }
            _ => panic!("expected UnaryOperator, got {:?}", expr),
        }
        assert!(!expr.is_signed());

        let source = format!("{operator_width}[]");
        let result = parser().parse(&source, &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("misplaced"));
        let source = format!("{operator_width}[");
        let result = parser().parse(&source, &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("operand"));
        let source = format!("{operator_width}[0 foo");
        let result = parser().parse(&source, &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("closing"));
        let source = format!("{operator_width}[0");
        let result = parser().parse(&source, &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("closing"));
    }

    #[test]
    fn parse_memory_operator() {
        validate_memory_operator("B", &FetchWidth::Byte);
        validate_memory_operator("W", &FetchWidth::Word);
        validate_memory_operator("D", &FetchWidth::DWord);
        validate_memory_operator("b", &FetchWidth::Byte);
        validate_memory_operator("w", &FetchWidth::Word);
        validate_memory_operator("d", &FetchWidth::DWord);
    }

    #[test]
    fn parse_flag_operator() {
        let result = parser().parse("`C", &mut no_vars());
        let expr = result.unwrap().unwrap();
        match expr.expr_type() {
            ExprType::Flag(operand) => {
                assert_eq!(*operand, 1);     // C = 1 in our mapper
            }
            _ => panic!("expected Flag, got {:?}", expr),
        }
        assert!(!expr.is_signed());

        let result = parser().parse("`9", &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("expected"));
        let result = parser().parse("`", &mut no_vars());
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("expected"));
    }

    fn validate_unary(operator_text: &str, expected_operator_type: &UnaryOperatorType) {
        let source = format!("{operator_text}foo");
        let result = parser().parse(&source, &mut no_vars()).unwrap();
        let expr = result.unwrap();
        match expr.expr_type() {
            ExprType::UnaryOperator(operator_type, operand) => {
                assert_eq!(operator_type, expected_operator_type);
                assert_eq!(operand.expr_type(), &ExprType::Number(42)); // foo == 42 in the symbol map
            }
            _ => panic!("expected UnaryOperator, got {:?}", expr),
        }
    }

    #[test]
    fn parse_unary_operator() {
        validate_unary("!", &UnaryOperatorType::LogicalNot);
        validate_unary("-", &UnaryOperatorType::Negate);
        validate_unary("+", &UnaryOperatorType::Identity);
        validate_unary("~", &UnaryOperatorType::BitwiseNot);
    }

    fn validate_binary_operator(operator_text: &str, expected_operator_type: &BinaryOperatorType) {
        let source = format!("foo {operator_text} 42");
        let result = parser().parse(&source, &mut no_vars()).unwrap();
        let expr = result.unwrap();
        match expr.expr_type() {
            ExprType::BinaryOperator(operator_type, left, right) => {
                assert_eq!(operator_type, expected_operator_type);
                assert_eq!(left.expr_type(), &ExprType::Number(42)); // foo == 42 in the symbol map
                assert_eq!(right.expr_type(), &ExprType::Number(42));
            }
            _ => panic!("expected BinaryOperator, got {:?}", expr),
        }
    }

    #[test]
    fn parse_binary_operator() {
        validate_binary_operator("&", &BinaryOperatorType::BitwiseAnd);
        validate_binary_operator("&&", &BinaryOperatorType::LogicalAnd);
        validate_binary_operator("!=", &BinaryOperatorType::NotEqual);
        validate_binary_operator("|", &BinaryOperatorType::BitwiseOr);
        validate_binary_operator("||", &BinaryOperatorType::LogicalOr);
        validate_binary_operator("^", &BinaryOperatorType::BitwiseXor);
        validate_binary_operator("=", &BinaryOperatorType::Equal);
        validate_binary_operator("==", &BinaryOperatorType::Equal);
        validate_binary_operator(">", &BinaryOperatorType::GreaterThan);
        validate_binary_operator(">=", &BinaryOperatorType::GreaterOrEqual);
        validate_binary_operator(">>", &BinaryOperatorType::RightShift);
        validate_binary_operator("<", &BinaryOperatorType::LessThan);
        validate_binary_operator("<=", &BinaryOperatorType::LessOrEqual);
        validate_binary_operator("<<", &BinaryOperatorType::LeftShift);
        validate_binary_operator("-", &BinaryOperatorType::Subtract);
        validate_binary_operator("%", &BinaryOperatorType::Remainder);
        validate_binary_operator("+", &BinaryOperatorType::Add);
        validate_binary_operator("/", &BinaryOperatorType::Divide);
        validate_binary_operator("*", &BinaryOperatorType::Multiply);
    }

    #[test]
    fn parse_compound() {
        let result = parser().parse("PC == 0x200 && `C || A == 5", &mut no_vars());
        assert!(result.is_ok());
    }

    #[test]
    fn parse_compound_symbols() {
        let result = parser().parse("A == foo || A == bar", &mut no_vars());
        result.unwrap();
    }

    fn validate_signedness(source_text: &str, signed: bool) {
        let result = parser().parse(source_text, &mut no_vars()).unwrap();
        let expr = result.unwrap();
        assert_eq!(expr.is_signed(), signed);
    }

    fn assert_signed(source_text: &str) {
        validate_signedness(source_text, true);
    }

    fn assert_unsigned(source_text: &str) {
        validate_signedness(source_text, false);
    }

    #[test]
    fn parse_signedness() {
        assert_unsigned("1");
        assert_signed("+1");
        assert_signed("-1");
        assert_unsigned("~1");
        assert_unsigned("!1");
        assert_unsigned("(1)");
        assert_signed("(+1)");
        assert_signed("+(1)");
        assert_unsigned("b[-1]");
        assert_signed("+b[-1]");
        assert_unsigned("`c");
        assert_unsigned("1 * 2");
        assert_unsigned("1 / 2");
        assert_unsigned("1 % 2");
        assert_signed("-1 * 2");
        assert_signed("1 + -2");
        assert_unsigned("1 + 2");
        assert_signed("-1 + 1");
        assert_signed("1 + -1");
        assert_unsigned("1 << 2");
        assert_unsigned("1 >> 2");
        assert_signed("-1 >> 2");
        assert_signed("1 << -2");
        assert_unsigned("1 > 2");
        assert_signed("-1 < 2");
        assert_signed("1 < -2");
        assert_unsigned("1 == 2");
        assert_signed("-1 == 2");
        assert_signed("1 != -2");
        assert_unsigned("1 & 2");
        assert_signed("1 ^ -2");
        assert_signed("-1 | 2");
        assert_signed("-1 && 0");
        assert_signed("1 || +0");
        assert_signed("a * (-foo * pc)");
        assert_unsigned("a * (foo * pc)");
    }

    #[test]
    fn parse_walrus_creates_assign_expr() {
        let mut vars = Variables::new();
        let expr = parser().parse("x := 42", &mut vars).unwrap().unwrap();
        let id = vars.get("x").unwrap();
        assert_eq!(expr.expr_type(), &ExprType::Assign(id, Box::new(Expr::number(expr.token(), 42))));
    }

    #[test]
    fn parse_walrus_rhs_can_reference_register() {
        let mut vars = Variables::new();
        let expr = parser().parse("x := A", &mut vars).unwrap().unwrap();
        match expr.expr_type() {
            ExprType::Assign(_, rhs) => assert_eq!(rhs.expr_type(), &ExprType::Register(1)),
            _ => panic!("expected Assign, got {:?}", expr),
        }
    }

    #[test]
    fn parse_variable_read_after_assign() {
        let mut vars = Variables::new();
        parser().parse("x := 42", &mut vars).unwrap();
        let expr = parser().parse("x", &mut vars).unwrap().unwrap();
        let id = vars.get("x").unwrap();
        assert_eq!(expr.expr_type(), &ExprType::Variable(id));
    }

    #[test]
    fn parse_walrus_allocates_stable_id() {
        let mut vars = Variables::new();
        parser().parse("x := 1", &mut vars).unwrap();
        let id1 = vars.get("x").unwrap();
        parser().parse("x := 2", &mut vars).unwrap();
        let id2 = vars.get("x").unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn parse_nested_walrus() {
        let mut vars = Variables::new();
        let _prev_a_id = vars.get_or_create("prev_A");
        parser().parse("(prev_A := A) != prev_A", &mut vars).unwrap();
    }

}
