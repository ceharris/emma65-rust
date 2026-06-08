mod wdc6502;

use emma65::watch::{WatchCompiler, WatchEvaluator};
use wdc6502::Wdc6502Machine;

fn main() {
    let mut compiler = WatchCompiler::new(
        wdc6502::map_register_name,
        wdc6502::map_flag_name,
        |_| None,
    );

    let mut vars = Vec::new();
    match compiler.compile("(prev_A := A) != prev_A", &mut vars) {
        Ok(watchpoint) => {
            let mut machine = Wdc6502Machine::new();
            machine.set_a(0);
            machine.set_x(0);
            machine.set_y(0);
            machine.set_s(0);
            machine.set_pc(0x40c);
            machine.set_p(0x1);
            machine.store_u8(0x2, 4);
            let mut evaluator = WatchEvaluator::new();
            evaluator.add(watchpoint);
            let triggered = evaluator.eval_all(&machine, &mut vars);
            println!("triggered: {triggered}");
        }
        Err(error) => eprintln!("compile error: {error}"),
    }
}