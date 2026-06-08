use super::compiler;
use super::compiler::OpCode;
use super::error::Error;
use super::evaluator::{EvalContext, eval};
use super::expr::Operand;
use super::parser::Parser;
use super::variables::Variables;

/// A compiled watch expression, ready for repeated evaluation.
pub struct Watchpoint {
    source: String,
    code: Vec<OpCode>,
}

impl Watchpoint {
    /// Returns the source text this watchpoint was compiled from.
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// A session that compiles and evaluates watch expressions against shared variable state.
pub struct WatchSession {
    parser: Parser,
    vars: Variables,
    var_storage: Vec<Operand>,
}

impl WatchSession {

    /// Creates a new session.
    ///
    /// # Arguments
    /// * `map_register` - a function that maps a register name to an Operand value
    /// * `map_flag` - a function that maps a flag name to an Operand value
    /// * `map_symbol` - a function that maps a symbol name to an Operand value
    pub fn new(
        map_register: impl Fn(&str) -> Option<Operand> + 'static,
        map_flag: impl Fn(&str) -> Option<Operand> + 'static,
        map_symbol: impl Fn(&str) -> Option<Operand> + 'static,
    ) -> Self {
        Self {
            parser: Parser::from(map_register, map_flag, map_symbol),
            vars: Variables::new(),
            var_storage: Vec::new(),
        }
    }

    /// Parses and compiles `source` into a [`Watchpoint`].
    ///
    /// Any variables introduced by walrus assignments are allocated in shared storage,
    /// where they persist across all watchpoints in this session.
    pub fn compile(&mut self, source: &str) -> Result<Watchpoint, Error> {
        match self.parser.parse(source, &mut self.vars)? {
            None => Err(Error::from(0, 0, "empty expression")),
            Some(expr) => {
                let code = compiler::compile(expr);
                if self.var_storage.len() < self.vars.len() {
                    self.var_storage.resize(self.vars.len(), 0);
                }
                Ok(Watchpoint { source: source.to_string(), code })
            }
        }
    }

    /// Evaluates a compiled watchpoint against the given machine context.
    ///
    /// Variable assignments (walrus expressions) in `watchpoint` update shared storage
    /// that is visible to all watchpoints in this session.
    pub fn eval(&mut self, watchpoint: &Watchpoint, context: &dyn EvalContext) -> Operand {
        eval(&watchpoint.code, context, &mut self.var_storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn register_mapper(name: &str) -> Option<Operand> {
        match name {
            "A" => Some(0),
            _ => None,
        }
    }

    fn flag_mapper(_name: &str) -> Option<Operand> {
        None
    }

    struct MockMachine {
        register: Operand,
    }

    impl MockMachine {
        fn new() -> Self { Self { register: 0 } }
        fn with_register(register: Operand) -> Self { Self { register } }
    }

    impl EvalContext for MockMachine {
        fn fetch_register(&self, _id: Operand) -> Operand { self.register }
        fn fetch_register_signed(&self, _id: Operand) -> Operand { self.register }
        fn fetch_flag(&self, _id: Operand) -> Operand { 0 }
        fn fetch_byte(&self, _address: Operand) -> Operand { 0 }
        fn fetch_byte_signed(&self, _address: Operand) -> Operand { 0 }
        fn fetch_word(&self, _address: Operand) -> Operand { 0 }
        fn fetch_word_signed(&self, _address: Operand) -> Operand { 0 }
        fn fetch_dword(&self, _address: Operand) -> Operand { 0 }
        fn fetch_dword_signed(&self, _address: Operand) -> Operand { 0 }
    }

    fn session() -> WatchSession {
        WatchSession::new(register_mapper, flag_mapper, |_| None)
    }

    #[test]
    fn compile_valid_expression() {
        let mut s = session();
        let wp = s.compile("A == 0").unwrap();
        assert_eq!(wp.source(), "A == 0");
    }

    #[test]
    fn compile_empty_expression_returns_error() {
        assert!(session().compile("").is_err());
    }

    #[test]
    fn compile_invalid_expression_returns_error() {
        assert!(session().compile("A ==").is_err());
    }

    #[test]
    fn eval_returns_correct_result() {
        let mut s = session();
        let wp = s.compile("A == 42").unwrap();
        assert_ne!(s.eval(&wp, &MockMachine::with_register(42)), 0);
        assert_eq!(s.eval(&wp, &MockMachine::with_register(0)), 0);
    }

    #[test]
    fn variables_are_shared_across_watchpoints() {
        let mut s = session();
        let wp_write = s.compile("x := 99").unwrap();
        let wp_read = s.compile("x").unwrap();
        let machine = MockMachine::new();
        s.eval(&wp_write, &machine);
        assert_eq!(s.eval(&wp_read, &machine), 99);
    }

    #[test]
    fn variable_storage_grows_with_each_new_variable() {
        let mut s = session();
        let wp1 = s.compile("x := A").unwrap();
        let wp2 = s.compile("y := A").unwrap();
        let wp3 = s.compile("x + y").unwrap();
        let machine = MockMachine::with_register(5);
        s.eval(&wp1, &machine);
        s.eval(&wp2, &machine);
        assert_eq!(s.eval(&wp3, &machine), 10);
    }
}