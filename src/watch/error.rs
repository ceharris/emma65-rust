/// An error produced while scanning, parsing, or compiling a watch expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    line: usize,
    column: usize,
    message: String,
}

impl Error {

    /// Creates an error instance.
    /// # Arguments
    /// * `line` - source line number where the error occurred
    /// * `column` - source column number where the error occurred
    /// * `message` - a message that describes the error that occurred
    ///
    pub fn from(line: usize, column: usize, message: &str) -> Self {
        Self {
            line, column, message: String::from(message),
        }
    }

}

impl std::fmt::Display for Error {

    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "at {line},{column}: {message}",
               line=self.line, column=self.column, message=self.message)
    }

}

impl std::error::Error for Error {}
