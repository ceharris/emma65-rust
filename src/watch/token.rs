use std::mem;
use super::location::Location;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenType {
    Amper,
    AmperAmper,
    Backtick,
    Bang,
    BangEqual,
    Bar,
    BarBar,
    Caret,
    Equal,
    EqualEqual,
    Greater,
    GreaterEqual,
    GreaterGreater,
    LeftBBracket,
    LeftWBracket,
    LeftDBracket,
    Lesser,
    LesserEqual,
    LesserLesser,
    LeftParen,
    Minus,
    Number(u32),
    Percent,
    Plus,
    RightBracket,
    RightParen,
    Semicolon,
    Slash,
    Star,
    String(String),
    Symbol(String),
    Tilde,
    Walrus,
}

impl PartialEq<TokenType> for &TokenType {
    fn eq(&self, other: &TokenType) -> bool {
        match (self, other) {
            (TokenType::Number(n), TokenType::Number(other_n)) => n == other_n,
            (TokenType::Symbol(s), TokenType::Symbol(other_s)) => s == other_s,
            (TokenType::String(s), TokenType::String(other_s)) => s == other_s,
            _ => mem::discriminant(*self) == mem::discriminant(other)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token<'a> {
    token_type: TokenType,
    text: &'a str,
    pub location: Location,
}


impl<'a> Token<'a> {

    pub fn from(token_type: TokenType, text: &'a str, line: usize, column: usize) -> Self {
        Token {
            token_type, text, location: Location::from(line, column)
        }
    }

    pub fn token_type(&self) -> &TokenType {
        &self.token_type
    }

    pub fn text(&self) -> &'a str {
        self.text
    }


}

#[cfg(test)]

mod tests {
    use super::*;

    #[test]
    fn token_type_eq() {
        assert_eq!(TokenType::Amper, TokenType::Amper);
        assert_ne!(TokenType::Amper, TokenType::Bang);
        assert_eq!(TokenType::Number(42), TokenType::Number(42));
        assert_ne!(TokenType::Number(42), TokenType::Number(24));
        assert_eq!(TokenType::Symbol(String::from("foo")), TokenType::Symbol(String::from("foo")));
        assert_ne!(TokenType::Symbol(String::from("foo")), TokenType::Symbol(String::from("bar")));
        assert_eq!(TokenType::String(String::from("hello")), TokenType::String(String::from("hello")));
        assert_ne!(TokenType::String(String::from("hello")), TokenType::String(String::from("world")));
    }

}
