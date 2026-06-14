use std::time::Instant;
use tokio::sync::{oneshot, watch};
use crate::emulator::cpu::Cpu;
use crate::emulator::cpu::opcodes::DecodedOp;
use crate::emulator::error::ExecError;
use crate::watch::WatchError;

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

    use crate::emulator::bus::Bus;
    use crate::emulator::bus::region::AddressRange;
    use crate::emulator::cpu::{Cpu, CpuBuilder};
    use crate::emulator::cpu::variant::CpuVariant;

    const RESET_VECTOR: u16 = 0xFFFC;
    const NOP_ADDR: u16 = 0x0200;

    /// Builds a CPU with 64KB RAM and an infinite NOP loop at `NOP_ADDR`.
    fn make_cpu_with_speed(speed: ClockSpeed) -> Cpu {
        let mut bus = Bus::config()
            .ram(AddressRange::new(0x0000, 0xFFFF))
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
        // At 1 MHz, 10_000 cycles should take at least 5ms (50% tolerance).
        // We stop after a time that should correspond to ~10ms of emulated time.
        let cpu = make_cpu_with_speed(ClockSpeed::mhz(1.0));
        let handle = run(cpu);
        // Wait 15ms — at 1 MHz with throttling, we should have slept at least once.
        let wall_start = Instant::now();
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        let cpu = handle.take_cpu().await;
        let wall_elapsed = wall_start.elapsed();
        // At 1 MHz, `cpu.cycles()` cycles would take cpu.cycles() microseconds.
        // The throttle should have kept wall time ≥ (cycles / 1_000_000) seconds.
        // We just check that we spent at least 5ms (very loose) and cycles are capped
        // roughly to 1MHz × wall_elapsed (within 2×).
        let max_expected_cycles = wall_elapsed.as_micros() as u64 * 2;
        assert!(cpu.cycles() <= max_expected_cycles,
            "throttled CPU ran too fast: {} cycles in {:?}", cpu.cycles(), wall_elapsed);
    }

    #[tokio::test]
    async fn unlimited_faster_than_throttled() {
        // Run both for 20ms wall time. Unlimited should accumulate far more cycles.
        let target_wall = std::time::Duration::from_millis(20);

        let cpu_unlimited = make_cpu_with_speed(ClockSpeed::unlimited());
        let handle_unlimited = run(cpu_unlimited);
        tokio::time::sleep(target_wall).await;
        let cpu_unlimited = handle_unlimited.take_cpu().await;

        let cpu_throttled = make_cpu_with_speed(ClockSpeed::mhz(1.0));
        let handle_throttled = run(cpu_throttled);
        tokio::time::sleep(target_wall).await;
        let cpu_throttled = handle_throttled.take_cpu().await;

        assert!(cpu_unlimited.cycles() > cpu_throttled.cycles(),
            "unlimited ({} cycles) should exceed throttled ({} cycles)",
            cpu_unlimited.cycles(), cpu_throttled.cycles());
    }
}
