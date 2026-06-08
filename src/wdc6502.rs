use std::ops::BitAnd;
use emma65::watch::Operand;
use emma65::watch::WatchContext;

const REG_A: Operand = 0;
const REG_X: Operand = 1;
const REG_Y: Operand = 2;
const REG_P: Operand = 3;
const REG_S: Operand = 4;
const REG_PC: Operand = 5;

const FLAG_C: Operand = 0x1;
const FLAG_Z: Operand = 0x2;
const FLAG_I: Operand = 0x4;
const FLAG_D: Operand = 0x8;
const FLAG_B: Operand = 0x10;
const FLAG_V: Operand = 0x40;
const FLAG_N: Operand = 0x80;


pub fn map_register_name(name: &str) -> Option<Operand> {
    match name {
        "A" | "a" => Some(REG_A),
        "X" | "x" => Some(REG_X),
        "Y" | "y" => Some(REG_Y),
        "P" | "p" => Some(REG_P),
        "S" | "s" => Some(REG_S),
        "PC" | "pc" | "Pc" | "pC" => Some(REG_PC),
        _ => None,
    }
}

pub fn map_flag_name(name: &str) -> Option<Operand> {
    match name {
        "C" | "c" => Some(FLAG_C),
        "Z" | "z" => Some(FLAG_Z),
        "I" | "i" => Some(FLAG_I),
        "D" | "d" => Some(FLAG_D),
        "B" | "b" => Some(FLAG_B),
        "V" | "v" => Some(FLAG_V),
        "N" | "n" => Some(FLAG_N),
        _ => None,
    }
}

struct Registers {
    a: u8,
    x: u8,
    y: u8,
    p: u8,
    s: u8,
    pc: u16,
}

impl Registers {

    fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            p: 0,
            s: 0,
            pc: 0,
        }
    }

}

pub struct Wdc6502Machine {
    registers: Registers,
    memory: [u8; 0x10000],
}

impl Wdc6502Machine {

    pub fn new() -> Self {
        Self {
            registers: Registers::new(),
            memory: [0; 0x10000]
        }
    }

    pub fn set_a(&mut self, a: u8) {
        self.registers.a = a;
    }

    pub fn set_x(&mut self, x: u8) {
        self.registers.x = x;
    }

    pub fn set_y(&mut self, y: u8) {
        self.registers.y = y;
    }

    pub fn set_p(&mut self, p: u8) {
        self.registers.p = p;
    }

    pub fn set_s(&mut self, s: u8) {
        self.registers.s = s;
    }

    pub fn set_pc(&mut self, pc: u16) {
        self.registers.pc = pc;
    }

    pub fn store_u8(&mut self, address: Operand, b: u8) {
        self.memory[(address & 0xffff) as usize] = b;
    }

    fn fetch_u8(&self, address: Operand) -> u8 {
        self.memory[(address & 0xffff) as usize]
    }

    fn fetch_u16(&self, address: Operand) -> u16 {
        let b0 = self.memory[(address & 0xffff) as usize];
        let b1 = self.memory[((address + 1) & 0xffff) as usize];
        ((b1 as u16) << 8 | b0 as u16) as u16
    }

    fn fetch_u32(&self, address: Operand) -> u32 {
        let b0 = self.memory[(address & 0xffff) as usize];
        let b1 = self.memory[((address + 1) & 0xffff) as usize];
        let b2 = self.memory[((address + 2) & 0xffff) as usize];
        let b3 = self.memory[((address + 3) & 0xffff) as usize];
        (b3 as u32) << 24 | (b2 as u32) << 16 | (b1 as u32) << 8 | b0 as u32
    }

}
impl WatchContext for Wdc6502Machine {

    fn fetch_register(&self, register_id: Operand) -> Operand {
        match register_id {
            REG_A => self.registers.a as Operand,
            REG_X => self.registers.x as Operand,
            REG_Y => self.registers.y as Operand,
            REG_P => self.registers.p as Operand,
            REG_S => self.registers.s as Operand,
            REG_PC => self.registers.pc as Operand,
            _ => panic!("unrecognized register ID: {register_id}")
        }
    }

    fn fetch_register_signed(&self, register_id: Operand) -> Operand {
        match register_id {
            REG_A => self.registers.a.cast_signed() as Operand,
            REG_X => self.registers.x.cast_signed() as Operand,
            REG_Y => self.registers.y.cast_signed() as Operand,
            REG_P => self.registers.p.cast_signed()as Operand,
            REG_S => self.registers.s.cast_signed() as Operand,
            REG_PC => self.registers.pc.cast_signed() as Operand,
            _ => panic!("unrecognized register ID: {register_id}")
        }
    }

    fn fetch_flag(&self, flag_id: Operand) -> Operand {
        ((self.registers.p as Operand).bitand(flag_id) != 0) as Operand
    }

    fn fetch_byte(&self, address: Operand) -> Operand {
        self.fetch_u8(address) as Operand
    }

    fn fetch_byte_signed(&self, address: Operand) -> Operand {
        self.fetch_u8(address).cast_signed() as Operand
    }

