mod wdc6502;

use emma65::watch::WatchSession;
use wdc6502::Wdc6502Machine;

fn main() {
    let mut session = WatchSession::new(
        wdc6502::map_register_name,
        wdc6502::map_flag_name,
        |_| None,
    );

    match session.compile("(prev_A := A) != prev_A") {
        Ok(watchpoint) => {
            let mut machine = Wdc6502Machine::new();
            machine.set_a(0);
            machine.set_x(0);
            machine.set_y(0);
            machine.set_s(0);
            machine.set_pc(0x40c);
            machine.set_p(0x1);
            machine.store_u8(0x2, 4);
            let result = session.eval(&watchpoint, &machine);
            println!("result {result}");
        }
        Err(error) => eprintln!("compile error: {error}"),
    }
}
