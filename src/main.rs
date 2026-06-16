use emma65::emulator::{BusConfig, Cpu, CpuVariant, StepResult, map_flag_name, map_register_name};
use emma65::emulator::bus::region::AddressRange;
use emma65::watch::WatchCompiler;

fn main() {
    let bus = BusConfig::new()
        .ram(AddressRange::new(0x0000, 0xFFFF))
        .unwrap()
        .build();

    let mut cpu = Cpu::builder(CpuVariant::Wdc65C02)
        .bus(bus)
        .build()
        .unwrap();

    // Write a small program: LDA #$00 at reset vector target, then NOP loop.
    let start: u16 = 0x0400;
    cpu.bus_mut().write(0xFFFC, (start & 0xFF) as u8).unwrap();
    cpu.bus_mut().write(0xFFFD, (start >> 8) as u8).unwrap();
    cpu.bus_mut().write(start, 0xA9).unwrap();     // LDA #$01
    cpu.bus_mut().write(start + 1, 0x01).unwrap();
    cpu.bus_mut().write(start + 2, 0xEA).unwrap(); // NOP
    cpu.reset().unwrap();

    // Watch for A changing from its previous value via walrus assignment.
    let mut compiler = WatchCompiler::new(map_register_name, map_flag_name, |_| None);
    let wp = compiler
        .compile("(prev_A := A) != prev_A", cpu.evaluator_mut())
        .unwrap();
    cpu.evaluator_mut().add(wp);

    for _ in 0..4 {
        match cpu.step() {
            StepResult::WatchTriggered { watch_index, pc } => {
                println!("triggered: watchpoint {watch_index} at PC=${pc:04X}");
            }
            StepResult::Executed(decoded) => {
                println!("executed: {:?} at PC=${:04X}", decoded.mnemonic, cpu.registers().pc);
            }
            StepResult::Error(e) => {
                println!("error: {e}");
                break;
            }
            _ => break,
        }
    }
}

