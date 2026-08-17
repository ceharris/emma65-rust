use super::error::Error;
use super::token::{Token, TokenType};
use crate::text::Text;

const TAB_SIZE: usize = 8;

pub struct Scanner<'a> {
    source: Text<'a>,
    line: usize,
    column: usize,
}

impl <'a> Scanner<'a> {

    pub fn new(source: &'a str) -> Self {
        Self {
            source: Text::from(source),
            line: 1,
            column: 1,
        }
    }

    pub fn scan(&mut self) -> Result<Vec<Token<'a>>, Error> {
        let mut tokens: Vec<Token<'a>> = Vec::new();
        loop {
            {
                let result = self.next_token()?;
                match result {
                    None => return Ok(tokens),
                    Some(token) => tokens.push(token),
                }
            }
        }
    }

    fn next_token(&mut self) -> Result<Option<Token<'a>>, Error> {
        self.consume_whitespace();
        match self.source.advance() {
            Some(c) => self.match_token(c),
            None => Ok(None),
        }
    }

    /// Skips spaces, tabs, carriage returns, and `;`-to-end-of-line comments.
    /// Newlines are significant statement terminators and are not consumed here.
    fn consume_whitespace(&mut self) {
        loop {
            match self.source.peek() {
                Some(b'\t') => {
                    self.source.skip();
                    self.column += TAB_SIZE;
                }
                Some(b' ') | Some(b'\r') => {
                    self.source.skip();
                    self.column += 1;
                }
                Some(b';') => {
                    loop {
                        match self.source.peek() {
                            Some(b'\n') | None => break,
                            Some(_) => {
                                self.source.skip();
                                self.column += 1;
                            }
                        }
                    }
                }
                Some(_) | None => break,
            }
        }
    }

    fn match_token(&mut self, c: u8) -> Result<Option<Token<'a>>, Error> {
        match c {
            b'\n' => {
                let token = self.make_token(TokenType::Newline);
                self.line += 1;
                self.column = 1;
                Ok(Some(token))
            }
            b'^' => Ok(Some(self.make_token(TokenType::Caret))),
            b'~' => Ok(Some(self.make_token(TokenType::Tilde))),
            b'+' => Ok(Some(self.make_token(TokenType::Plus))),
            b'-' => Ok(Some(self.make_token(TokenType::Minus))),
            b'*' => Ok(Some(self.make_token(TokenType::Star))),
            b'/' => Ok(Some(self.make_token(TokenType::Slash))),
            b'%' => Ok(Some(self.make_token(TokenType::Percent))),
            b'(' => Ok(Some(self.make_token(TokenType::LeftParen))),
            b')' => Ok(Some(self.make_token(TokenType::RightParen))),
            b',' => Ok(Some(self.make_token(TokenType::Comma))),
            b':' => Ok(Some(self.make_token(TokenType::Colon))),
            b'#' => Ok(Some(self.make_token(TokenType::Hash))),
            b'=' => Ok(Some(self.make_token(TokenType::Equal))),
            b'&' => match self.source.peek() {
                Some(b'&') => Ok(Some(self.advance_and_make_token(TokenType::AmperAmper))),
                Some(_) | None => Ok(Some(self.make_token(TokenType::Amper))),
            }
            b'!' => Ok(Some(self.make_token(TokenType::Bang))),
            b'|' => match self.source.peek() {
                Some(b'|') => Ok(Some(self.advance_and_make_token(TokenType::BarBar))),
                Some(_) | None => Ok(Some(self.make_token(TokenType::Bar))),
            }
            b'>' => match self.source.peek() {
                Some(b'>') => Ok(Some(self.advance_and_make_token(TokenType::GreaterGreater))),
                Some(_) | None => Ok(Some(self.make_token(TokenType::Greater))),
            },
            b'<' => match self.source.peek() {
                Some(b'<') => Ok(Some(self.advance_and_make_token(TokenType::LesserLesser))),
                Some(_) | None => Ok(Some(self.make_token(TokenType::Lesser))),
            }
            b'.' => Self::optional(self.make_directive()),
            b'$' => Self::optional(self.make_hexadecimal_number()),
            b'"' => Self::optional(self.make_string_literal()),
            b'\'' => Self::optional(self.make_char_literal()),
            b'0' => Self::optional(self.make_prefixed_number()),
            b'1'..=b'9' => Self::optional(self.make_decimal_number(c)),
            b'_' | b'A'..=b'Z' | b'a'..=b'z' => Ok(Some(self.make_symbol(c))),
            _ => Err(Error::from(self.line, self.column, "unrecognized character")),
        }
    }

    fn optional<T, E>(result: Result<T, E>) -> Result<Option<T>, E> {
        match result {
            Ok(t) => Ok(Some(t)),
            Err(e) => Err(e),
        }
    }

    fn advance_and_make_token(&mut self, token_type: TokenType) -> Token<'a> {
        self.source.advance();
        self.make_token(token_type)
    }

    fn make_symbol(&mut self, c: u8) -> Token<'a> {
        let mut name = String::new();
        name.push(c as char);
        loop {
            let c = self.source.peek();
            match c {
                Some(b'_') | Some(b'0'..=b'9') | Some(b'A'..=b'Z') | Some(b'a'..=b'z') => {
                    self.source.advance();
                    name.push(c.unwrap() as char)
                }
                Some(_) | None => break
            }
        }
        self.make_token(TokenType::Symbol(name))
    }

    fn make_directive(&mut self) -> Result<Token<'a>, Error> {
        match self.source.peek() {
            Some(b'_') | Some(b'A'..=b'Z') | Some(b'a'..=b'z') => {}
            Some(_) | None => return Err(Error::from(self.line, self.column, "expected directive name")),
        }
        let mut name = String::new();
        loop {
            let c = self.source.peek();
            match c {
                Some(b'_') | Some(b'0'..=b'9') | Some(b'A'..=b'Z') | Some(b'a'..=b'z') => {
                    self.source.advance();
                    name.push(c.unwrap() as char)
                }
                Some(_) | None => break
            }
        }
        Ok(self.make_token(TokenType::Directive(name)))
    }

    fn make_prefixed_number(&mut self) -> Result<Token<'a>, Error> {
        match self.source.peek() {
            Some(b'b') | Some(b'B') => {
                self.source.advance();
                self.make_binary_number()
            },
            Some(b'o') | Some(b'O') | Some(b'q') | Some(b'Q') => {
                self.source.advance();
                self.make_octal_number()
            },
            Some(b'x') | Some(b'X') => {
                self.source.advance();
                self.make_hexadecimal_number()
            },
            Some(b'1'..=b'9') => {
                self.make_octal_number()
            },
            Some(b'A'..=b'Z') | Some(b'a'..=b'z') => {
                Err(Error::from(self.line, self.column, "invalid number"))
            }
            Some(_) | None => {
                Ok(self.make_token(TokenType::Number(0)))
            }
        }
    }

    fn make_binary_number(&mut self) -> Result<Token<'a>, Error> {
        let mut value: u32 = 0;
        let mut count = 0;
        loop {
            match self.source.peek() {
                Some(d) if (b'0'..=b'1').contains(&d) => {
                    self.source.advance();
                    let digit: u32 = (d - b'0').into();
                    value = (value << 1) | digit;
                    count += 1;
                }
                Some(b'2'..=b'9') | Some(b'A'..=b'Z') | Some(b'a'..=b'z') =>
                    return Err(Error::from(self.line, self.column, "invalid binary digit")),
                Some(_) | None => break,
            }
        }
        if count > 0 {
            Ok(self.make_token(TokenType::Number(value)))
        }
        else {
            Err(Error::from(self.line, self.column, "expected binary digit"))
        }
    }

    fn make_octal_number(&mut self) -> Result<Token<'a>, Error> {
        let mut value: u32 = 0;
        let mut count = 0;
        loop {
            match self.source.peek() {
                Some(d) if (b'0'..=b'7').contains(&d) => {
                    self.source.advance();
                    let digit: u32 = (d - b'0').into();
                    value = (value << 3) | digit;
                    count += 1;
                }
                Some(b'8'..=b'9') | Some(b'A'..=b'Z') | Some(b'a'..=b'z') =>
                    return Err(Error::from(self.line, self.column, "invalid octal digit")),
                Some(_) | None => break,
            }
        }
        if count > 0 {
            Ok(self.make_token(TokenType::Number(value)))
        }
        else {
            Err(Error::from(self.line, self.column, "expected octal digit"))
        }
    }

    fn make_decimal_number(&mut self, d: u8) -> Result<Token<'a>, Error> {
        let mut value: u32 = (d - b'0').into();
        loop {
            match self.source.peek() {
                Some(d) if d.is_ascii_digit() => {
                    self.source.advance();
                    let digit: u32 = (d - b'0').into();
                    value = 10*value + digit;
                }
                Some(b'A'..=b'Z') | Some(b'a'..=b'z') =>
                    return Err(Error::from(self.line, self.column, "invalid decimal digit")),
                Some(_) | None => break,
            }
        }
        Ok(self.make_token(TokenType::Number(value)))
    }

    fn hexadecimal_digit(c: u8) -> u32 {
        // map 'a'..='f' to 'A'..='F'
        let d = if c >= b'A' { c & 0xdf } else { c };
        // map ASCII to 0..=15
        let mut digit = (d - b'0').into();
        if d > b'9' {
            digit -= 7;  // adjust for gap between '9' and 'A'
        }
        digit
    }

    fn make_hexadecimal_number(&mut self) -> Result<Token<'a>, Error> {
        let mut value: u32 = 0;
        let mut count = 0;
        loop {
            let c = self.source.peek();
            match c {
                Some(b'0'..=b'9') | Some(b'A'..=b'F') | Some(b'a'..=b'f') => {
                    self.source.advance();
                    value = (value << 4) | Self::hexadecimal_digit(c.unwrap());
                    count += 1;
                }
                Some(b'G'..=b'Z') | Some(b'g'..=b'z') =>
                    return Err(Error::from(self.line, self.column, "invalid hexadecimal digit")),
                Some(_) | None => break,
            }
        }
        if count > 0 {
            Ok(self.make_token(TokenType::Number(value)))
        }
        else {
            Err(Error::from(self.line, self.column, "expected hexadecimal digit"))
        }

    }

    fn make_token(&mut self, token_type: TokenType) -> Token<'a> {
        let token = Token::from(token_type, self.source.consume(), self.line, self.column);
        self.column += token.text().len();
        token
    }

    fn make_char_literal(&mut self) -> Result<Token<'a>, Error> {
        let ch = self.scan_char_literal();
        match self.source.advance() {
            Some(b'\'') => Ok(self.make_token(TokenType::Number(ch?.into()))),
            Some(_) | None => Err(Error::from(self.line, self.column, "expected closing single quote")),
        }
    }

    fn scan_char_literal(&mut self) -> Result<u8, Error> {
        let c = self.source.advance();
        match c {
            Some(b'\'') => Err(Error::from(self.line, self.column, "empty character literal")),
            Some(b'\\') => self.unescape(),
            Some(_) => Ok(c.unwrap()),
            None => Err(Error::from(self.line, self.column, "expected character literal")),
        }
    }

    fn make_string_literal(&mut self) -> Result<Token<'a>, Error> {
        let mut s = String::new();
        loop {
            let c = self.source.advance();
            match c {
                Some(b'\\') => {
                    {
                        let ch = self.unescape()?;
                        s.push(ch as char)
                    }
                }
                Some(b'"') => break,
                Some(_) => s.push(c.unwrap() as char),
                None => return Err(Error::from(self.line, self.column, "expected closing quote")),
            }
        }
        Ok(self.make_token(TokenType::String(s)))
    }

    fn unescape(&mut self) -> Result<u8, Error> {
        match self.source.advance() {
            Some(b'\\') => Ok(b'\\'),
            Some(b'n') => Ok(b'\n'),
            Some(b'r') => Ok(b'\r'),
            Some(b't') => Ok(b'\t'),
            Some(b'\'') => Ok(b'\''),
            Some(b'"') => Ok(b'"'),
            Some(_) | None => Err(Error::from(self.line, self.column, "unrecognized escape sequence")),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_next_token_valid(token_text: &str, token_type: &TokenType) {
        let mut scanner = Scanner::new(token_text);
        let token = scanner.next_token().unwrap();
        assert!(token.is_some());
        let token = token.unwrap();
        assert_eq!(token.token_type(), token_type);
        assert_eq!(token.text(), token_text);
    }

    fn assert_next_token_invalid(token_text: &str, error_predicate: fn(&Error) -> bool) {
        let mut scanner = Scanner::new(token_text);
        let result = scanner.next_token();
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error_predicate(&error));
    }

    #[test]
    fn next_token_with_valid_operators() {
        assert_next_token_valid("&", &TokenType::Amper);
        assert_next_token_valid("&&", &TokenType::AmperAmper);
        assert_next_token_valid("!", &TokenType::Bang);
        assert_next_token_valid("|", &TokenType::Bar);
        assert_next_token_valid("||", &TokenType::BarBar);
        assert_next_token_valid("^", &TokenType::Caret);
        assert_next_token_valid(":", &TokenType::Colon);
        assert_next_token_valid(",", &TokenType::Comma);
        assert_next_token_valid("=", &TokenType::Equal);
        assert_next_token_valid(">", &TokenType::Greater);
        assert_next_token_valid(">>", &TokenType::GreaterGreater);
        assert_next_token_valid("#", &TokenType::Hash);
        assert_next_token_valid("<", &TokenType::Lesser);
        assert_next_token_valid("<<", &TokenType::LesserLesser);
        assert_next_token_valid("-", &TokenType::Minus);
        assert_next_token_valid("%", &TokenType::Percent);
        assert_next_token_valid("+", &TokenType::Plus);
        assert_next_token_valid("/", &TokenType::Slash);
        assert_next_token_valid("*", &TokenType::Star);
        assert_next_token_valid("~", &TokenType::Tilde);
        assert_next_token_valid("(", &TokenType::LeftParen);
        assert_next_token_valid(")", &TokenType::RightParen);
        assert_next_token_valid("\n", &TokenType::Newline);
        assert_next_token_valid(".org", &TokenType::Directive(String::from("org")));
        assert_next_token_valid(".byte", &TokenType::Directive(String::from("byte")));
        assert_next_token_valid(".word", &TokenType::Directive(String::from("word")));
        assert_next_token_valid(".res", &TokenType::Directive(String::from("res")));
        assert_next_token_valid(".equ", &TokenType::Directive(String::from("equ")));
        assert_next_token_valid("\"foobar\"", &TokenType::String(String::from("foobar")));
        assert_next_token_valid("\"\\n\\r\\t\\\\\"", &TokenType::String(String::from("\n\r\t\\")));
        assert_next_token_valid("\"\\\"\"", &TokenType::String(String::from("\"")));
        assert_next_token_valid("\"\\'\"", &TokenType::String(String::from("'")));
        assert_next_token_valid("'a'", &TokenType::Number(b'a'.into()));
        assert_next_token_valid("'\n'", &TokenType::Number('\n'.into()));
        assert_next_token_valid("'\\''", &TokenType::Number('\''.into()));
    }

    fn assert_first_token_type(source: &str, expected_type: &TokenType) {
        let mut scanner = Scanner::new(source);
        let token = scanner.next_token().unwrap().unwrap();
        assert_eq!(token.token_type(), expected_type);
    }

    #[test]
    fn next_token_lesser_vs_lesser_lesser_disambiguation() {
        assert_next_token_valid("<", &TokenType::Lesser);
        assert_next_token_valid("<<", &TokenType::LesserLesser);
        assert_first_token_type("< ", &TokenType::Lesser);
        assert_first_token_type("<label", &TokenType::Lesser);
        assert_first_token_type("<< 4", &TokenType::LesserLesser);
    }

    #[test]
    fn next_token_greater_vs_greater_greater_disambiguation() {
        assert_next_token_valid(">", &TokenType::Greater);
        assert_next_token_valid(">>", &TokenType::GreaterGreater);
        assert_first_token_type("> ", &TokenType::Greater);
        assert_first_token_type(">label", &TokenType::Greater);
        assert_first_token_type(">> 4", &TokenType::GreaterGreater);
    }

    #[test]
    fn next_token_binary_literal() {
        assert_next_token_valid("0b0", &TokenType::Number(0));
        assert_next_token_valid("0b1", &TokenType::Number(1));
        assert_next_token_valid("0b11", &TokenType::Number(0b11));
        assert_next_token_valid("0B11", &TokenType::Number(0b11));
        assert_next_token_invalid("0b",
            |e|e.to_string().contains("expected binary"));
        assert_next_token_invalid("0b2",
            |e| { e.to_string().contains("binary digit")});
        assert_next_token_invalid("0b9",
            |e| { e.to_string().contains("binary digit")});
        assert_next_token_invalid("0bA",
            |e| { e.to_string().contains("binary digit")});
        assert_next_token_invalid("0bZ",
            |e| { e.to_string().contains("binary digit")});
        assert_next_token_invalid("0ba",
            |e| { e.to_string().contains("binary digit")});
        assert_next_token_invalid("0bz",
            |e| { e.to_string().contains("binary digit")});
    }

    #[test]
    fn next_token_octal_literal() {
        assert_next_token_valid("0o0", &TokenType::Number(0));
        assert_next_token_valid("0o1", &TokenType::Number(1));
        assert_next_token_valid("0o7", &TokenType::Number(7));
        assert_next_token_valid("0o17", &TokenType::Number(0o17));
        assert_next_token_valid("0O17", &TokenType::Number(0o17));
        assert_next_token_valid("0q17", &TokenType::Number(0o17));
        assert_next_token_valid("0Q17", &TokenType::Number(0o17));
        assert_next_token_valid("017", &TokenType::Number(0o17));
        assert_next_token_valid("0", &TokenType::Number(0));
        assert_next_token_invalid("0o",
            |e| e.to_string().contains("expected octal"));
        assert_next_token_invalid("0o8",
            |e| e.to_string().contains("octal digit"));
        assert_next_token_invalid("0o9",
            |e| e.to_string().contains("octal digit"));
        assert_next_token_invalid("0oA",
            |e| e.to_string().contains("octal digit"));
        assert_next_token_invalid("0oZ",
            |e| e.to_string().contains("octal digit"));
        assert_next_token_invalid("0oa",
            |e| e.to_string().contains("octal digit"));
        assert_next_token_invalid("0oz",
            |e| e.to_string().contains("octal digit"));
    }

    #[test]
    pub fn next_token_decimal_literal() {
        assert_next_token_valid("0", &TokenType::Number(0));
        assert_next_token_valid("1", &TokenType::Number(1));
        assert_next_token_valid("9", &TokenType::Number(9));
        assert_next_token_valid("19", &TokenType::Number(19));
        assert_next_token_invalid("1A",
            |e|e.to_string().contains("decimal digit"));
        assert_next_token_invalid("1Z",
            |e|e.to_string().contains("decimal digit"));
        assert_next_token_invalid("1a",
            |e|e.to_string().contains("decimal digit"));
        assert_next_token_invalid("1z",
            |e|e.to_string().contains("decimal digit"));
    }

    #[test]
    pub fn next_token_hexadecimal_literal() {
        assert_next_token_valid("0x0", &TokenType::Number(0));
        assert_next_token_valid("0x9", &TokenType::Number(9));
        assert_next_token_valid("0xA", &TokenType::Number(0xa));
        assert_next_token_valid("0xF", &TokenType::Number(0xf));
        assert_next_token_valid("0xa", &TokenType::Number(0xa));
        assert_next_token_valid("0xf", &TokenType::Number(0xf));
        assert_next_token_valid("0x1f", &TokenType::Number(0x1f));
        assert_next_token_valid("0X1f", &TokenType::Number(0x1f));
        assert_next_token_valid("$1f", &TokenType::Number(0x1f));
        assert_next_token_invalid("$",
            |e| { e.to_string().contains("expected hexadecimal")});
        assert_next_token_invalid("0x",
            |e| { e.to_string().contains("expected hexadecimal")});
        assert_next_token_invalid("0xG",
            |e| { e.to_string().contains("hexadecimal digit")});
        assert_next_token_invalid("0xZ",
            |e| { e.to_string().contains("hexadecimal digit")});
        assert_next_token_invalid("0xg",
            |e| { e.to_string().contains("hexadecimal digit")});
        assert_next_token_invalid("0xz",
            |e| { e.to_string().contains("hexadecimal digit")});
    }

    #[test]
    fn next_token_symbol() {
        assert_next_token_valid("_", &TokenType::Symbol(String::from("_")));
        assert_next_token_valid("_0", &TokenType::Symbol(String::from("_0")));
        assert_next_token_valid("_9", &TokenType::Symbol(String::from("_9")));
        assert_next_token_valid("_A", &TokenType::Symbol(String::from("_A")));
        assert_next_token_valid("_Z", &TokenType::Symbol(String::from("_Z")));
        assert_next_token_valid("_a", &TokenType::Symbol(String::from("_a")));
        assert_next_token_valid("_z", &TokenType::Symbol(String::from("_z")));
        assert_next_token_valid("A", &TokenType::Symbol(String::from("A")));
        assert_next_token_valid("Z", &TokenType::Symbol(String::from("Z")));
        assert_next_token_valid("a", &TokenType::Symbol(String::from("a")));
        assert_next_token_valid("z", &TokenType::Symbol(String::from("z")));
        assert_next_token_valid("A0", &TokenType::Symbol(String::from("A0")));
        assert_next_token_valid("Az", &TokenType::Symbol(String::from("Az")));
        assert_next_token_valid("my_routine", &TokenType::Symbol(String::from("my_routine")));
        assert_next_token_valid("LDA", &TokenType::Symbol(String::from("LDA")));
    }

    #[test]
    fn next_token_directive() {
        assert_next_token_valid(".org", &TokenType::Directive(String::from("org")));
        assert_next_token_valid(".ORG", &TokenType::Directive(String::from("ORG")));
        assert_next_token_valid(".equ", &TokenType::Directive(String::from("equ")));
        assert_next_token_invalid(".",
            |e| e.to_string().contains("expected directive name"));
        assert_next_token_invalid(".1",
            |e| e.to_string().contains("expected directive name"));
    }

    #[test]
    fn next_token_string_literal() {
        assert_next_token_valid("\"\"", &TokenType::String(String::from("")));
        assert_next_token_valid("\"a string\"", &TokenType::String(String::from("a string")));
        assert_next_token_valid("\"\\t\\n\\r\"", &TokenType::String(String::from("\t\n\r")));
        assert_next_token_valid("\"\\\"\"", &TokenType::String(String::from("\"")));
        assert_next_token_valid("\"\\\\\"", &TokenType::String(String::from("\\")));
        assert_next_token_invalid("\"",
            |e| e.to_string().contains("closing"));
        assert_next_token_invalid("\"\\@\"",
            |e| e.to_string().contains("escape"));
    }

    #[test]
    fn next_token_char_literal() {
        assert_next_token_valid("'a'", &TokenType::Number(b'a'.into()));
        assert_next_token_valid("'\\t'", &TokenType::Number(b'\t'.into()));
        assert_next_token_valid("'\\n'", &TokenType::Number(b'\n'.into()));
        assert_next_token_valid("'\\r'", &TokenType::Number(b'\r'.into()));
        assert_next_token_valid("'\\''", &TokenType::Number(b'\''.into()));
        assert_next_token_valid("'\\\"'", &TokenType::Number(b'"'.into()));
        assert_next_token_invalid("''",
            |e| e.to_string().contains("expected"));
        assert_next_token_invalid("'",
            |e| e.to_string().contains("closing"));
    }

    #[test]
    fn next_token_unrecognized() {
        assert_next_token_invalid("@",
            |e| e.to_string().contains("unrecognized"));
    }

    fn assert_whitespace_consumed(token_text: &str, expected_line: usize, expected_column: usize) {
        let mut scanner = Scanner::new(token_text);
        let result = scanner.next_token();
        assert!(result.is_ok());
        let token = result.unwrap();
        assert!(token.is_none());
        assert_eq!(scanner.line, expected_line);
        assert_eq!(scanner.column, expected_column);
    }

    #[test]
    fn next_token_whitespace() {
        assert_whitespace_consumed(" ", 1, 2);
        assert_whitespace_consumed("\t", 1, 1 + TAB_SIZE);
        assert_whitespace_consumed("\r", 1, 2);
    }

    #[test]
    fn scan_comment_is_stripped_and_newline_still_tokenized() {
        let mut scanner = Scanner::new("; a comment\n");
        let tokens = scanner.scan().unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].token_type(), &TokenType::Newline);
    }

    #[test]
    fn scan_comment_at_end_of_source_with_no_trailing_newline() {
        let mut scanner = Scanner::new("LDA #1 ; load one");
        let tokens = scanner.scan().unwrap();
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].token_type(), &TokenType::Symbol(String::from("LDA")));
        assert_eq!(tokens[1].token_type(), &TokenType::Hash);
        assert_eq!(tokens[2].token_type(), &TokenType::Number(1));
    }

    #[test]
    fn scan_trims_whitespace() {
        let token_text = "  LDA  #$20 , X";
        let mut scanner = Scanner::new(token_text);
        let tokens = scanner.scan().unwrap();
        assert_eq!(tokens.len(), 5);
        assert_eq!(tokens[0].text(), "LDA");
        assert_eq!(tokens[1].text(), "#");
        assert_eq!(tokens[2].text(), "$20");
        assert_eq!(tokens[3].text(), ",");
        assert_eq!(tokens[4].text(), "X");
    }

    fn assert_is_expected_token(actual: &Token, expected_type: &TokenType) {
        assert_eq!(actual.token_type(), expected_type);
    }

    #[test]
    fn scan_with_valid_text_produces_expected_tokens() {
        let token_text = "loop:  LDA #<label\n  BNE loop";
        let mut scanner = Scanner::new(token_text);
        let tokens = scanner.scan().unwrap();
        assert_is_expected_token(&tokens[0], &TokenType::Symbol(String::from("loop")));
        assert_is_expected_token(&tokens[1], &TokenType::Colon);
        assert_is_expected_token(&tokens[2], &TokenType::Symbol(String::from("LDA")));
        assert_is_expected_token(&tokens[3], &TokenType::Hash);
        assert_is_expected_token(&tokens[4], &TokenType::Lesser);
        assert_is_expected_token(&tokens[5], &TokenType::Symbol(String::from("label")));
        assert_is_expected_token(&tokens[6], &TokenType::Newline);
        assert_is_expected_token(&tokens[7], &TokenType::Symbol(String::from("BNE")));
        assert_is_expected_token(&tokens[8], &TokenType::Symbol(String::from("loop")));
    }

    #[test]
    fn scan_multiple_lines_produces_newline_tokens_between_statements() {
        let mut scanner = Scanner::new("FOO = 1\nBAR = 2\n");
        let tokens = scanner.scan().unwrap();
        assert_eq!(tokens.len(), 8);
        assert_is_expected_token(&tokens[3], &TokenType::Newline);
        assert_eq!(tokens[3].location.line, 1);
        assert_is_expected_token(&tokens[7], &TokenType::Newline);
        assert_eq!(tokens[7].location.line, 2);
        assert_eq!(tokens[4].location.line, 2);
        assert_eq!(tokens[4].location.column, 1);
    }

    #[test]
    fn scan_hexadecimal_with_invalid_digits() {
        let token_text = "LDA $invalid";
        let mut scanner = Scanner::new(token_text);
        let result = scanner.scan();
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("invalid"))
    }

    fn assert_token_sequence_valid(source_text: &str, expected_token_text: &[&str], expected_token_types: &[&TokenType]) {
        assert_eq!(expected_token_text.len(), expected_token_types.len(), "mismatched expectations");
        let mut scanner = Scanner::new(source_text);
        let tokens = scanner.scan().unwrap();
        assert_eq!(tokens.len(), expected_token_text.len());
        for i in 0..tokens.len() {
            assert_eq!(tokens[i].text(), expected_token_text[i]);
            assert_eq!(tokens[i].token_type(), expected_token_types[i]);
        }
    }

    #[test]
    fn scan_number_followed_by_token() {
        assert_token_sequence_valid("0,", &["0", ","],
                                    &[&TokenType::Number(0), &TokenType::Comma]);
        assert_token_sequence_valid("9,", &["9", ","],
                                    &[&TokenType::Number(9), &TokenType::Comma]);
        assert_token_sequence_valid("0b0,", &["0b0", ","],
                                    &[&TokenType::Number(0), &TokenType::Comma]);
        assert_token_sequence_valid("0o0,", &["0o0", ","],
                                    &[&TokenType::Number(0), &TokenType::Comma]);
        assert_token_sequence_valid("01,", &["01", ","],
                                    &[&TokenType::Number(1), &TokenType::Comma]);
        assert_token_sequence_valid("0x0,", &["0x0", ","],
                                    &[&TokenType::Number(0), &TokenType::Comma]);
    }

    #[test]
    fn scan_directive_followed_by_expression() {
        let mut scanner = Scanner::new(".org $8000");
        let tokens = scanner.scan().unwrap();
        assert_eq!(tokens.len(), 2);
        assert_is_expected_token(&tokens[0], &TokenType::Directive(String::from("org")));
        assert_is_expected_token(&tokens[1], &TokenType::Number(0x8000));
    }

}