    fn fetch_word(&self, address: Operand) -> Operand {
        self.fetch_u16(address) as Operand
    }

    fn fetch_word_signed(&self, address: Operand) -> Operand {
        self.fetch_u16(address).cast_signed() as Operand
    }

    fn fetch_dword(&self, address: Operand) -> Operand {
        self.fetch_u32(address) as Operand
    }

    fn fetch_dword_signed(&self, address: Operand) -> Operand {
        self.fetch_u32(address) as Operand
    }

}

#[cfg(test)]

mod tests {
    use super::*;

    #[test]
    fn fetch_register() {
        let mut machine = Wdc6502Machine::new();
        machine.registers.a = 1;
        assert_eq!(machine.fetch_register(REG_A), 1);
        machine.registers.x = 2;
        assert_eq!(machine.fetch_register(REG_X), 2);
        machine.registers.y = 3;
        assert_eq!(machine.fetch_register(REG_Y), 3);
        machine.registers.p = 4;
        assert_eq!(machine.fetch_register(REG_P), 4);
        machine.registers.s = 5;
        assert_eq!(machine.fetch_register(REG_S), 5);
        machine.registers.pc = 6;
        assert_eq!(machine.fetch_register(REG_PC), 6);
    }

    #[test]
    #[should_panic(expected="unrecognized")]
    fn fetch_register_unrecognized() {
        let machine = Wdc6502Machine::new();
        machine.fetch_register(0xff);
    }

    #[test]
    fn fetch_register_signed() {
        let mut machine = Wdc6502Machine::new();
        machine.registers.a = -1i8 as u8;
        assert_eq!(machine.fetch_register_signed(REG_A), -1i32 as Operand);
        machine.registers.x = -2i8 as u8;
        assert_eq!(machine.fetch_register_signed(REG_X), -2i32 as Operand);
        machine.registers.y = -3i8 as u8;
        assert_eq!(machine.fetch_register_signed(REG_Y), -3i32 as Operand);
        machine.registers.p = -4i8 as u8;
        assert_eq!(machine.fetch_register_signed(REG_P), -4i32 as Operand);
        machine.registers.s = -5i8 as u8;
        assert_eq!(machine.fetch_register_signed(REG_S), -5i32 as Operand);
        machine.registers.pc = -6i16 as u16;
        assert_eq!(machine.fetch_register_signed(REG_PC), -6i32 as Operand);
    }

    #[test]
    fn fetch_flag() {
        let mut machine = Wdc6502Machine::new();
        machine.registers.p = 0;
        assert_eq!(machine.fetch_flag(FLAG_C), 0);
        assert_eq!(machine.fetch_flag(FLAG_Z), 0);
        assert_eq!(machine.fetch_flag(FLAG_I), 0);
        assert_eq!(machine.fetch_flag(FLAG_D), 0);
        assert_eq!(machine.fetch_flag(FLAG_B), 0);
        assert_eq!(machine.fetch_flag(FLAG_V), 0);
        assert_eq!(machine.fetch_flag(FLAG_N), 0);
        machine.registers.p = 0xff;
        assert_eq!(machine.fetch_flag(FLAG_C), 1);
        assert_eq!(machine.fetch_flag(FLAG_Z), 1);
        assert_eq!(machine.fetch_flag(FLAG_I), 1);
        assert_eq!(machine.fetch_flag(FLAG_D), 1);
        assert_eq!(machine.fetch_flag(FLAG_B), 1);
        assert_eq!(machine.fetch_flag(FLAG_V), 1);
        assert_eq!(machine.fetch_flag(FLAG_N), 1);
        machine.registers.p = 0xfe;
        assert_eq!(machine.fetch_flag(FLAG_C), 0);
    }

    #[test]
    fn fetch_byte() {
        let mut machine = Wdc6502Machine::new();
        machine.store_u8(0, 0xaa);
        assert_eq!(machine.fetch_byte(0), 0xaa);
        assert_eq!(machine.fetch_byte_signed(0), 0xffffffaa);
    }

    #[test]
    fn fetch_word() {
        let mut machine = Wdc6502Machine::new();
        machine.store_u8(0, 0x55);
        machine.store_u8(1, 0xaa);
        assert_eq!(machine.fetch_word(0), 0xaa55);
        assert_eq!(machine.fetch_word_signed(0), 0xffffaa55);
        machine.store_u8(0xffff, 0x55);
        assert_eq!(machine.fetch_word(0xffff), 0x5555);
    }

    #[test]
    fn fetch_dword() {
        let mut machine = Wdc6502Machine::new();
        machine.store_u8(0, 0x55);
        machine.store_u8(1, 0xaa);
        machine.store_u8(2, 0x55);
        machine.store_u8(3, 0xaa);
        assert_eq!(machine.fetch_dword(0), 0xaa55aa55);
        assert_eq!(machine.fetch_dword_signed(0), 0xaa55aa55);
        machine.store_u8(0xfffe, 0x55);
        machine.store_u8(0xffff, 0xaa);
        assert_eq!(machine.fetch_dword(0xfffe), 0xaa55aa55);
    }

}
