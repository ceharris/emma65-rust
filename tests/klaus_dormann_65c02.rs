use emma65::emulator::{
    AddressRange, Bus, ClockSpeed, CpuBuilder, CpuVariant, DeviceId, InvalidOpcodePolicy,
    IoDevice, StepResult,
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

/// Single-byte device at `$BFFC` used by the Klaus Dormann interrupt test.
///
/// The test drives the feedback register with an open-collector, active-low protocol:
/// - Bit 0 low  → assert IRQ; bit 0 high → release IRQ
/// - Bit 1 falling edge (1 → 0) → signal one NMI edge
///
/// `irq_active()` and `take_nmi()` are polled by the CPU after each instruction via
/// `Bus::device_irq_states()` and `Bus::take_device_nmi()`.
struct FeedbackRegister {
    /// Current value written by the test program.
    last_value: u8,
    /// Whether the IRQ line is currently asserted (bit 0 low).
    irq_asserted: bool,
    /// Latched NMI edge: set on a falling edge of bit 1, cleared by `take_nmi`.
    nmi_pending: bool,
}

impl FeedbackRegister {
    fn new() -> Self {
        // Both bits start low (inactive); the test's init code precharges them to 0 anyway.
        Self { last_value: 0x00, irq_asserted: false, nmi_pending: false }
    }
}

impl IoDevice for FeedbackRegister {
    fn base_address(&self) -> u16 {
        0xBFFC
    }

    fn write(&mut self, _offset: u16, value: u8) {
        let prev = self.last_value;
        self.last_value = value;
        // Bit 0: active-HIGH IRQ (1 = asserted, 0 = released).
        self.irq_asserted = (value & 0x01) != 0;
        // Bit 1: NMI edge-triggered on rising edge (0 → 1).
        if (prev & 0x02) == 0 && (value & 0x02) != 0 {
            self.nmi_pending = true;
        }
    }

    fn read(&mut self, _offset: u16) -> u8 {
        self.last_value
    }

    fn peek(&self, _offset: u16) -> u8 {
        self.last_value
    }

    fn irq_active(&self) -> bool {
        self.irq_asserted
    }

    fn take_nmi(&mut self) -> bool {
        let pending = self.nmi_pending;
        self.nmi_pending = false;
        pending
    }

    fn name(&self) -> &str {
        "feedback_register"
    }
}

/// Loads the Klaus Dormann interrupt test ROM, runs it to completion, and verifies the success PC.
///
/// The test exercises IRQ and NMI handling, BRK, interrupt masking, flag preservation across
/// interrupts, and CMOS D-flag clearing on interrupt entry. It communicates with the emulator
/// through a `FeedbackRegister` device at `$BFFC`: writing a value with bit 0 low asserts IRQ,
/// and a falling edge on bit 1 signals NMI.
///
/// Assembled from `6502_interrupt_test.a65` with `D_clear = 1` (WDC 65C02 CMOS behavior).
/// Success PC `$0719` is the address of the final `jmp *` after all automated tests pass.
/// The WAI and STP manual tests that follow `$0719` require single-step debugger operation
/// and are not exercised here.
fn run_interrupt_test(rom_path: &str, start: u16, success_pc: u16) {
    let image = std::fs::read(rom_path)
        .unwrap_or_else(|_| panic!("failed to read ROM image — ensure {rom_path} is present"));

    assert_eq!(image.len(), 65536, "ROM image must be exactly 64 KiB");

    let bus = Bus::config()
        .ram_with_data(AddressRange::new(0x0000, 0xBFFB), image[0x0000..=0xBFFB].to_vec())
        .expect("lower RAM")
        .device(AddressRange::new(0xBFFC, 0xBFFC), DeviceId(1), Box::new(FeedbackRegister::new()))
        .expect("feedback register")
        .ram_with_data(AddressRange::new(0xBFFD, 0xFFFF), image[0xBFFD..=0xFFFF].to_vec())
        .expect("upper RAM")
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

/// Runs the Bruce Clark decimal mode test ROM and verifies that `ERROR` (`$000B`) is zero.
///
/// The test exhaustively checks all 256×256 combinations of ADC and SBC operands in decimal
/// mode (both carry states), comparing actual CPU results against predicted CMOS 65C02 values.
/// It terminates by executing a `STP` instruction (`$DB`) at `$024B`. `ERROR = 0` at `$000B`
/// confirms all checks passed.
///
/// Assembled from `6502_decimal_test.a65` with `cputype = 1` (WDC 65C02 CMOS behavior).
fn run_decimal_test(rom_path: &str, start: u16) {
    let image = std::fs::read(rom_path)
        .unwrap_or_else(|_| panic!("failed to read ROM image — ensure {rom_path} is present"));

    assert_eq!(image.len(), 65536, "ROM image must be exactly 64 KiB");

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
        match cpu.step() {
            StepResult::Executed(_) | StepResult::Waiting => {}
            StepResult::Breakpoint(_) | StepResult::WatchTriggered { .. } | StepResult::WatchError { .. } => {
                unreachable!("no breakpoints or watches configured")
            }
            StepResult::Stopped => {
                let error = cpu.bus().peek(0x000B).expect("ERROR byte readable");
                assert_eq!(error, 0, "decimal mode test failed: ERROR=${error:02X} at $000B");
                return;
            }
            StepResult::Error(e) => {
                let pc = cpu.registers().pc;
                panic!("CPU error at PC=${pc:04X}: {e}");
            }
        }
    }

    let pc = cpu.registers().pc;
    panic!("Test did not complete within {MAX_STEPS} steps; stuck at PC=${pc:04X}");
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

/// Tests IRQ and NMI handling, BRK, interrupt masking, and flag preservation across interrupts.
///
/// Assembled with `D_clear = 1` for WDC 65C02 CMOS behavior (D flag cleared on interrupt entry).
/// On failure, the non-success PC identifies the failing interrupt test section.
#[test]
fn interrupt_test() {
    run_interrupt_test(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/roms/6502_interrupt_test.bin"),
        0x0400,
        0x0719,
    );
}

/// Tests all 256×256 ADC and SBC combinations in decimal mode against predicted CMOS 65C02 values.
///
/// Assembled with `cputype = 1` for WDC 65C02 CMOS decimal behavior.
/// Terminates via `STP` at `$024B`; verifies `ERROR = 0` at `$000B`.
#[test]
fn decimal_mode_test() {
    run_decimal_test(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/roms/6502_decimal_test.bin"),
        0x0200,
    );
}
