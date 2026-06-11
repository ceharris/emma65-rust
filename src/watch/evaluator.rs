use crate::watch::context::WatchContext;
use super::compiler::OpCode;
use super::error::WatchError;
use super::expr::Operand;

/// A stack used to evaluate a watch expression.
struct Stack {
    delegate: Vec<Operand>,
}

impl Stack {

    fn new() -> Self {
       Self {
           delegate: Vec::with_capacity(16),
       }
    }

    fn push(&mut self, v: Operand) {
        self.delegate.push(v);
    }

    fn pop(&mut self) -> Operand {
        self.delegate.pop().expect("stack underflow")
    }

}

/// Evaluates a byte code expression and returns the result as an [`Operand`].
pub fn eval(code: &[OpCode], context: &dyn WatchContext, vars: &mut [Operand]) -> Result<Operand, WatchError> {
    let mut stack = Stack::new();
    for opcode in code {
        match opcode {
            OpCode::PushImmediate(n) => stack.push(*n),
            OpCode::FetchRegister(n) => stack.push(context.read_register_u32(*n)),
            OpCode::FetchRegisterSigned(n) => stack.push(context.read_register_i32(*n)),
            OpCode::FetchFlag(n) => stack.push(context.read_flag(*n)),
            OpCode::PushVariable(id) => stack.push(vars[*id as usize]),
            OpCode::AssignAndPushVariable(id) => {
                let v = stack.pop();
                vars[*id as usize] = v;
                stack.push(v);
            }
            OpCode::FetchByte => {
                let x = stack.pop();
                stack.push(context.read_mem_u32(x as u16, 1));
            }
            OpCode::FetchByteSigned => {
                let x = stack.pop();
                stack.push(context.read_mem_i32(x as u16, 1));
            },
            OpCode::FetchWord => {
                let x = stack.pop();
                stack.push(context.read_mem_u32(x as u16, 2));
            },
            OpCode::FetchWordSigned => {
                let x = stack.pop();
                stack.push(context.read_mem_i32(x as u16, 2));
            },
            OpCode::FetchDWord => {
                let x = stack.pop();
                stack.push(context.read_mem_u32(x as u16, 4));
            },
            OpCode::FetchDWordSigned => {
                let x = stack.pop();
                stack.push(context.read_mem_i32(x as u16, 4));
            },
            OpCode::Add => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y.wrapping_add(x));
            }
            OpCode::Subtract => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y.wrapping_sub(x));
            }
            OpCode::Multiply => {
                let x = stack.pop();
                let y = stack.pop();
                stack.push(y.wrapping_mul(x));
            }
            OpCode::Divide => {
                let x = stack.pop();
                if x == 0 { return Err(WatchError::DivisionByZero); }
                let y = stack.pop();
                stack.push(y / x);
            }
            OpCode::DivideSigned => {
                let x = stack.pop() as i32;
                if x == 0 { return Err(WatchError::DivisionByZero); }
                let y = stack.pop() as i32;
                stack.push((y / x) as Operand);
            }
            OpCode::Remainder => {
                let x = stack.pop();
                if x == 0 { return Err(WatchError::DivisionByZero); }
                let y = stack.pop();
                stack.push(y % x);
            }
            OpCode::RemainderSigned => {
                let x = stack.pop() as i32;
                if x == 0 { return Err(WatchError::DivisionByZero); }
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
    Ok(stack.pop())
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

    impl WatchContext for MockMachine {

        fn read_register_u32(&self, _register_id: Operand) -> Operand {
            self.register
        }

        fn read_register_i32(&self, _register_id: Operand) -> Operand {
            self.register
        }

        fn read_flag(&self, _flag_id: Operand) -> Operand {
            self.flag
        }

        fn read_mem_u32(&self, _addr: u16, width: u8) -> u32 {
            match width {
                1 => self.memory_byte,
                2 => self.memory_word,
                4 => self.memory_dword,
                _ => 0,
            }
        }

        fn read_mem_i32(&self, _addr: u16, width: u8) -> u32 {
            match width {
                1 => self.memory_byte,
                2 => self.memory_word,
                4 => self.memory_dword,
                _ => 0,
            }
        }
    }

    fn no_vars() -> Vec<Operand> { vec![] }

    #[test]
    fn push_immediate() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(42)], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn push_register() {
        let mut machine = MockMachine::new();
        machine.register = 42;
        let result = eval(&vec![OpCode::FetchRegister(0)], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn push_register_signed() {
        let mut machine = MockMachine::new();
        machine.register = -1i32 as Operand;
        let result = eval(&vec![OpCode::FetchRegisterSigned(0)], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn push_flag() {
        let mut machine = MockMachine::new();
        machine.flag = 42;
        let result = eval(&vec![OpCode::FetchFlag(0)], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn fetch_byte() {
        let mut machine = MockMachine::new();
        machine.memory_byte = 42;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchByte], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn fetch_byte_signed() {
        let mut machine = MockMachine::new();
        machine.memory_byte = -1i32 as Operand;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchByteSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn fetch_word() {
        let mut machine = MockMachine::new();
        machine.memory_word = 42;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchWord], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn fetch_word_signed() {
        let mut machine = MockMachine::new();
        machine.memory_word = -1i32 as Operand;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchWordSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn fetch_dword() {
        let mut machine = MockMachine::new();
        machine.memory_dword = 42;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchDWord], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn fetch_dword_signed() {
        let mut machine = MockMachine::new();
        machine.memory_dword = -1i32 as Operand;
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::FetchDWordSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn negate() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, -1i32 as Operand);
    }

    #[test]
    fn add() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::Add], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 2);
    }

    #[test]
    fn subtract() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(2), OpCode::PushImmediate(1), OpCode::Subtract], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
    }

    #[test]
    fn multiply() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(2), OpCode::Multiply], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 2);
    }

    #[test]
    fn divide() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(4), OpCode::PushImmediate(2), OpCode::Divide], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 2);
    }

    #[test]
    fn divide_by_zero() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(4), OpCode::PushImmediate(0), OpCode::Divide], &machine, &mut no_vars());
        assert_eq!(result, Err(WatchError::DivisionByZero));
    }

    #[test]
    fn divide_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(4), OpCode::Negate, OpCode::PushImmediate(2), OpCode::DivideSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, (-4i32 / 2i32) as Operand);
    }

    #[test]
    fn divide_signed_by_zero() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(4), OpCode::PushImmediate(0), OpCode::DivideSigned], &machine, &mut no_vars());
        assert_eq!(result, Err(WatchError::DivisionByZero));
    }

    #[test]
    fn remainder() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(5), OpCode::PushImmediate(2), OpCode::Remainder], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
    }

    #[test]
    fn remainder_by_zero() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(5), OpCode::PushImmediate(0), OpCode::Remainder], &machine, &mut no_vars());
        assert_eq!(result, Err(WatchError::DivisionByZero));
    }

    #[test]
    fn remainder_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(5), OpCode::Negate, OpCode::PushImmediate(2), OpCode::RemainderSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, (-5i32 % 2i32) as Operand);
    }

    #[test]
    fn remainder_signed_by_zero() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(5), OpCode::PushImmediate(0), OpCode::RemainderSigned], &machine, &mut no_vars());
        assert_eq!(result, Err(WatchError::DivisionByZero));
    }

    #[test]
    fn equal() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::Equal], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::Equal], &machine, &mut no_vars()).unwrap();
        assert_ne!(result, 0);
    }

    #[test]
    fn not_equal() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::NotEqual], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::NotEqual], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn greater_than() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::GreaterThan], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::GreaterThan], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn greater_than_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::Negate, OpCode::GreaterThanSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate, OpCode::PushImmediate(1), OpCode::GreaterThanSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn greater_or_equal() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::GreaterOrEqual], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::GreaterOrEqual], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::GreaterOrEqual], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn greater_or_equal_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::Negate, OpCode::GreaterOrEqualSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(0), OpCode::GreaterOrEqualSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate, OpCode::PushImmediate(0), OpCode::GreaterOrEqualSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn less_than() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::LessThan], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::LessThan], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn less_than_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate, OpCode::PushImmediate(1), OpCode::LessThanSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::Negate, OpCode::LessThanSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn less_or_equal() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::LessOrEqual], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::LessOrEqual], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::LessOrEqual], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn less_or_equal_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::Negate, OpCode::PushImmediate(0), OpCode::Negate, OpCode::LessOrEqualSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(0), OpCode::LessOrEqualSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::Negate, OpCode::LessOrEqualSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn left_shift() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::LeftShift], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 2);
    }

    #[test]
    fn right_shift() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(2), OpCode::PushImmediate(1), OpCode::RightShift], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 1);
    }

    #[test]
    fn right_shift_signed() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0x80000000), OpCode::PushImmediate(1), OpCode::RightShiftSigned], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0xc0000000);
    }

    #[test]
    fn logical_not() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::LogicalNot], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::LogicalNot], &machine, &mut no_vars()).unwrap();
        assert_ne!(result, 0);
    }

    #[test]
    fn logical_and() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(1), OpCode::LogicalAnd], &machine, &mut no_vars()).unwrap();
        assert_ne!(result, 0);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(1), OpCode::LogicalAnd], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn logical_or() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::PushImmediate(0), OpCode::LogicalOr], &machine, &mut no_vars()).unwrap();
        assert_ne!(result, 0);
        let result = eval(&vec![OpCode::PushImmediate(0), OpCode::PushImmediate(0), OpCode::LogicalOr], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn bitwise_not() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(1), OpCode::BitwiseNot], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, !1);
    }

    #[test]
    fn bitwise_and() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0xff), OpCode::PushImmediate(0x55), OpCode::BitwiseAnd], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0x55);
    }

    #[test]
    fn bitwise_or() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0x55), OpCode::PushImmediate(0xaa), OpCode::BitwiseOr], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0xff);
    }

    #[test]
    fn bitwise_xor() {
        let machine = MockMachine::new();
        let result = eval(&vec![OpCode::PushImmediate(0x55), OpCode::PushImmediate(0xff), OpCode::BitwiseXor], &machine, &mut no_vars()).unwrap();
        assert_eq!(result, 0xaa);
    }

    #[test]
    fn push_and_store_variable() {
        let machine = MockMachine::new();
        let mut vars = vec![0u32; 1];
        eval(&vec![OpCode::PushImmediate(99), OpCode::AssignAndPushVariable(0)], &machine, &mut vars).unwrap();
        assert_eq!(vars[0], 99);
        let result = eval(&vec![OpCode::PushVariable(0)], &machine, &mut vars).unwrap();
        assert_eq!(result, 99);
    }

    #[test]
    fn store_variable_leaves_value_on_stack() {
        let machine = MockMachine::new();
        let mut vars = vec![0u32; 1];
        let result = eval(&vec![OpCode::PushImmediate(42), OpCode::AssignAndPushVariable(0)], &machine, &mut vars).unwrap();
        assert_eq!(result, 42);
    }

}