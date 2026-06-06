use super::compiler::OpCode;
use super::expr::Operand;

/// A context for evaluating a watch expression.
///
/// This trait is implemented by the emulator to provide safe access to the current state of
/// the machine's registers and flags, as well as idempotent access to system memory and mapped
/// I/O devices.
///
/// The mapping of register names and flag names to the [`super::expr::Operand`] type used to
/// identify registers and flags in the compiler's byte code is handled by a
/// [`super::parser::Mapper`] that is passed to the expression parser. These mappings are
/// architecture-specific.
///
/// Addresses specified as arguments to memory fetch functions that are larger than the address
/// space of the machine architecture will "wrap around"; i.e. the address argument will be
/// evaluated modulo the size of the address space.
///
pub trait EvalContext {

    /// Fetches the contents of a machine register, returning an unsigned value, extended to the
    /// width of [super::expr::Operand].
    ///
    /// # Arguments
    /// `register_id` - ID of the register to fetch
    ///
    fn fetch_register(&self, register_id: Operand) -> Operand;

    /// Fetches the contents of a machine register, returning a signed value, sign-extended to the
    /// width of [super::expr::Operand].
    ///
    /// # Arguments
    /// `register_id` - ID of the register to fetch
    ///
    fn fetch_register_signed(&self, register_id: Operand) -> Operand;

    /// Fetches the state of a machine flag. Returns 1 if the flag is set, else 0.
    ///
    /// # Arguments
    /// `flag_id` - ID of the flag to fetch
    ///
    fn fetch_flag(&self, flag_id: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning an unsigned byte
    /// extended to the width of [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_byte(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning a signed byte,
    /// sign-extended to the width of [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_byte_signed(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning an unsigned word
    /// (two consecutive bytes in the architecture's byte order) extended to the width of
    /// [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_word(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning a signed word
    /// (two consecutive bytes in the architecture's byte order) sign-extended to the width of
    /// [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_word_signed(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning an unsigned double word
    /// (four consecutive bytes in the architecture's byte order) extended to the width of
    /// [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_dword(&self, address: Operand) -> Operand;

    /// Performs an idempotent fetch at the given memory address, returning a signed double word
    /// (four consecutive bytes in the architecture's byte order) sign-extended to the width of
    /// [super::expr::Operand].
    ///
    /// # Arguments
    /// `address` - the subject address
    ///
    fn fetch_dword_signed(&self, address: Operand) -> Operand;

}

/// A stack used to evaluate a watch expression.
///
struct Stack {
    /// A vector that serves as the backing storage for stack.
    delegate: Vec<Operand>,
}

impl Stack {

    /// Creates a new empty stack instance with initial capacity sufficient to handle
    /// most expressions without the need for reallocation.
    fn new() -> Self {
       Self {
           delegate: Vec::with_capacity(16),    // capacity adequate for most expressions
       }
    }

    /// Pushes an operand onto the stack.
    ///
    /// # Arguments
    /// * `v` - the operand value to push
    ///
    fn push(&mut self, v: Operand) {
        self.delegate.push(v);
    }

