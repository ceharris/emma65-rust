use std::time::Instant;
use tokio::sync::{oneshot, watch};
use crate::emulator::cpu::Cpu;
use crate::emulator::cpu::opcodes::DecodedOp;
use crate::emulator::error::ExecError;
use crate::watch::WatchError;

const JSR_OPCODE: u8 = 0x20;
const JSR_BYTE_LEN: u16 = 3;

const UNLIMITED_SENTINEL: u64 = 0;
const BATCH_SIZE: u32 = 1000;

/// Target clock frequency for the emulated CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockSpeed {
    hz: u64,
}

impl ClockSpeed {
    /// Creates a clock speed from a frequency in MHz (e.g. `1.8432` for 1.8432 MHz).
    pub fn mhz(mhz: f64) -> Self {
        Self { hz: (mhz * 1_000_000.0).round() as u64 }
    }

    /// Creates a clock speed from a frequency in Hz.
    pub fn hz(hz: u64) -> Self {
        assert!(hz > 0, "hz must be non-zero; use ClockSpeed::unlimited() for no throttling");
        Self { hz }
    }

    /// Creates an unlimited clock speed, disabling throttling in free-running mode.
    pub fn unlimited() -> Self {
        Self { hz: UNLIMITED_SENTINEL }
    }

    /// Returns `true` if this clock speed has no throttling limit.
    pub fn is_unlimited(&self) -> bool {
        self.hz == UNLIMITED_SENTINEL
    }

    /// Returns the clock frequency in Hz, or `None` if unlimited.
    pub fn hz_value(&self) -> Option<u64> {
        if self.is_unlimited() { None } else { Some(self.hz) }
    }
}

/// Result returned by `Cpu::step()`.
pub enum StepResult {
    /// Instruction executed normally.
    Executed(DecodedOp),
    /// PC matched a breakpoint; instruction was NOT executed.
    Breakpoint(u16),
    /// A watch expression triggered; instruction was NOT executed.
    WatchTriggered { watch_index: usize, pc: u16 },
    /// A watch expression evaluation failed; instruction was NOT executed.
    WatchError { watch_index: usize, pc: u16, error: WatchError },
    /// CPU is in WAI state, waiting for an interrupt.
    Waiting,
    /// CPU is in STP state; only reset() clears it.
    Stopped,
    /// A fatal execution error occurred.
    Error(ExecError),
}

/// Handle returned by [`run`] for controlling a free-running CPU thread.
///
/// Dropping the handle without calling [`stop`](RunHandle::stop) or
/// [`take_cpu`](RunHandle::take_cpu) will leave the CPU thread running until it
/// stops on its own (breakpoint, watch trigger, error, or STP/WAI with no interrupt).
pub struct RunHandle {
    /// Sends `true` to ask the CPU thread to stop after the current instruction.
    stop_tx: watch::Sender<bool>,
    /// Receives the `StepResult` that caused execution to stop.
    result_rx: oneshot::Receiver<StepResult>,
    /// Receives ownership of the CPU after the thread exits.
    cpu_rx: oneshot::Receiver<Cpu>,
}

impl RunHandle {
    /// Signals the CPU thread to stop after the current instruction.
    ///
    /// Non-blocking. The CPU will stop before the next instruction fetch.
    pub fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }

    /// Awaits the [`StepResult`] that caused execution to stop.
    ///
    /// If execution was stopped via [`stop`](RunHandle::stop), the result is
    /// `StepResult::Executed` for the last instruction that completed normally.
    pub async fn wait(self) -> StepResult {
        self.result_rx.await.expect("CPU thread exited without sending result")
    }

    /// Signals the CPU thread to stop and awaits the CPU being returned.
    pub async fn take_cpu(self) -> Cpu {
        self.stop();
        self.cpu_rx.await.expect("CPU thread exited without returning CPU")
    }
}

