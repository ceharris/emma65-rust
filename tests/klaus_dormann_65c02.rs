use emma65::emulator::{
    AddressRange, Bus, ClockSpeed, CpuBuilder, CpuVariant, InvalidOpcodePolicy, StepResult,
};

/// Maximum steps before declaring a test hung (well above any reasonable run count).
const MAX_STEPS: u64 = 500_000_000;

/// Loads a flat 64 KB ROM image into a `Wdc65C02` CPU with full 64 KB writable RAM,
/// sets PC to `start`, and steps until `success_pc` is reached.
///
/// On failure the Klaus Dormann tests trap in a `JMP *` at the failing test's address,
/// so a non-success PC in the panic message directly identifies which test failed.
fn run_functional_test(rom_path: &str, start: u16, success_pc: u16) {
    let image = std::fs::read(rom_path)
        .unwrap_or_else(|_| panic!("failed to read ROM image — ensure {rom_path} is present"));

    assert_eq!(
        image.len(),
        65536,
        "ROM image should be exactly 64 KiB (flat image), got {} bytes",
        image.len()
    );

    let bus = Bus::config()
        .ram_with_data(AddressRange::new(0x0000, 0xFFFF), image)
        .expect("bus configuration failed")
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .expect("CPU build failed");

    cpu.registers_mut().pc = start;

    for _ in 0..MAX_STEPS {
        let pc = cpu.registers().pc;
        if pc == success_pc {
            return;
        }

        match cpu.step() {
            StepResult::Executed(_) | StepResult::Waiting => {}
            StepResult::Breakpoint(_) | StepResult::WatchTriggered { .. } | StepResult::WatchError { .. } => {
                unreachable!("no breakpoints or watches configured")
            }
            StepResult::Stopped => {
                panic!("CPU halted (STP) at PC=${pc:04X} before reaching success address ${success_pc:04X}")
            }
            StepResult::Error(e) => {
                panic!("CPU error at PC=${pc:04X}: {e} — test failed before reaching ${success_pc:04X}")
            }
        }
    }

    let pc = cpu.registers().pc;
    panic!(
        "Test did not complete within {MAX_STEPS} steps; stuck at PC=${pc:04X} (success would be ${success_pc:04X})"
    );
}

/// Tests the full 6502 base instruction set and all addressing modes.
///
/// On failure, the non-success PC value identifies the failing test group.
#[test]
fn base_6502_functional_test() {
    run_functional_test(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/roms/6502_functional_test.bin"),
        0x0400,
        0x3469,
    );
}

/// Tests all 65C02 extended opcodes: new addressing modes, `STZ`, `BRA`, `PHX`/`PHY`/`PLX`/`PLY`,
/// `TRB`/`TSB`, `BIT` immediate, `JMP (abs,X)`, WDC-only instructions, and defined NOPs.
///
/// On failure, the non-success PC value identifies the failing test group.
#[test]
fn extended_65c02_functional_test() {
    run_functional_test(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/roms/65C02_extended_opcodes_test.bin"),
        0x0400,
        0x24F1,
    );
}
