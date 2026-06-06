
#[derive(Debug, Clone)]
pub struct Text<'a> {
    buf: &'a str,
    start: usize,
    current: usize,
}

impl<'a> Text<'a> {

    pub fn from(buf: &'a str) -> Self {
        Self {
            buf,
            start: 0,
            current: 0,
        }
    }

    pub fn is_at_end(&self) -> bool {
        self.current == self.buf.len()
    }

    pub fn advance(&mut self) -> Option<u8> {
        if self.is_at_end() {
            return None
        }
        let c = self.buf.as_bytes()[self.current];

        self.current += 1;

        Some(c)
    }

    pub fn peek(&self) -> Option<u8> {
        if self.is_at_end() {
            return None
        }
        Some(self.buf.as_bytes()[self.current])
    }

    pub fn skip(&mut self) {
        self.current += 1;
        self.start += 1;
    }

    pub fn consume(&mut self) -> &'a str {
        let text = &self.buf[self.start..self.current];
        self.start = self.current;
        text
    }

}


#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_from_when_not_empty() {
        let t = Text::from("not empty");
        assert!(!t.is_at_end());
    }

    #[test]
    fn test_from_when_empty() {
        let t = Text::from("");
        assert!(t.is_at_end());
    }

    #[test]
    fn test_advance_and_consume() {
        let mut t = Text::from("abc");
        assert!(!t.is_at_end());
        assert_eq!(t.advance(), Some(b'a'));
        assert_eq!(t.advance(), Some(b'b'));
        assert!(!t.is_at_end());
        assert_eq!(t.consume(), "ab");
        assert_eq!(t.advance(), Some(b'c'));
        assert!(t.is_at_end());
        assert_eq!(t.consume(), "c");
    }

    #[test]
    fn test_peek() {
        let mut t = Text::from("a");
        assert!(!t.is_at_end());
        assert_eq!(t.peek(), Some(b'a'));
        assert!(!t.is_at_end());
        assert_eq!(t.advance(), Some(b'a'));
        assert!(t.is_at_end());
    }

}