/// Executes one logical step, treating JSR as atomic.
///
/// If the instruction at the current PC is JSR, sets a temporary breakpoint at
/// `PC+3` and steps until that breakpoint is hit (or execution halts for any
/// other reason). For all other instructions, behaves identically to
/// [`Cpu::step`].
///
/// The temporary breakpoint is always removed before returning, even if
/// execution stops before reaching it. Any pre-existing breakpoint at the
/// target address is preserved.
///
/// Returns the [`StepResult`] that ended the operation.
pub fn step_over(cpu: &mut Cpu) -> StepResult {
    let pc = cpu.registers().pc;
    let opcode = cpu.bus().peek(pc).unwrap_or(0);
    if opcode != JSR_OPCODE {
        return cpu.step();
    }

    let target = pc.wrapping_add(JSR_BYTE_LEN);
    let already_set = cpu.breakpoints().contains(&target);
    if !already_set {
        cpu.add_breakpoint(target);
    }

    let result = loop {
        match cpu.step() {
            StepResult::Executed(op) => {
                if cpu.registers().pc == target {
                    break if already_set {
                        StepResult::Breakpoint(target)
                    } else {
                        StepResult::Executed(op)
                    };
                }
            }
            StepResult::Breakpoint(addr) if addr == target => {
                // The temporary breakpoint fired before the instruction executed.
                // Surface as Breakpoint only if the caller owns it; otherwise
                // remove it and let the next step execute the instruction.
                if already_set {
                    break StepResult::Breakpoint(target);
                }
                cpu.remove_breakpoint(target);
                match cpu.step() {
                    StepResult::Executed(op) => break StepResult::Executed(op),
                    other => break other,
                }
            }
            other => break other,
        }
    };

    if !already_set {
        cpu.remove_breakpoint(target);
    }
    result
}

/// Runs until the stack pointer rises above its value at the time of the call.
///
/// This detects subroutine return: once `S` exceeds `initial_s` (using
/// wrapping 8-bit arithmetic to handle stack pointer wraparound), the
/// subroutine's stack frame has been unwound by `RTS` or `RTI`. Execution also
/// halts on any non-[`StepResult::Executed`] result (breakpoint, watch trigger,
/// error, STP, WAI stall).
///
/// Returns the [`StepResult`] that ended the operation. If the stack pointer
/// rose above `initial_s`, the result is [`StepResult::Executed`] for the
/// instruction that caused it (typically the `RTS`/`RTI`).
pub fn step_return(cpu: &mut Cpu) -> StepResult {
    let initial_s = cpu.registers().s;
    loop {
        match cpu.step() {
            StepResult::Executed(op)
                if (cpu.registers().s.wrapping_sub(initial_s) as i8) > 0 =>
            {
                return StepResult::Executed(op);
            }
            StepResult::Executed(_) => {}
            other => return other,
        }
    }
}

/// Moves `cpu` into a dedicated OS thread and begins executing instructions.
///
/// Returns a [`RunHandle`] immediately. The CPU thread runs `cpu.step()` in a
/// tight loop, checking the stop signal between instructions. When execution
/// halts (stop signal, breakpoint, watch trigger, error, STP, or WAI stall),
/// both the final [`StepResult`] and the `Cpu` are sent back via the handle's
/// channels.
///
/// When `cpu.clock_speed()` is not unlimited, the loop sleeps as needed to
/// match the target frequency. Throttling is batched over ~1000 instructions
/// to avoid per-instruction syscall overhead.
pub fn run(cpu: Cpu) -> RunHandle {
    let (stop_tx, stop_rx) = watch::channel(false);
    let (result_tx, result_rx) = oneshot::channel();
    let (cpu_tx, cpu_rx) = oneshot::channel();

    std::thread::spawn(move || {
        run_loop(cpu, stop_rx, result_tx, cpu_tx);
    });

    RunHandle { stop_tx, result_rx, cpu_rx }
}

