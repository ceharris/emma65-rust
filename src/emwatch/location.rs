

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

impl Location {
    pub fn from(line: usize, column: usize) -> Self {
        Self {
            line,
            column,
        }
    }
}
