//! Assembles a small program with `emma65::assembler::assemble`, loads the
//! resulting bytes into a real `Cpu`/`Bus`, and runs it to completion.
//!
//! The e2e tests in `src/assembler/mod.rs` verify that assembled bytes
//! round-trip correctly through `Disassembler` — a strong check, since the
//! disassembler's decode table is inverted from the same
//! `emulator::cpu::opcodes::decode_table` the assembler encodes against, but
//! it only confirms the bytes *decode back* to the intended instructions.
//! This file covers the one thing that doesn't: that the assembled bytes
//! actually *behave* correctly when a real CPU executes them.

use emma65::assembler::{assemble, AssembledProgram};
use emma65::emulator::cpu::StepResult;
use emma65::emulator::{AddressRange, Bus, ClockSpeed, CpuBuilder, CpuVariant, InvalidOpcodePolicy};

const MAX_STEPS: u32 = 10_000;

/// Assembles `source` and writes every resulting segment into a 64 KB RAM
/// bus. Panics if assembly fails. Does not set any vectors or reset —
/// callers that need the IRQ/NMI vectors populated before reset (e.g. to
/// point at a handler by symbol address) should do so before calling
/// [`reset_at`].
fn load_program(source: &str) -> (emma65::emulator::Cpu, AssembledProgram) {
    let program = assemble(source).unwrap_or_else(|errors| {
        panic!("failed to assemble test program: {errors:?}\nsource:\n{source}");
    });
    let bus = Bus::config().ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0).unwrap().build();
    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();
    for segment in &program.segments {
        for (i, &b) in segment.bytes.iter().enumerate() {
            cpu.bus_mut().write(segment.origin.wrapping_add(i as u16), b).unwrap();
        }
    }
    (cpu, program)
}

/// Points the reset vector at `addr` and resets.
fn reset_at(cpu: &mut emma65::emulator::Cpu, addr: u16) {
    cpu.bus_mut().write(0xFFFC, (addr & 0xFF) as u8).unwrap();
    cpu.bus_mut().write(0xFFFD, (addr >> 8) as u8).unwrap();
    cpu.reset().unwrap();
}

/// Assembles `source`, loads it, and resets at the first segment's origin —
/// the common case for tests that don't also need the IRQ/NMI vectors set.
fn build_cpu_from_source(source: &str) -> emma65::emulator::Cpu {
    let (mut cpu, program) = load_program(source);
    let start = program.segments[0].origin;
    reset_at(&mut cpu, start);
    cpu
}

/// Steps `cpu` until `StepResult::Stopped`, panicking on any error or if the
/// step budget is exhausted (i.e. the program never reached `STP`).
fn step_to_stop(cpu: &mut emma65::emulator::Cpu) {
    for _ in 0..MAX_STEPS {
        match cpu.step(None, true) {
            StepResult::Stopped => return,
            StepResult::Executed(_) | StepResult::Waiting => {}
            StepResult::Error(e) => panic!("CPU error: {e}"),
            StepResult::Reset
            | StepResult::Breakpoint(_)
            | StepResult::WatchTriggered { .. }
            | StepResult::WatchError { .. } => unreachable!(),
        }
    }
    panic!("program did not reach STP within {MAX_STEPS} steps");
}

/// Assembles a program that sums a 4-byte table via an indexed-addressing
/// loop, stores the result to a zero-page symbol, calls a subroutine, and
/// halts — exercising forward references (`len`, `result`, `bump_y`),
/// symbol arithmetic-free indexed addressing (`data,X` resolving to
/// `AbsoluteX` since `data` lands above `$00FF`), a backward branch
/// (`BNE sum_loop`), and a `JSR`/`RTS` pair — then actually runs it and
/// checks the resulting register and memory state.
#[test]
fn assembled_loop_and_subroutine_program_executes_correctly() {
    let source = "\
.setcpu \"wdc65c02\"
.org $0200
start:
  LDX #$00
  LDA #$00
sum_loop:
  CLC
  ADC data,X
  INX
  CPX #len
  BNE sum_loop
  STA result
  JSR bump_y
  STP
bump_y:
  INY
  RTS
data:
  .byte 10, 20, 30, 40
len = 4
result = $10
";
    let mut cpu = build_cpu_from_source(source);
    step_to_stop(&mut cpu);

    assert_eq!(cpu.registers().a, 100, "A should hold the summed total (10+20+30+40)");
    assert_eq!(cpu.registers().x, 4, "X should have counted through all 4 table entries");
    assert_eq!(cpu.registers().y, 1, "Y should have been incremented once by the subroutine");
    assert_eq!(cpu.bus().peek(0x0010).unwrap(), 100, "result byte should hold the summed total");
}

/// Regression test for the `BRK` byte-length bug caught by the addressing-mode
/// sweep test in `src/assembler/mod.rs`: `BRK` is a 2-byte instruction (opcode
/// plus a padding/signature byte the CPU skips over without reading — see
/// `Cpu`'s `Mnemonic::Brk` handling), but the assembler was previously only
/// reserving 1 byte for it, which would silently misalign every statement
/// that followed.
///
/// This test would have caught that bug at the *execution* level, not just
/// the byte-length level: it assembles `BRK` immediately followed by a
/// distinctive `LDA #$99` / `STP`, sets the IRQ vector at a handler that
/// just does `RTI`, and runs from reset. On real 6502-family hardware,
/// `BRK` always advances the PC by 2 during execution regardless of what
/// the assembler thought the instruction length was, and `RTI` resumes
/// there. If the assembler had under-reserved a byte for `BRK`, `LDA #$99`
/// would have been assembled one byte too early, so the CPU would resume
/// execution mid-instruction after the `RTI` and never load `$99` into `A`.
#[test]
fn assembled_brk_reserves_its_full_two_bytes_so_the_next_instruction_survives_the_round_trip() {
    let source = "\
.setcpu \"wdc65c02\"
.org $0200
start:
  BRK
  LDA #$99
  STP
irq_handler:
  RTI
";
    let (mut cpu, program) = load_program(source);
    let irq_handler = program.symbols.address_for("irq_handler").expect("irq_handler symbol should be defined");
    cpu.bus_mut().write(0xFFFE, (irq_handler & 0xFF) as u8).unwrap();
    cpu.bus_mut().write(0xFFFF, (irq_handler >> 8) as u8).unwrap();
    let start = program.segments[0].origin;
    reset_at(&mut cpu, start);

    step_to_stop(&mut cpu);

    assert_eq!(cpu.registers().a, 0x99, "LDA #$99 should have executed intact after the BRK/RTI round trip");
}