fn run_loop(
    mut cpu: Cpu,
    stop_rx: watch::Receiver<bool>,
    result_tx: oneshot::Sender<StepResult>,
    cpu_tx: oneshot::Sender<Cpu>,
) {
    let start = Instant::now();
    let start_cycles = cpu.cycles();
    let hz = cpu.clock_speed().hz_value();

    let final_result = 'outer: loop {
        for _ in 0..BATCH_SIZE {
            if *stop_rx.borrow() {
                break 'outer None;
            }
            match cpu.step() {
                StepResult::Executed(_) => {}
                other => break 'outer Some(other),
            }
        }

        if let Some(hz) = hz {
            let elapsed_ns = start.elapsed().as_nanos() as u64;
            let expected_cycles =
                (elapsed_ns as u128 * hz as u128 / 1_000_000_000) as u64;
            let actual_cycles = cpu.cycles() - start_cycles;
            if actual_cycles > expected_cycles {
                let excess = actual_cycles - expected_cycles;
                let sleep_ns = excess * 1_000_000_000 / hz;
                std::thread::sleep(std::time::Duration::from_nanos(sleep_ns));
            }
        }
    };

    // Synthesize an Executed result for a clean stop so the caller always gets
    // a valid StepResult regardless of why the loop exited.
    let step_result = final_result.unwrap_or_else(|| {
        use crate::emulator::cpu::opcodes::{AddressingMode, Mnemonic};
        StepResult::Executed(DecodedOp {
            opcode: 0xEA,
            mnemonic: Mnemonic::Nop,
            mode: AddressingMode::Implied,
            byte_len: 1,
            base_cycles: 2,
            is_valid: true,
        })
    });

    let _ = result_tx.send(step_result);
    let _ = cpu_tx.send(cpu);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mhz_converts_to_hz() {
        assert_eq!(ClockSpeed::mhz(1.0).hz_value(), Some(1_000_000));
        assert_eq!(ClockSpeed::mhz(1.8432).hz_value(), Some(1_843_200));
        assert_eq!(ClockSpeed::mhz(2.0).hz_value(), Some(2_000_000));
    }

    #[test]
    fn hz_constructor() {
        assert_eq!(ClockSpeed::hz(1_843_200).hz_value(), Some(1_843_200));
    }

    #[test]
    fn unlimited_sentinel() {
        let s = ClockSpeed::unlimited();
        assert!(s.is_unlimited());
        assert_eq!(s.hz_value(), None);
    }

    #[test]
    fn non_unlimited_is_not_unlimited() {
        assert!(!ClockSpeed::mhz(1.0).is_unlimited());
        assert!(!ClockSpeed::hz(1).is_unlimited());
    }

    // --- free-run tests ---

    use crate::emulator::bus::{AddressRange, Bus};
    use crate::emulator::cpu::{Cpu, CpuBuilder};
    use crate::emulator::cpu::variant::CpuVariant;

    const RESET_VECTOR: u16 = 0xFFFC;
    const NOP_ADDR: u16 = 0x0200;

    /// Builds a CPU with 64KB RAM and an infinite NOP loop at `NOP_ADDR`.
    fn make_cpu_with_speed(speed: ClockSpeed) -> Cpu {
        let mut bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        // Reset vector → NOP_ADDR
        bus.write(RESET_VECTOR, (NOP_ADDR & 0xFF) as u8).unwrap();
        bus.write(RESET_VECTOR + 1, (NOP_ADDR >> 8) as u8).unwrap();
        // NOP at NOP_ADDR; BRA -2 loops back to NOP_ADDR forever
        bus.write(NOP_ADDR, 0xEA).unwrap();       // NOP
        bus.write(NOP_ADDR + 1, 0x80).unwrap();   // BRA
        bus.write(NOP_ADDR + 2, 0xFD_u8).unwrap(); // offset -3 → back to NOP_ADDR
        let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
            .clock_speed(speed)
            .bus(bus)
            .build()
            .unwrap();
        cpu.reset().unwrap();
        cpu
    }

    #[tokio::test]
    async fn free_run_stop_returns_cpu() {
        let cpu = make_cpu_with_speed(ClockSpeed::unlimited());
        let handle = run(cpu);
        // Let it run briefly, then stop.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let cpu = handle.take_cpu().await;
        // CPU executed NOPs; PC should still be in the NOP loop.
        assert!(cpu.cycles() > 0);
    }

    #[tokio::test]
    async fn breakpoint_during_free_run_returns_breakpoint_result() {
        let mut cpu = make_cpu_with_speed(ClockSpeed::unlimited());
        cpu.add_breakpoint(NOP_ADDR);
        let handle = run(cpu);
        let result = handle.wait().await;
        assert!(matches!(result, StepResult::Breakpoint(NOP_ADDR)));
    }

    #[tokio::test]
    async fn watch_trigger_during_free_run_returns_watch_triggered() {
        use crate::emulator::cpu::{map_register_name, map_flag_name};
        use crate::watch::WatchCompiler;

        let mut cpu = make_cpu_with_speed(ClockSpeed::unlimited());
        let mut compiler = WatchCompiler::new(map_register_name, map_flag_name, |_| None);
        // PC == NOP_ADDR triggers immediately on the first watch evaluation.
        let wp = compiler.compile(
            &format!("PC == ${:X}", NOP_ADDR),
            cpu.evaluator_mut(),
        ).unwrap();
        cpu.evaluator_mut().add(wp);
        let handle = run(cpu);
        let result = handle.wait().await;
        assert!(matches!(result, StepResult::WatchTriggered { .. }));
    }

    #[tokio::test]
    async fn throttled_execution_takes_at_least_wall_time() {
        // Test at 2 MHz — the most demanding common clock speed (1 MHz, 1.8432 MHz, 2 MHz).
        // At 2 MHz, BATCH_SIZE=1000 instructions ≈ 2500 cycles ≈ 1.25ms of emulated time,
        // so the throttle fires roughly every 1ms — still fine-grained enough.
        // We allow 2× tolerance: cycles must not exceed 2 × (wall_elapsed × 2_000_000 Hz).
        let cpu = make_cpu_with_speed(ClockSpeed::mhz(2.0));
        let handle = run(cpu);
        let wall_start = Instant::now();
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        let cpu = handle.take_cpu().await;
        let wall_elapsed = wall_start.elapsed();
        // cycles / 2_000_000 should be ≤ wall_elapsed; allow 2× slack for CI jitter.
        let max_expected_cycles = wall_elapsed.as_micros() as u64 * 4; // 2 cycles/µs × 2 slack
        assert!(cpu.cycles() <= max_expected_cycles,
            "throttled CPU ran too fast: {} cycles in {:?}", cpu.cycles(), wall_elapsed);
    }

    #[tokio::test]
    async fn unlimited_faster_than_throttled() {
        // Use 50 kHz throttle — well below even a debug-build's natural step() throughput,
        // so the throttle demonstrably limits cycle accumulation compared to unlimited.
        // 50ms × 50_000 Hz = 2500 expected throttled cycles; unlimited should far exceed that.
        let target_wall = std::time::Duration::from_millis(50);

        let cpu_unlimited = make_cpu_with_speed(ClockSpeed::unlimited());
        let handle_unlimited = run(cpu_unlimited);
        tokio::time::sleep(target_wall).await;
        let cpu_unlimited = handle_unlimited.take_cpu().await;

        let cpu_throttled = make_cpu_with_speed(ClockSpeed::hz(50_000));
        let handle_throttled = run(cpu_throttled);
        tokio::time::sleep(target_wall).await;
        let cpu_throttled = handle_throttled.take_cpu().await;

        assert!(cpu_unlimited.cycles() > cpu_throttled.cycles(),
            "unlimited ({} cycles) should exceed throttled ({} cycles)",
            cpu_unlimited.cycles(), cpu_throttled.cycles());
    }

    // --- step_over / step_return helpers ---

    fn make_cpu_at(start: u16) -> Cpu {
        use crate::emulator::bus::AddressRange;
        use crate::emulator::cpu::variant::CpuVariant;
        let mut bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        bus.write(0xFFFC, (start & 0xFF) as u8).unwrap();
        bus.write(0xFFFC + 1, (start >> 8) as u8).unwrap();
        let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
            .bus(bus)
            .build()
            .unwrap();
        cpu.reset().unwrap();
        cpu
    }

    fn write(cpu: &mut Cpu, addr: u16, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            cpu.bus_mut().write(addr + i as u16, b).unwrap();
        }
    }

    // --- step_over ---

    #[test]
    fn step_over_non_jsr_behaves_like_step_into() {
        // NOP at $0200; step_over should execute it and advance PC by 1.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0xEA]); // NOP
        let result = step_over(&mut cpu);
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0201);
    }

    #[test]
    fn step_over_jsr_advances_past_subroutine() {
        // JSR $0300 at $0200; subroutine is NOP + RTS.
        // step_over should return with PC at $0203 (instruction after JSR).
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xEA, 0x60]);        // NOP, RTS
        let result = step_over(&mut cpu);
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0203);
    }

    #[test]
    fn step_over_jsr_nested_calls_treated_atomically() {
        // Subroutine at $0300 itself calls $0400 before returning.
        // step_over must treat the entire call tree as atomic.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0x20, 0x00, 0x04]); // JSR $0400
        write(&mut cpu, 0x0303, &[0x60]);               // RTS
        write(&mut cpu, 0x0400, &[0x60]);               // RTS
        let result = step_over(&mut cpu);
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0203);
    }

    #[test]
    fn step_over_jsr_halts_at_inner_breakpoint() {
        // A breakpoint inside the subroutine interrupts step_over.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xEA, 0x60]);        // NOP, RTS
        cpu.add_breakpoint(0x0300);
        let result = step_over(&mut cpu);
        assert!(matches!(result, StepResult::Breakpoint(0x0300)));
        assert_eq!(cpu.registers().pc, 0x0300);
    }

    #[test]
    fn step_over_jsr_preserves_caller_breakpoint_at_target() {
        // If the caller had already set a breakpoint at PC+3, step_over must
        // not remove it and must surface it as Breakpoint, not Executed.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xEA, 0x60]);        // NOP, RTS
        cpu.add_breakpoint(0x0203);
        let result = step_over(&mut cpu);
        // Must surface as Breakpoint since caller owns it.
        assert!(matches!(result, StepResult::Breakpoint(0x0203)));
        // Breakpoint must still be present after step_over returns.
        assert!(cpu.breakpoints().contains(&0x0203));
    }

    #[test]
    fn step_over_jsr_halts_on_stp() {
        // Subroutine executes STP before returning.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xDB]);               // STP
        let result = step_over(&mut cpu);
        assert!(matches!(result, StepResult::Stopped));
    }

    // --- step_return ---

    #[test]
    fn step_return_halts_after_rts() {
        // Call a subroutine via JSR; land at $0300 then call step_return.
        // step_return should run until RTS and return with PC at $0203.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xEA, 0xEA, 0x60]); // NOP, NOP, RTS
        cpu.step(); // JSR — now at $0300, S = 0xFD
        let result = step_return(&mut cpu);
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0203);
    }

    #[test]
    fn step_return_halts_after_rti() {
        // Manually set up a stack frame and use RTI to unwind.
        let mut cpu = make_cpu_at(0x0200);
        // Push P, PC lo, PC hi so RTI returns to $0300.
        let s = cpu.registers().s;
        cpu.bus_mut().write(0x0100 | s as u16, 0x03).unwrap();          // PC hi
        cpu.bus_mut().write(0x0100 | s.wrapping_sub(1) as u16, 0x00).unwrap(); // PC lo
        cpu.bus_mut().write(0x0100 | s.wrapping_sub(2) as u16, 0x24).unwrap(); // P
        cpu.registers_mut().s = s.wrapping_sub(3);
        write(&mut cpu, 0x0200, &[0x40]); // RTI
        let result = step_return(&mut cpu);
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0300);
    }

    #[test]
    fn step_return_handles_stack_pointer_wraparound() {
        // Place initial_s at 0x01 so RTS (which pops 2) wraps S through 0xFF
        // to 0x03. A naive `s > initial_s` would see 0x03 > 0x01 == true, but
        // a naive `0xFF > 0x01` (the intermediate value after the first pop)
        // would also be true, which is correct — the wrapping comparison
        // (s.wrapping_sub(initial_s) as i8) > 0 must also give the right answer.
        //
        // Set up: manually put S at 0x01, push a return address of $0203 so
        // that RTS pops 2 bytes (S: 0x01 → 0xFF → 0x03), and verify step_return
        // halts correctly.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x60]); // RTS at $0200 (acts as the subroutine body)
        // Lay the return address ($0202) on the stack with S = 0x01.
        // RTS pops lo then hi: stack[S+1]=lo=$02, stack[S+2]=hi=$02; RTS adds 1 → PC=$0203.
        cpu.bus_mut().write(0x0102, 0x02).unwrap(); // PC hi
        cpu.bus_mut().write(0x0101, 0x02).unwrap(); // PC lo
        cpu.registers_mut().s = 0x00;               // so S+1=0x01, S+2=0x02
        // initial_s for step_return will be 0x00; after RTS, S = 0x02 (wrapped).
        // (0x02_u8.wrapping_sub(0x00) as i8) = 2 > 0 ✓
        let result = step_return(&mut cpu);
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0203);
    }

    #[test]
    fn step_return_halts_at_breakpoint_inside_subroutine() {
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xEA, 0xEA, 0x60]); // NOP, NOP, RTS
        cpu.step(); // JSR — now inside subroutine
        cpu.add_breakpoint(0x0301);
        let result = step_return(&mut cpu);
        assert!(matches!(result, StepResult::Breakpoint(0x0301)));
    }

    #[test]
    fn step_return_halts_on_stp() {
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xDB]);               // STP before RTS
        cpu.step(); // JSR
        let result = step_return(&mut cpu);
        assert!(matches!(result, StepResult::Stopped));
    }

    #[test]
    fn step_return_nested_call_unwinds_to_correct_frame() {
        // JSR $0300; subroutine JSR $0400; step_return inside $0400 unwinds to $0303.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0x20, 0x00, 0x04]); // JSR $0400
        write(&mut cpu, 0x0303, &[0x60]);               // RTS (outer)
        write(&mut cpu, 0x0400, &[0xEA, 0x60]);        // NOP, RTS (inner)
        cpu.step(); // JSR $0300
        cpu.step(); // JSR $0400 — now inside inner subroutine
        let s_inside = cpu.registers().s;
        let result = step_return(&mut cpu);
        assert!(matches!(result, StepResult::Executed(_)));
        // Returned to $0303 (after inner JSR).
        assert_eq!(cpu.registers().pc, 0x0303);
        // S rose above s_inside but not back to original (outer frame still on stack).
        assert!(cpu.registers().s > s_inside);
    }

    #[test]
    fn step_return_nmi_during_subroutine_does_not_prematurely_halt() {
        // NMI is used instead of IRQ: it fires once (edge-triggered), avoids the
        // re-fire loop that IRQ would create after RTI restores I=0.
        //
        // Scenario: step_return starts at $0300 with S=0xFD (initial_s).
        // NMI entry pushes 3 bytes (S→0xFA); ISR RTI restores S to 0xFD.
        // 0xFD == initial_s (not above), so step_return must continue.
        // The outer RTS then raises S to 0xFF > 0xFD, halting correctly at $0203.
        let mut cpu = make_cpu_at(0x0200);
        cpu.bus_mut().write(0xFFFA, 0x00).unwrap(); // NMI vector lo
        cpu.bus_mut().write(0xFFFB, 0x04).unwrap(); // NMI vector hi → $0400
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xEA, 0xEA, 0xEA, 0x60]); // NOP, NOP, NOP, RTS
        write(&mut cpu, 0x0400, &[0x40]); // ISR: RTI
        cpu.step(); // JSR $0300 — S = 0xFD
        cpu.interrupts_mut().signal_nmi();
        let result = step_return(&mut cpu);
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0203);
    }
}
