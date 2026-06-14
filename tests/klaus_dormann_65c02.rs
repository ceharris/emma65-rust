use emma65::emulator::{
    AddressRange, Bus, ClockSpeed, CpuBuilder, CpuVariant, InvalidOpcodePolicy, StepResult,
};

/// Load address for the flat 64 KB image.
const LOAD_ADDR: u16 = 0x0000;

/// Entry point: first instruction of the test suite.
const START_ADDR: u16 = 0x0400;

/// PC value of the success infinite loop (`JMP $24F1`).
const SUCCESS_PC: u16 = 0x24F1;

/// Maximum steps before declaring the test hung (well above any reasonable run count).
const MAX_STEPS: u64 = 500_000_000;

/// Path to the Klaus Dormann 65C02 extended opcodes functional test ROM.
const ROM_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/roms/65C02_extended_opcodes_test.bin"
);

/// Loads the Klaus Dormann 65C02 functional test ROM, runs it to completion, and
/// asserts that execution reaches the success infinite loop at `SUCCESS_PC`.
///
/// On failure the test traps in a `JMP *` at the address of the failing test, so a
/// non-success PC in the assertion output directly identifies which test failed.
#[test]
fn klaus_dormann_65c02_functional_test() {
    let image = std::fs::read(ROM_PATH)
        .expect("failed to read Klaus Dormann ROM — ensure tests/roms/65C02_extended_opcodes_test.bin is present");

    assert_eq!(
        image.len(),
        65536,
        "ROM image should be exactly 64 KiB (flat image), got {} bytes",
        image.len()
    );

    let bus = Bus::config()
        .ram_with_data(AddressRange::new(LOAD_ADDR, 0xFFFF), image)
        .expect("bus configuration failed")
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .expect("CPU build failed");

    cpu.registers_mut().pc = START_ADDR;

    for _ in 0..MAX_STEPS {
        let pc = cpu.registers().pc;
        if pc == SUCCESS_PC {
            // All tests passed.
            return;
        }

        match cpu.step() {
            StepResult::Executed(_) | StepResult::Waiting => {}
            StepResult::Breakpoint(_) | StepResult::WatchTriggered { .. } | StepResult::WatchError { .. } => {
                unreachable!("no breakpoints or watches configured")
            }
            StepResult::Stopped => {
                panic!("CPU halted (STP) at PC=${pc:04X} before reaching success address ${SUCCESS_PC:04X}")
            }
            StepResult::Error(e) => {
                panic!("CPU error at PC=${pc:04X}: {e} — test suite failed before reaching ${SUCCESS_PC:04X}")
            }
        }
    }

    let pc = cpu.registers().pc;
    panic!(
        "Test did not complete within {MAX_STEPS} steps; stuck at PC=${pc:04X} (success would be ${SUCCESS_PC:04X})"
    );
}
