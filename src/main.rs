mod wdc6502;

use emma65::watch::{Operand, Parser};
use emma65::watch::compiler;
use wdc6502::Wdc6502Machine;

fn main() {
    let mut parser = Parser::from(
        wdc6502::map_register_name,
        wdc6502::map_flag_name,
        noop_mapper,
    );
    let mut vars = emma65::watch::Variables::new();
    let prev_a_id = vars.get_or_create("prev_A");
    match parser.parse("(prev_A := A) != prev_A", &mut vars) {
        Ok(Some(expr)) => {
            let mut machine = Wdc6502Machine::new();
            machine.set_a(0);
            machine.set_x(0);
            machine.set_y(0);
            machine.set_s(0);
            machine.set_pc(0x40c);
            machine.set_p(0x1);
            machine.store_u8(0x2, 4);
            let code = compiler::compile(expr);
            let mut var_storage: Vec<Operand> = vec![0; vars.len()];
            var_storage[prev_a_id as usize] = 42;
            let result = emma65::watch::eval(&code, &machine, &mut var_storage);
            println!("result {result}");
        }
        Ok(None) => println!("nothing parsed"),
        Err(error) => eprintln!("parse error: {error:?}"),
    }
}

fn noop_mapper(_name: &str) -> Option<Operand> {
    None
}
