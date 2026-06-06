
#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    line: usize,
    column: usize,
    message: String,
}

impl Error {
    pub fn from(line: usize, column: usize, message: &str) -> Self {
        Self {
            line, column, message: String::from(message),
        }
    }

    pub fn to_string(&self) -> String {
        format!("at {line},{column}: {message}", line=self.line, column=self.column, message=self.message)
    }

}