    /// Pops an operand from the stack returning the operand value, panicking with a stack
    /// underflow message if the stack is empty.
    ///
    fn pop(&mut self) -> Operand {
        self.delegate.pop().expect("stack underflow")
    }

}

/// Evaluates a byte code expression and returns the result as an [`super::expr::Operand`].
///
/// # Arguments
/// * `code` - the subject byte code expression
/// * `context` - machine context for registers, flags, and addresses referenced in the expression
/// * `vars` - bindings for variables referenced within the expression
///
pub fn eval(code: &[OpCode], context: &dyn EvalContext, vars: &mut [Operand]) -> Operand {
    let mut stack = Stack::new();
    for opcode in code {
        match opcode {
            OpCode::PushImmediate(n) => stack.push(*n),
            OpCode::FetchRegister(n) => stack.push(context.fetch_register(*n)),
            OpCode::FetchRegisterSigned(n) => stack.push(context.fetch_register_signed(*n)),
            OpCode::FetchFlag(n) => stack.push(context.fetch_flag(*n)),
            OpCode::PushVariable(id) => stack.push(vars[*id as usize]),
            OpCode::AssignAndPushVariable(id) => {
                let v = stack.pop();
                vars[*id as usize] = v;
                stack.push(v);
            }
            OpCode::FetchByte => {
                let x = stack.pop();
                stack.push(context.fetch_byte(x));
            }
            OpCode::FetchByteSigned => {
                let x = stack.pop();
                stack.push(context.fetch_byte_signed(x));
            },
            OpCode::FetchWord => {
                let x = stack.pop();
                stack.push(context.fetch_word(x));
            },
            OpCode::FetchWordSigned => {
                let x = stack.pop();
                stack.push(context.fetch_word_signed(x));
            },
            OpCode::FetchDWord => {
                let x = stack.pop();
                stack.push(context.fetch_dword(x));
            },
            OpCode::FetchDWordSigned => {
                let x = stack.pop();
                stack.push(context.fetch_dword_signed(x));
            },
            OpCode::Add => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y + x);
            }
            OpCode::Subtract => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y - x);
            }
            OpCode::Multiply => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y * x);
            }
            OpCode::Divide => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y / x);
            }
            OpCode::DivideSigned => {
                let x = stack.pop() as i32;
                let y = stack.pop() as i32;
                stack.push((y / x) as Operand);
            }
            OpCode::Remainder => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y % x);
            }
            OpCode::RemainderSigned => {
                let x = stack.pop() as i32;
                let y = stack.pop() as i32;
                stack.push((y % x) as Operand);
            }
            OpCode::Negate => {
                let x = stack.pop() as i32;
                stack.push((-x) as Operand);
            }
            OpCode::Equal => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push((y == x) as Operand);
            }
            OpCode::NotEqual => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push((y != x) as Operand);
            }
            OpCode::GreaterThan => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push((y > x) as Operand);
            }
            OpCode::GreaterThanSigned => {
                let x = stack.pop() as i32;
                let y = stack.pop() as i32;
                stack.push((y > x) as Operand);
            }
            OpCode::GreaterOrEqual => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push((y >= x) as Operand);
            }
            OpCode::GreaterOrEqualSigned => {
                let x = stack.pop() as i32;
                let y = stack.pop() as i32;
                stack.push((y >= x) as Operand);
            }
            OpCode::LessThan => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push((y < x) as Operand);
            }
            OpCode::LessThanSigned => {
                let x = stack.pop() as i32;
                let y = stack.pop() as i32;
                stack.push((y < x) as Operand);
            }
            OpCode::LessOrEqual => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push((y <= x) as Operand);
            }
            OpCode::LessOrEqualSigned => {
                let x = stack.pop() as i32;
                let y = stack.pop() as i32;
                stack.push((y <= x) as Operand);
            }
            OpCode::LeftShift => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y << x);
            }
            OpCode::RightShift => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y >> x);
            }
            OpCode::RightShiftSigned => {
                let x = stack.pop() as i32;
                let y = stack.pop() as i32;
                stack.push((y >> x) as Operand);
            }
            OpCode::LogicalAnd => {
                let x = stack.pop() != 0;
                let y = stack.pop() != 0;
                stack.push((y && x) as Operand);
            }
            OpCode::LogicalOr => {
                let x = stack.pop() != 0;
                let y = stack.pop() != 0;
                stack.push((y || x) as Operand);
            }
            OpCode::LogicalNot => {
                let x = stack.pop() != 0;
                stack.push((!x) as Operand);
            }
            OpCode::BitwiseAnd => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y & x);
            }
            OpCode::BitwiseOr => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y | x);
            }
            OpCode::BitwiseXor => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y ^ x);
            }
            OpCode::BitwiseNot => {
                let x = stack.pop();
                stack.push(!x);
            }
        }
    }
    stack.pop()
}

#[cfg(test)]

mod tests {

    use super::*;

    struct MockMachine {
        register: Operand,
        flag: Operand,
        memory_byte: Operand,
        memory_word: Operand,
        memory_dword: Operand,
    }

    impl MockMachine {
        fn new() -> Self {
            Self {
                register: 0,
                flag: 0,
                memory_byte: 0,
                memory_word: 0,
                memory_dword: 0,
            }
        }
    }

