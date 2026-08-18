//! Support for assembling 6502 assembly language source into bytes.
//!
//! No public API yet — the parser/evaluator, instruction table, and driver
//! land in later units (see `doc/assembler-plan.md`), so nothing here has a
//! consumer until then.
#![allow(dead_code)]

mod driver;
mod error;
mod eval;
mod expr;
mod instructions;
mod operand;
mod parser;
mod scanner;
mod statement;
mod token;
