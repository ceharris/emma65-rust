
use super::expr::{Operand};

pub trait Machine {

    fn fetch_register(&self, register_id: Operand) -> Operand;

    fn fetch_register_signed(&self, register_id: Operand) -> Operand;

    fn fetch_flag(&self, flag_id: Operand) -> Operand;

    fn fetch_byte(&self, address: Operand) -> Operand;

    fn fetch_byte_signed(&self, address: Operand) -> Operand;

    fn fetch_word(&self, address: Operand) -> Operand;

    fn fetch_word_signed(&self, address: Operand) -> Operand;

    fn fetch_dword(&self, address: Operand) -> Operand;

    fn fetch_dword_signed(&self, address: Operand) -> Operand;

}
