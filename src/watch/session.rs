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
/// Holds a `Parser` and shares variable name→index mappings with its associated
/// `WatchEvaluator`. New variables introduced by walrus assignments are allocated
/// through the evaluator's storage.
pub struct WatchCompiler {
    parser: Parser,
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
        }
    }

    /// Parses and compiles `source` into a [`Watchpoint`].
    ///
    /// New variables introduced by walrus assignments are allocated in `evaluator`.
    pub fn compile(&mut self, source: &str, evaluator: &mut WatchEvaluator) -> Result<Watchpoint, Error> {
        match self.parser.parse(source, &mut evaluator.vars)? {
            None => Err(Error::from(0, 0, "empty expression")),
            Some(expr) => {
                let code = compiler::compile(expr);
                evaluator.grow_storage();
                Ok(Watchpoint { source: source.to_string(), code })
            }
        }
    }

    /// Parses and compiles all semicolon-terminated expressions in `source`.
    ///
    /// Returns successfully compiled watchpoints and any errors encountered.
    /// Whitespace-only content between expressions is silently ignored. On a parse error,
    /// parsing resumes at the next semicolon so subsequent expressions are still attempted.
    /// New variables introduced by walrus assignments are allocated in `evaluator`.
    pub fn compile_all(&mut self, source: &str, evaluator: &mut WatchEvaluator) -> (Vec<Watchpoint>, Vec<Error>) {
        let mut watchpoints = Vec::new();
        let mut errors = Vec::new();
        for result in self.parser.parse_all(source, &mut evaluator.vars) {
            match result {
                Ok((expr_source, expr)) => {
                    let code = compiler::compile(expr);
                    evaluator.grow_storage();
                    watchpoints.push(Watchpoint { source: expr_source.to_string(), code });
                }
                Err(e) => errors.push(e),
            }
        }
        (watchpoints, errors)
    }
}

/// An ordered collection of [`Watchpoint`] values that can be evaluated together.
///
/// Owns variable name→index mappings and the runtime storage for variable values.
/// Variables persist across evaluations and are shared across all watchpoints.
pub struct WatchEvaluator {
    watchpoints: Vec<Watchpoint>,
    vars: Variables,
    var_storage: Vec<Operand>,
}

impl WatchEvaluator {

    pub fn new() -> Self {
        Self {
            watchpoints: Vec::new(),
            vars: Variables::new(),
            var_storage: Vec::new(),
        }
    }

    /// Appends a watchpoint to the end of the collection, returning its index.
    pub fn add(&mut self, watchpoint: Watchpoint) -> usize {
        let index = self.watchpoints.len();
        self.watchpoints.push(watchpoint);
        index
    }

    /// Removes and returns the watchpoint at `index`.
    ///
    /// # Panics
    /// Panics if `index` is out of bounds.
    pub fn remove(&mut self, index: usize) -> Watchpoint {
        self.watchpoints.remove(index)
    }

    /// Removes all watchpoints.
    pub fn clear(&mut self) {
        self.watchpoints.clear();
    }

    /// Returns a slice over all watchpoints in order.
    pub fn watchpoints(&self) -> &[Watchpoint] {
        &self.watchpoints
    }

    /// Returns the current variable values, indexed by variable ID.
    pub fn variables(&self) -> &[Operand] {
        &self.var_storage
    }

    /// Sets the value of a variable by ID.
    ///
    /// # Panics
    /// Panics if `id` is out of bounds.
    pub fn set_variable(&mut self, id: usize, value: Operand) {
        self.var_storage[id] = value;
    }

    /// Evaluates all watchpoints in order against `context`.
    ///
    /// Returns `Ok(None)` if no watchpoint triggered, `Ok(Some(index))` if the watchpoint
    /// at `index` yielded a non-zero result, or `Err((index, error))` if evaluation of the
    /// watchpoint at `index` failed. Variable assignments in earlier watchpoints update
    /// internal storage and are visible to subsequent watchpoints in the same call.
    pub fn evaluate_all(
        &mut self,
        context: &dyn WatchContext,
    ) -> Result<Option<usize>, (usize, WatchError)> {
        for (i, wp) in self.watchpoints.iter().enumerate() {
            match eval(&wp.code, context, &mut self.var_storage) {
                Ok(value) if value != 0 => return Ok(Some(i)),
                Ok(_) => {}
                Err(e) => return Err((i, e)),
            }
        }
        Ok(None)
    }

    fn grow_storage(&mut self) {
        let needed = self.vars.len();
        if self.var_storage.len() < needed {
            self.var_storage.resize(needed, 0);
        }
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
        let mut ev = WatchEvaluator::new();
        let wp = compiler().compile("A == 0", &mut ev).unwrap();
        assert_eq!(wp.source(), "A == 0");
    }

    #[test]
    fn compile_empty_expression_returns_error() {
        assert!(compiler().compile("", &mut WatchEvaluator::new()).is_err());
    }

    #[test]
    fn compile_invalid_expression_returns_error() {
        assert!(compiler().compile("A ==", &mut WatchEvaluator::new()).is_err());
    }

