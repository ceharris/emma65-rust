use crate::watch::context::WatchContext;
use super::compiler;
use super::compiler::OpCode;
use super::error::{Error, WatchError};
use super::evaluator::eval;
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

/// Compiles watch expressions into [`Watchpoint`] values.
///
/// The caller owns a `Vec<Operand>` for variable storage and passes it mutably to compile
/// methods, which grow it as new variables are introduced by walrus assignments.
pub struct WatchCompiler {
    parser: Parser,
    vars: Variables,
}

impl WatchCompiler {

    /// Creates a new compiler.
    ///
    /// # Arguments
    /// * `map_register` - maps a register name to an [`Operand`] ID
    /// * `map_flag` - maps a flag name to an [`Operand`] ID
    /// * `map_symbol` - maps a symbol name to a constant [`Operand`] value
    pub fn new(
        map_register: impl Fn(&str) -> Option<Operand> + 'static,
        map_flag: impl Fn(&str) -> Option<Operand> + 'static,
        map_symbol: impl Fn(&str) -> Option<Operand> + 'static,
    ) -> Self {
        Self {
            parser: Parser::from(map_register, map_flag, map_symbol),
            vars: Variables::new(),
        }
    }

    /// Parses and compiles `source` into a [`Watchpoint`].
    ///
    /// Grows `var_storage` if new variables are introduced by walrus assignments.
    pub fn compile(&mut self, source: &str, var_storage: &mut Vec<Operand>) -> Result<Watchpoint, Error> {
        match self.parser.parse(source, &mut self.vars)? {
            None => Err(Error::from(0, 0, "empty expression")),
            Some(expr) => {
                let code = compiler::compile(expr);
                Self::grow(var_storage, self.vars.len());
                Ok(Watchpoint { source: source.to_string(), code })
            }
        }
    }

    /// Parses and compiles all semicolon-terminated expressions in `source`.
    ///
    /// Returns successfully compiled watchpoints and any errors encountered.
    /// Whitespace-only content between expressions is silently ignored. On a parse error,
    /// parsing resumes at the next semicolon so subsequent expressions are still attempted.
    /// Grows `var_storage` as new variables are introduced.
    pub fn compile_all(&mut self, source: &str, var_storage: &mut Vec<Operand>) -> (Vec<Watchpoint>, Vec<Error>) {
        let mut watchpoints = Vec::new();
        let mut errors = Vec::new();
        for result in self.parser.parse_all(source, &mut self.vars) {
            match result {
                Ok((expr_source, expr)) => {
                    let code = compiler::compile(expr);
                    Self::grow(var_storage, self.vars.len());
                    watchpoints.push(Watchpoint { source: expr_source.to_string(), code });
                }
                Err(e) => errors.push(e),
            }
        }
        (watchpoints, errors)
    }

    fn grow(var_storage: &mut Vec<Operand>, needed: usize) {
        if var_storage.len() < needed {
            var_storage.resize(needed, 0);
        }
    }
}

/// An ordered collection of [`Watchpoint`] values that can be evaluated together.
pub struct WatchEvaluator {
    watchpoints: Vec<Watchpoint>,
}

impl WatchEvaluator {

    pub fn new() -> Self {
        Self { watchpoints: Vec::new() }
    }

    /// Appends a watchpoint to the end of the collection.
    pub fn add(&mut self, watchpoint: Watchpoint) {
        self.watchpoints.push(watchpoint);
    }

    /// Removes and returns the watchpoint at `index`.
    ///
    /// # Panics
    /// Panics if `index` is out of bounds.
    pub fn remove(&mut self, index: usize) -> Watchpoint {
        self.watchpoints.remove(index)
    }

    /// Returns a slice over all watchpoints in order.
    pub fn watchpoints(&self) -> &[Watchpoint] {
        &self.watchpoints
    }

