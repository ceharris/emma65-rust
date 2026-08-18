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

use emma65::assembler::assemble;
use emma65::emulator::cpu::StepResult;
use emma65::emulator::{AddressRange, Bus, ClockSpeed, CpuBuilder, CpuVariant, InvalidOpcodePolicy};

const MAX_STEPS: u32 = 10_000;

/// Assembles `source`, writes every resulting segment into a 64 KB RAM bus
/// at its `.org` address, points the reset vector at the first segment's
/// origin, and resets. Panics if assembly fails.
fn build_cpu_from_source(source: &str) -> emma65::emulator::Cpu {
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
    let start = program.segments[0].origin;
    cpu.bus_mut().write(0xFFFC, (start & 0xFF) as u8).unwrap();
    cpu.bus_mut().write(0xFFFD, (start >> 8) as u8).unwrap();
    cpu.reset().unwrap();
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