    #[test]
    fn compile_grows_var_storage_for_new_variables() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        let wp = c.compile("x := A", &mut ev).unwrap();
        ev.add(wp);
        assert_eq!(ev.variables().len(), 1);
        let wp = c.compile("y := A", &mut ev).unwrap();
        ev.add(wp);
        assert_eq!(ev.variables().len(), 2);
    }

    #[test]
    fn compile_all_empty_source_returns_empty() {
        let (wps, errs) = compiler().compile_all("", &mut WatchEvaluator::new());
        assert!(wps.is_empty());
        assert!(errs.is_empty());
    }

    #[test]
    fn compile_all_single_expression() {
        let (wps, errs) = compiler().compile_all("A == 0;", &mut WatchEvaluator::new());
        assert_eq!(wps.len(), 1);
        assert!(errs.is_empty());
        assert_eq!(wps[0].source(), "A == 0");
    }

    #[test]
    fn compile_all_multiple_expressions() {
        let (wps, errs) = compiler().compile_all("A == 0;\nA == 1;", &mut WatchEvaluator::new());
        assert_eq!(wps.len(), 2);
        assert!(errs.is_empty());
        assert_eq!(wps[0].source(), "A == 0");
        assert_eq!(wps[1].source(), "A == 1");
    }

    #[test]
    fn compile_all_whitespace_between_expressions_is_ignored() {
        let (wps, errs) = compiler().compile_all("A == 0;\n\n   \nA == 1;", &mut WatchEvaluator::new());
        assert_eq!(wps.len(), 2);
        assert!(errs.is_empty());
    }

    #[test]
    fn compile_all_collects_errors_and_continues() {
        let (wps, errs) = compiler().compile_all("A == 0;\nA ==;\nA == 2;", &mut WatchEvaluator::new());
        assert_eq!(wps.len(), 2);
        assert_eq!(errs.len(), 1);
        assert_eq!(wps[0].source(), "A == 0");
        assert_eq!(wps[1].source(), "A == 2");
    }

    // --- WatchEvaluator tests ---

    #[test]
    fn evaluate_all_returns_none_when_empty() {
        let mut ev = WatchEvaluator::new();
        assert_eq!(ev.evaluate_all(&MockMachine::new()), Ok(None));
    }

    #[test]
    fn evaluate_all_returns_none_when_no_watchpoint_triggered() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        let wp = c.compile("A == 99", &mut ev).unwrap();
        ev.add(wp);
        assert_eq!(ev.evaluate_all(&MockMachine::with_register(0)), Ok(None));
    }

    #[test]
    fn evaluate_all_returns_index_when_watchpoint_triggered() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        let wp = c.compile("A == 42", &mut ev).unwrap();
        ev.add(wp);
        assert_eq!(ev.evaluate_all(&MockMachine::with_register(42)), Ok(Some(0)));
    }

    #[test]
    fn evaluate_all_returns_first_triggered_index() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        let wp0 = c.compile("A == 99", &mut ev).unwrap();
        let wp1 = c.compile("A == 42", &mut ev).unwrap();
        let wp2 = c.compile("A == 42", &mut ev).unwrap();
        ev.add(wp0);
        ev.add(wp1);
        ev.add(wp2);
        assert_eq!(ev.evaluate_all(&MockMachine::with_register(42)), Ok(Some(1)));
    }

    #[test]
    fn evaluate_all_returns_error_with_index() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        let wp0 = c.compile("A == 99", &mut ev).unwrap();
        let wp1 = c.compile("A / 0", &mut ev).unwrap();
        ev.add(wp0);
        ev.add(wp1);
        assert_eq!(
            ev.evaluate_all(&MockMachine::with_register(0)),
            Err((1, WatchError::DivisionByZero))
        );
    }

    #[test]
    fn evaluate_all_variable_assignments_visible_to_subsequent_watchpoints() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        // x := A yields 0 when A is 0 (not triggered), but assigns x = 0
        let wp_assign = c.compile("x := A", &mut ev).unwrap();
        // x == 0 is true (1) — proves the assignment from wp_assign is visible
        let wp_check = c.compile("x == 0", &mut ev).unwrap();
        ev.add(wp_assign);
        ev.add(wp_check);
        assert_eq!(ev.evaluate_all(&MockMachine::with_register(0)), Ok(Some(1)));
    }

    #[test]
    fn add_returns_index() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        let wp0 = c.compile("A == 0", &mut ev).unwrap();
        let wp1 = c.compile("A == 1", &mut ev).unwrap();
        assert_eq!(ev.add(wp0), 0);
        assert_eq!(ev.add(wp1), 1);
    }

    #[test]
    fn clear_removes_all_watchpoints() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        let wp0 = c.compile("A == 0", &mut ev).unwrap();
        let wp1 = c.compile("A == 1", &mut ev).unwrap();
        ev.add(wp0);
        ev.add(wp1);
        ev.clear();
        assert!(ev.watchpoints().is_empty());
    }

    #[test]
    fn set_variable_updates_storage() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        let wp = c.compile("x := A", &mut ev).unwrap();
        ev.add(wp);
        let _ = ev.evaluate_all(&MockMachine::with_register(42));
        assert_eq!(ev.variables()[0], 42);
        ev.set_variable(0, 99);
        assert_eq!(ev.variables()[0], 99);
    }

    #[test]
    fn remove_watchpoint_by_index() {
        let mut c = compiler();
        let mut ev = WatchEvaluator::new();
        let wp1 = c.compile("A == 0", &mut ev).unwrap();
        let wp2 = c.compile("A == 1", &mut ev).unwrap();
        ev.add(wp1);
        ev.add(wp2);
        let removed = ev.remove(0);
        assert_eq!(removed.source(), "A == 0");
        assert_eq!(ev.watchpoints().len(), 1);
        assert_eq!(ev.watchpoints()[0].source(), "A == 1");
    }
}