    /// Evaluates all watchpoints in order against `context` and `var_storage`.
    ///
    /// Returns `Ok(None)` if no watchpoint triggered, `Ok(Some(index))` if the watchpoint
    /// at `index` yielded a non-zero result, or `Err((index, error))` if evaluation of the
    /// watchpoint at `index` failed. Variable assignments in earlier watchpoints update
    /// `var_storage` and are visible to subsequent watchpoints in the same call.
    pub fn evaluate_all(
        &self,
        context: &dyn WatchContext,
        var_storage: &mut [Operand],
    ) -> Result<Option<usize>, (usize, WatchError)> {
        for (i, wp) in self.watchpoints.iter().enumerate() {
            match eval(&wp.code, context, var_storage) {
                Ok(value) if value != 0 => return Ok(Some(i)),
                Ok(_) => {}
                Err(e) => return Err((i, e)),
            }
        }
        Ok(None)
    }
}

impl Default for WatchEvaluator {
    fn default() -> Self {
        Self::new()
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

    struct MockMachine {
        register: Operand,
    }

    impl MockMachine {
        fn new() -> Self { Self { register: 0 } }
        fn with_register(register: Operand) -> Self { Self { register } }
    }

    impl WatchContext for MockMachine {
        fn read_register_u32(&self, _id: Operand) -> Operand { self.register }
        fn read_register_i32(&self, _id: Operand) -> Operand { self.register }
        fn read_flag(&self, _id: Operand) -> Operand { 0 }
        fn read_mem_u32(&self, _addr: u16, _width: u8) -> u32 { 0 }
        fn read_mem_i32(&self, _addr: u16, _width: u8) -> u32 { 0 }
    }

    fn compiler() -> WatchCompiler {
        WatchCompiler::new(register_mapper, |_| None, |_| None)
    }

    // --- WatchCompiler tests ---

    #[test]
    fn compile_valid_expression() {
        let mut vars = Vec::new();
        let wp = compiler().compile("A == 0", &mut vars).unwrap();
        assert_eq!(wp.source(), "A == 0");
    }

    #[test]
    fn compile_empty_expression_returns_error() {
        assert!(compiler().compile("", &mut Vec::new()).is_err());
    }

    #[test]
    fn compile_invalid_expression_returns_error() {
        assert!(compiler().compile("A ==", &mut Vec::new()).is_err());
    }

    #[test]
    fn compile_grows_var_storage_for_new_variables() {
        let mut c = compiler();
        let mut vars = Vec::new();
        c.compile("x := A", &mut vars).unwrap();
        assert_eq!(vars.len(), 1);
        c.compile("y := A", &mut vars).unwrap();
        assert_eq!(vars.len(), 2);
    }

    #[test]
    fn compile_all_empty_source_returns_empty() {
        let (wps, errs) = compiler().compile_all("", &mut Vec::new());
        assert!(wps.is_empty());
        assert!(errs.is_empty());
    }

    #[test]
    fn compile_all_single_expression() {
        let (wps, errs) = compiler().compile_all("A == 0;", &mut Vec::new());
        assert_eq!(wps.len(), 1);
        assert!(errs.is_empty());
        assert_eq!(wps[0].source(), "A == 0");
    }

    #[test]
    fn compile_all_multiple_expressions() {
        let (wps, errs) = compiler().compile_all("A == 0;\nA == 1;", &mut Vec::new());
        assert_eq!(wps.len(), 2);
        assert!(errs.is_empty());
        assert_eq!(wps[0].source(), "A == 0");
        assert_eq!(wps[1].source(), "A == 1");
    }

    #[test]
    fn compile_all_whitespace_between_expressions_is_ignored() {
        let (wps, errs) = compiler().compile_all("A == 0;\n\n   \nA == 1;", &mut Vec::new());
        assert_eq!(wps.len(), 2);
        assert!(errs.is_empty());
    }

    #[test]
    fn compile_all_collects_errors_and_continues() {
        let (wps, errs) = compiler().compile_all("A == 0;\nA ==;\nA == 2;", &mut Vec::new());
        assert_eq!(wps.len(), 2);
        assert_eq!(errs.len(), 1);
        assert_eq!(wps[0].source(), "A == 0");
        assert_eq!(wps[1].source(), "A == 2");
    }

    // --- WatchEvaluator tests ---

    #[test]
    fn evaluate_all_returns_none_when_empty() {
        let ev = WatchEvaluator::new();
        assert_eq!(ev.evaluate_all(&MockMachine::new(), &mut Vec::new()), Ok(None));
    }

    #[test]
    fn evaluate_all_returns_none_when_no_watchpoint_triggered() {
        let mut c = compiler();
        let mut vars = Vec::new();
        let wp = c.compile("A == 99", &mut vars).unwrap();
        let mut ev = WatchEvaluator::new();
        ev.add(wp);
        assert_eq!(ev.evaluate_all(&MockMachine::with_register(0), &mut vars), Ok(None));
    }

    #[test]
    fn evaluate_all_returns_index_when_watchpoint_triggered() {
        let mut c = compiler();
        let mut vars = Vec::new();
        let wp = c.compile("A == 42", &mut vars).unwrap();
        let mut ev = WatchEvaluator::new();
        ev.add(wp);
        assert_eq!(ev.evaluate_all(&MockMachine::with_register(42), &mut vars), Ok(Some(0)));
    }

    #[test]
    fn evaluate_all_returns_first_triggered_index() {
        let mut c = compiler();
        let mut vars = Vec::new();
        let wp0 = c.compile("A == 99", &mut vars).unwrap();
        let wp1 = c.compile("A == 42", &mut vars).unwrap();
        let wp2 = c.compile("A == 42", &mut vars).unwrap();
        let mut ev = WatchEvaluator::new();
        ev.add(wp0);
        ev.add(wp1);
        ev.add(wp2);
        assert_eq!(ev.evaluate_all(&MockMachine::with_register(42), &mut vars), Ok(Some(1)));
    }

    #[test]
    fn evaluate_all_returns_error_with_index() {
        let mut c = compiler();
        let mut vars = Vec::new();
        let wp0 = c.compile("A == 99", &mut vars).unwrap();
        let wp1 = c.compile("A / 0", &mut vars).unwrap();
        let mut ev = WatchEvaluator::new();
        ev.add(wp0);
        ev.add(wp1);
        assert_eq!(
            ev.evaluate_all(&MockMachine::with_register(0), &mut vars),
            Err((1, WatchError::DivisionByZero))
        );
    }

    #[test]
    fn evaluate_all_variable_assignments_visible_to_subsequent_watchpoints() {
        let mut c = compiler();
        let mut vars = Vec::new();
        // x := A yields 0 when A is 0 (not triggered), but assigns x = 0
        let wp_assign = c.compile("x := A", &mut vars).unwrap();
        // x == 0 is true (1) — proves the assignment from wp_assign is visible
        let wp_check = c.compile("x == 0", &mut vars).unwrap();
        let mut ev = WatchEvaluator::new();
        ev.add(wp_assign);
        ev.add(wp_check);
        assert_eq!(ev.evaluate_all(&MockMachine::with_register(0), &mut vars), Ok(Some(1)));
    }

    #[test]
    fn remove_watchpoint_by_index() {
        let mut c = compiler();
        let mut vars = Vec::new();
        let wp1 = c.compile("A == 0", &mut vars).unwrap();
        let wp2 = c.compile("A == 1", &mut vars).unwrap();
        let mut ev = WatchEvaluator::new();
        ev.add(wp1);
        ev.add(wp2);
        let removed = ev.remove(0);
        assert_eq!(removed.source(), "A == 0");
        assert_eq!(ev.watchpoints().len(), 1);
        assert_eq!(ev.watchpoints()[0].source(), "A == 1");
    }
}