    impl EvalContext for MockMachine {

        fn fetch_register(&self, _register_id: Operand) -> Operand {
            self.register
        }

        fn fetch_register_signed(&self, _register_id: Operand) -> Operand {
            self.register
        }

        fn fetch_flag(&self, _flag_id: Operand) -> Operand {
            self.flag
        }

        fn fetch_byte(&self, _address: Operand) -> Operand {
            self.memory_byte
        }

        fn fetch_byte_signed(&self, _address: Operand) -> Operand {
            self.memory_byte
        }

        fn fetch_word(&self, _address: Operand) -> Operand {
            self.memory_word
        }

        fn fetch_word_signed(&self, _address: Operand) -> Operand {
            self.memory_word
        }

        fn fetch_dword(&self, _address: Operand) -> Operand {
            self.memory_dword
        }

        fn fetch_dword_signed(&self, _address: Operand) -> Operand {
            self.memory_dword
        }
    }

    fn no_vars() -> Vec<Operand> { vec![] }

    #[test]
    fn push_immediate() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(42)], &machine, &mut no_vars());
        assert_eq!(result, 42);
    }

    #[test]
    fn push_register() {
        let mut machine = MockMachine::new();
        machine.register = 42;
        let result = eval(&vec![OpCode::FetchRegister(0)], &machine, &mut no_vars());
        assert_eq!(result, 42);
    }

    #[test]
    fn push_register_signed() {
        let mut machine = MockMachine::new();
        machine.register = -1i32 as Operand;
        let result = eval(&vec![OpCode::FetchRegisterSigned(0)], &machine, &mut no_vars());
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn push_flag() {
        let mut machine = MockMachine::new();
        machine.flag = 42;
        let result = eval(&vec![OpCode::FetchFlag(0)], &machine, &mut no_vars());
        assert_eq!(result, 42);
    }

    #[test]
    fn fetch_byte() {
        let mut machine = MockMachine::new();
        machine.memory_byte = 42;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchByte], &machine, &mut no_vars());
        assert_eq!(result, 42);
    }

    #[test]
    fn fetch_byte_signed() {
        let mut machine = MockMachine::new();
        machine.memory_byte = -1i32 as Operand;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchByteSigned], &machine, &mut no_vars());
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn fetch_word() {
        let mut machine = MockMachine::new();
        machine.memory_word = 42;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchWord], &machine, &mut no_vars());
        assert_eq!(result, 42);
    }

    #[test]
    fn fetch_word_signed() {
        let mut machine = MockMachine::new();
        machine.memory_word = -1i32 as Operand;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchWordSigned], &machine, &mut no_vars());
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn fetch_dword() {
        let mut machine = MockMachine::new();
        machine.memory_dword = 42;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchDWord], &machine, &mut no_vars());
        assert_eq!(result, 42);
    }

    #[test]
    fn fetch_dword_signed() {
        let mut machine = MockMachine::new();
        machine.memory_dword = -1i32 as Operand;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchDWordSigned], &machine, &mut no_vars());
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn negate() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate], &machine, &mut no_vars());
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn add() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::Add], &machine, &mut no_vars());
        assert_eq!(result, 2);
    }

    #[test]
    fn subtract() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(2), OpCode::PushImmediate(1), OpCode::Subtract], &machine, &mut no_vars());
        assert_eq!(result, 1);
    }

    #[test]
    fn multiply() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(2), OpCode::Multiply], &machine, &mut no_vars());
        assert_eq!(result, 2);
    }

    #[test]
    fn divide() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(4), OpCode::PushImmediate(2), OpCode::Divide], &machine, &mut no_vars());
        assert_eq!(result, 2);
    }

    #[test]
    fn divide_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(4), OpCode::Negate, OpCode::PushImmediate(2), OpCode::DivideSigned], &machine, &mut no_vars());
        assert_eq!(result, (-4i32 / 2i32) as Operand);
    }

    #[test]
    fn remainder() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(5), OpCode::PushImmediate(2), OpCode::Remainder], &machine, &mut no_vars());
        assert_eq!(result, 1);
    }

    #[test]
    fn remainder_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(5), OpCode::Negate, OpCode::PushImmediate(2), OpCode::RemainderSigned], &machine, &mut no_vars());
        assert_eq!(result, (-5i32 % 2i32) as Operand);
    }

    #[test]
    fn equal() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::Equal], &machine, &mut no_vars());
        assert_eq!(result, 0);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::Equal], &machine, &mut no_vars());
        assert_ne!(result, 0);
    }

    #[test]
    fn not_equal() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::NotEqual], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::NotEqual], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn greater_than() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::GreaterThan], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::GreaterThan], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn greater_than_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::Negate, OpCode::GreaterThanSigned], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate, OpCode::PushImmediate(1), OpCode::GreaterThanSigned], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn greater_or_equal() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::GreaterOrEqual], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::GreaterOrEqual], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::GreaterOrEqual], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn greater_or_equal_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::Negate, OpCode::GreaterOrEqualSigned], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(0), OpCode::GreaterOrEqualSigned], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate, OpCode::PushImmediate(0), OpCode::GreaterOrEqualSigned], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn less_than() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::LessThan], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::LessThan], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn less_than_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate, OpCode::PushImmediate(1), OpCode::LessThanSigned], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::Negate, OpCode::LessThanSigned], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn less_or_equal() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::LessOrEqual], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::LessOrEqual], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::LessOrEqual], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn less_or_equal_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate, OpCode::PushImmediate(0), OpCode::Negate, OpCode::LessOrEqualSigned], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(0), OpCode::LessOrEqualSigned], &machine, &mut no_vars());
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::Negate, OpCode::LessOrEqualSigned], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn left_shift() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::LeftShift], &machine, &mut no_vars());
        assert_eq!(result, 2);
    }

    #[test]
    fn right_shift() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(2), OpCode::PushImmediate(1), OpCode::RightShift], &machine, &mut no_vars());
        assert_eq!(result, 1);
    }

    #[test]
    fn right_shift_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0x80000000), OpCode::PushImmediate(1), OpCode::RightShiftSigned], &machine, &mut no_vars());
        assert_eq!(result, 0xc0000000);
    }

    #[test]
    fn logical_not() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::LogicalNot], &machine, &mut no_vars());
        assert_eq!(result, 0);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::LogicalNot], &machine, &mut no_vars());
        assert_ne!(result, 0);
    }

    #[test]
    fn logical_and() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::LogicalAnd], &machine, &mut no_vars());
        assert_ne!(result, 0);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::LogicalAnd], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn logical_or() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::LogicalOr], &machine, &mut no_vars());
        assert_ne!(result, 0);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(0), OpCode::LogicalOr], &machine, &mut no_vars());
        assert_eq!(result, 0);
    }

    #[test]
    fn bitwise_not() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::BitwiseNot], &machine, &mut no_vars());
        assert_eq!(result, !1);
    }

    #[test]
    fn bitwise_and() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0xff), OpCode::PushImmediate(0x55), OpCode::BitwiseAnd], &machine, &mut no_vars());
        assert_eq!(result, 0x55);
    }

    #[test]
    fn bitwise_or() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0x55), OpCode::PushImmediate(0xaa), OpCode::BitwiseOr], &machine, &mut no_vars());
        assert_eq!(result, 0xff);
    }

    #[test]
    fn bitwise_xor() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0x55), OpCode::PushImmediate(0xff), OpCode::BitwiseXor], &machine, &mut no_vars());
        assert_eq!(result, 0xaa);
    }

    #[test]
    fn push_and_store_variable() {
        let machine = MockMachine::new();
        let mut vars = vec![0u32; 1];
        eval(&vec![OpCode::PushImmediate(99), OpCode::AssignAndPushVariable(0)], &machine, &mut vars);
        assert_eq!(vars[0], 99);
        let result = eval(&vec![OpCode::PushVariable(0)], &machine, &mut vars);
        assert_eq!(result, 99);
    }

    #[test]
    fn store_variable_leaves_value_on_stack() {
        let machine = MockMachine::new();
        let mut vars = vec![0u32; 1];
        let result = eval(&vec![OpCode::PushImmediate(42), OpCode::AssignAndPushVariable(0)], &machine, &mut vars);
        assert_eq!(result, 42);
    }

}
