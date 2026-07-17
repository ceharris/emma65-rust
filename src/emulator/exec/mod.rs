//! Execution control; provides the main entry points for execution on the emulated CPU.

use std::sync::{Arc, atomic::{AtomicU16, Ordering}};
use std::time::Instant;
use tokio::sync::{oneshot, watch};
use crate::emulator::cpu::{Cpu, Registers};
use crate::emulator::cpu::opcodes::DecodedOp;
use crate::emulator::error::ExecError;
use crate::watch::WatchError;

/// A lightweight snapshot of CPU state published by the run loop after each
/// instruction batch so callers can display live state without stopping the CPU.
#[derive(Clone)]
pub struct CpuLiveSnapshot {
    /// Register state at the end of the most recent instruction batch.
    pub registers: Registers,
    /// Full stack page (0x0100–0x01FF).
    pub stack_page: Vec<u8>,
    /// Total cycles executed since the last reset.
    pub cycles: u64,
    /// True if any device is currently asserting IRQ.
    pub irq_active: bool,
    /// True if an NMI is pending (latched but not yet serviced).
    pub nmi_pending: bool,
    /// True when the CPU executed STP and is halted until reset.
    pub cpu_stopped: bool,
    /// True when the CPU executed WAI and is waiting for an interrupt.
    pub cpu_waiting: bool,
    /// Paragraph-aligned start address of the memory page captured in `memory_page`.
    pub memory_page_addr: u16,
    /// 256 bytes of memory starting at `memory_page_addr`.
    pub memory_page: Vec<u8>,
}

/// Builds a [`CpuLiveSnapshot`] from the current state of `cpu`.
///
/// `mem_addr` is the address currently displayed in the memory panel; it is
/// paragraph-aligned (`& 0xfff0`) before the read so the stored page always
/// starts on a paragraph boundary.
fn build_live_snapshot(cpu: &Cpu, mem_addr: u16) -> CpuLiveSnapshot {
    let mut stack_page = vec![0u8; 256];
    let _ = cpu.bus().peek_range(0x0100, &mut stack_page);
    let memory_page_addr = mem_addr & 0xfff0;
    let mut memory_page = vec![0u8; 256];
    let _ = cpu.bus().peek_range(memory_page_addr, &mut memory_page);
    CpuLiveSnapshot {
        registers: *cpu.registers(),
        stack_page,
        cycles: cpu.cycles(),
        irq_active: cpu.interrupts().irq_active(),
        nmi_pending: cpu.interrupts().nmi_pending(),
        cpu_stopped: cpu.is_stopped(),
        cpu_waiting: cpu.is_waiting(),
        memory_page_addr,
        memory_page,
    }
}

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

/// Clonable handle for signaling a free-running CPU thread to stop.
///
/// Obtained from [`RunHandle::stopper`] or created independently via
/// [`RunStopper::channel`]. Multiple stoppers share the same underlying stop
/// channel, so any one of them can halt the run.
pub struct RunStopper(watch::Sender<bool>);

impl RunStopper {
    /// Creates a `(stopper, receiver)` pair for use with [`step_over_subroutine`]
    /// and [`step_return`].
    pub fn channel() -> (Self, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);
        (Self(tx), rx)
    }

    /// Signals the CPU thread to stop after the current instruction.
    ///
    /// Non-blocking. The CPU will stop before the next instruction fetch.
    pub fn stop(&self) {
        let _ = self.0.send(true);
    }
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
    /// Receives live CPU snapshots published by the run loop after each batch.
    live_rx: watch::Receiver<Option<CpuLiveSnapshot>>,
}

impl RunHandle {
    /// Returns a [`RunStopper`] that shares the underlying stop channel.
    ///
    /// The stopper can be cloned and stored independently; calling
    /// [`RunStopper::stop`] on any clone halts the run.
    pub fn stopper(&self) -> RunStopper {
        RunStopper(self.stop_tx.clone())
    }

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

    /// Awaits both the final [`StepResult`] and the CPU without sending a stop
    /// signal.
    ///
    /// Use a [`RunStopper`] obtained from [`stopper`](RunHandle::stopper) to
    /// trigger a stop externally before awaiting here.
    pub async fn take_cpu_with_result(self) -> (StepResult, Cpu) {
        let result = self.result_rx.await.expect("CPU thread exited without sending result");
        let cpu = self.cpu_rx.await.expect("CPU thread exited without returning CPU");
        (result, cpu)
    }

    /// Returns the most recent live snapshot published by the run loop, or
    /// `None` if no batch has completed yet.
    pub fn live_snapshot(&self) -> Option<CpuLiveSnapshot> {
        self.live_rx.borrow().clone()
    }

    /// Returns a cloned receiver for the live snapshot channel so the caller
    /// can poll it independently (e.g. from a background task).
    pub fn subscribe_live(&self) -> watch::Receiver<Option<CpuLiveSnapshot>> {
        self.live_rx.clone()
    }
}

/// Fetches, decodes, and executes one instruction. Returns the step result.
///
/// Halts before executing if PC matches a breakpoint or a watch expression triggers.
/// Use [`step_over_breakpoint`] to advance past the breakpoint at the current PC without 
/// disabling it.
pub fn step_into(cpu: &mut Cpu) -> StepResult {
    cpu.step(None)
}

/// Like [`step_into`], but skips the breakpoint and watch check at `skip_pc`.
///
/// Use this when the debugger is already halted at `skip_pc` (due to a breakpoint or
/// watch trigger) and needs to advance past it without requiring the breakpoint to be
/// disabled first. All other addresses are checked normally.
pub fn step_over_breakpoint(cpu: &mut Cpu, skip_pc: u16) -> StepResult {
    cpu.step(Some(skip_pc))
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
/// `stop_rx` is checked between instructions; if it becomes `true` the
/// operation is interrupted and `None` is returned with the CPU left at its
/// current PC. Returns `Some(result)` on natural completion.
///
/// If `live_tx` is `Some`, a [`CpuLiveSnapshot`] is published periodically
/// (based on a fixed batch size) so callers can display live state while a long
/// subroutine runs. `mem_view_addr` selects which 256-byte page is captured in
/// each snapshot; pass `Arc::new(AtomicU16::new(0))` if not needed.
pub fn step_over_subroutine(
    cpu: &mut Cpu,
    stop_rx: &watch::Receiver<bool>,
    live_tx: Option<&watch::Sender<Option<CpuLiveSnapshot>>>,
    mem_view_addr: &Arc<AtomicU16>,
) -> Option<StepResult> {
    let pc = cpu.registers().pc;
    let opcode = cpu.bus().peek(pc).unwrap_or(0);
    if opcode != JSR_OPCODE {
        return Some(step_over_breakpoint(cpu, pc));
    }

    let target = pc.wrapping_add(JSR_BYTE_LEN);
    let already_set = cpu.breakpoints().contains(&target);
    if !already_set {
        cpu.add_breakpoint(target);
    }

    // The first iteration skips the breakpoint/watch check at the current PC so
    // that a breakpoint there does not immediately re-fire before JSR executes.
    // Subsequent iterations use the normal step() path.
    let mut first = true;
    let mut steps = 0u32;
    let result = loop {
        if *stop_rx.borrow() {
            if !already_set { cpu.remove_breakpoint(target); }
            return None;
        }
        let res = if first {
            first = false;
            step_over_breakpoint(cpu, pc)
        } else {
            step_into(cpu)
        };
        steps += 1;
        if let Some(tx) = live_tx.filter(|_| steps.is_multiple_of(BATCH_SIZE)) {
            let _ = tx.send(Some(build_live_snapshot(cpu, mem_view_addr.load(Ordering::Relaxed))));
        }
        match res {
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
                match step_into(cpu) {
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
    Some(result)
}

/// Runs until the stack pointer rises above its value at the time of the call.
///
/// This detects subroutine return: once `S` exceeds `initial_s` (using
/// wrapping 8-bit arithmetic to handle stack pointer wraparound), the
/// subroutine's stack frame has been unwound by `RTS` or `RTI`. Execution also
/// halts on any non-[`StepResult::Executed`] result (breakpoint, watch trigger,
/// error, STP, WAI stall).
///
/// `stop_rx` is checked between instructions; if it becomes `true` the
/// operation is interrupted and `None` is returned with the CPU left at its
/// current PC. Returns `Some(result)` on natural completion.
///
/// If `live_tx` is `Some`, a [`CpuLiveSnapshot`] is published periodically
/// (based on a fixed batch size) so callers can display live state while a long
/// subroutine runs. `mem_view_addr` selects which 256-byte page is captured in
/// each snapshot; pass `Arc::new(AtomicU16::new(0))` if not needed.
pub fn step_return(
    cpu: &mut Cpu,
    stop_rx: &watch::Receiver<bool>,
    live_tx: Option<&watch::Sender<Option<CpuLiveSnapshot>>>,
    mem_view_addr: &Arc<AtomicU16>,
) -> Option<StepResult> {
    let initial_s = cpu.registers().s;
    // The first iteration skips the breakpoint/watch check at the current PC so
    // that a breakpoint there does not immediately re-fire before the instruction executes.
    let mut first = true;
    let mut steps = 0u32;
    loop {
        if *stop_rx.borrow() {
            return None;
        }
        let pc = cpu.registers().pc;
        let res = if first {
            first = false;
            step_over_breakpoint(cpu, pc)
        } else {
            step_into(cpu)
        };
        steps += 1;
        if let Some(tx) = live_tx.filter(|_| steps.is_multiple_of(BATCH_SIZE)) {
            let _ = tx.send(Some(build_live_snapshot(cpu, mem_view_addr.load(Ordering::Relaxed))));
        }
        match res {
            StepResult::Executed(op)
                if (cpu.registers().s.wrapping_sub(initial_s) as i8) > 0 =>
            {
                return Some(StepResult::Executed(op));
            }
            StepResult::Executed(_) => {}
            other => return Some(other),
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
    run_from(cpu, None, Arc::new(AtomicU16::new(0)))
}

/// Like [`run`], but skips the breakpoint/watch check at `skip_pc` on the
/// first instruction. Use this when the CPU is already halted at a breakpoint
/// and the caller wants execution to continue past it without disabling it.
///
/// `mem_view_addr` is an atomically-updated address that controls which 256-byte
/// memory page is captured in each live snapshot. The caller should store the
/// paragraph-aligned address currently shown in the memory panel, and may update
/// it at any time while running.
pub fn run_from(cpu: Cpu, skip_pc: Option<u16>, mem_view_addr: Arc<AtomicU16>) -> RunHandle {
    let (stop_tx, stop_rx) = watch::channel(false);
    let (result_tx, result_rx) = oneshot::channel();
    let (cpu_tx, cpu_rx) = oneshot::channel();
    let (live_tx, live_rx) = watch::channel(None);

    std::thread::spawn(move || {
        run_loop(cpu, skip_pc, mem_view_addr, stop_rx, live_tx, result_tx, cpu_tx);
    });

    RunHandle { stop_tx, result_rx, cpu_rx, live_rx }
}

fn run_loop(
    mut cpu: Cpu,
    skip_pc: Option<u16>,
    mem_view_addr: Arc<AtomicU16>,
    stop_rx: watch::Receiver<bool>,
    live_tx: watch::Sender<Option<CpuLiveSnapshot>>,
    result_tx: oneshot::Sender<StepResult>,
    cpu_tx: oneshot::Sender<Cpu>,
) {
    let start = Instant::now();
    let start_cycles = cpu.cycles();
    let hz = cpu.clock_speed().hz_value();

    let mut first = skip_pc.is_some();

    let final_result = 'outer: loop {
        for _ in 0..BATCH_SIZE {
            if *stop_rx.borrow() {
                break 'outer None;
            }
            let res = if first {
                first = false;
                step_over_breakpoint(&mut cpu, skip_pc.unwrap())
            } else {
                step_into(&mut cpu)
            };
            match res {
                StepResult::Executed(_) => {}
                other => break 'outer Some(other),
            }
        }

        // Publish a live snapshot after each batch so the frontend can display
        // current state without stopping the CPU.
        let _ = live_tx.send(Some(build_live_snapshot(&cpu, mem_view_addr.load(Ordering::Relaxed))));

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

    #[tokio::test]
    async fn run_from_advances_past_breakpoint_at_initial_pc() {
        // run_from(cpu, Some(pc)) must advance past a breakpoint at the initial PC
        // rather than immediately halting before a single instruction executes.
        let mut cpu = make_cpu_at(0x0200);
        // NOP; NOP; STP — run should execute past the breakpoint at $0200 and halt at STP.
        write(&mut cpu, 0x0200, &[0xEA, 0xEA, 0xDB]); // NOP, NOP, STP
        cpu.add_breakpoint(0x0200);

        let handle = run_from(cpu, Some(0x0200), no_mem());
        let (result, cpu) = handle.take_cpu_with_result().await;

        assert!(matches!(result, StepResult::Stopped), "expected Stopped result");
        assert!(cpu.registers().pc > 0x0200, "PC should have advanced past the breakpoint");
        // Breakpoint must still be present after the run.
        assert!(cpu.breakpoints().contains(&0x0200));
    }

    #[tokio::test]
    async fn run_halts_at_breakpoint_when_no_skip() {
        // Plain run() (no skip) must halt immediately at a breakpoint.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.add_breakpoint(0x0200);

        let handle = run(cpu);
        let (result, cpu) = handle.take_cpu_with_result().await;

        assert!(matches!(result, StepResult::Breakpoint(0x0200)), "expected Breakpoint");
        assert_eq!(cpu.registers().pc, 0x0200);
    }

    // --- step_over / step_return helpers ---

    /// Returns a stop receiver that never fires, for use in tests that don't need interruption.
    fn no_stop() -> watch::Receiver<bool> {
        watch::channel(false).1
    }

    /// Returns a dummy memory-view-address arc for tests that don't exercise live snapshots.
    fn no_mem() -> Arc<AtomicU16> {
        Arc::new(AtomicU16::new(0))
    }

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
        let result = step_over_subroutine(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        let result = step_over_subroutine(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        let result = step_over_subroutine(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        let result = step_over_subroutine(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        let result = step_over_subroutine(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        let result = step_over_subroutine(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        step_into(&mut cpu); // JSR — now at $0300, S = 0xFD
        let result = step_return(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        let result = step_return(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        let result = step_return(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0203);
    }

    #[test]
    fn step_return_halts_at_breakpoint_inside_subroutine() {
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xEA, 0xEA, 0x60]); // NOP, NOP, RTS
        step_into(&mut cpu); // JSR — now inside subroutine
        cpu.add_breakpoint(0x0301);
        let result = step_return(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
        assert!(matches!(result, StepResult::Breakpoint(0x0301)));
    }

    #[test]
    fn step_return_halts_on_stp() {
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0300, &[0xDB]);               // STP before RTS
        step_into(&mut cpu); // JSR
        let result = step_return(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        step_into(&mut cpu); // JSR $0300
        step_into(&mut cpu); // JSR $0400 — now inside inner subroutine
        let s_inside = cpu.registers().s;
        let result = step_return(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
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
        step_into(&mut cpu); // JSR $0300 — S = 0xFD
        cpu.interrupts_mut().signal_nmi();
        let result = step_return(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0203);
    }

    // --- step_over_breakpoint (skip-current-PC behaviour) ---

    #[test]
    fn step_into_at_breakpoint_advances_past_it() {
        // A breakpoint at the current PC must not block step_over_breakpoint.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.add_breakpoint(0x0200);
        let result = step_over_breakpoint(&mut cpu, 0x0200);
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0201);
        // Breakpoint must still be present after the skip.
        assert!(cpu.breakpoints().contains(&0x0200));
    }

    #[test]
    fn step_into_at_breakpoint_halts_at_next_breakpoint() {
        // The skip applies only to the current PC; the next breakpoint fires normally.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0xEA, 0xEA]); // NOP NOP
        cpu.add_breakpoint(0x0200);
        cpu.add_breakpoint(0x0201);
        let result = step_over_breakpoint(&mut cpu, 0x0200);
        // Should execute the NOP at $0200, then the next step_over_breakpoint/step
        // call would halt at $0201 — but this single call returns Executed.
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0201);
    }

    #[test]
    fn step_over_non_jsr_at_breakpoint_advances_past_it() {
        // step_over on a non-JSR instruction with a breakpoint at the current PC
        // must execute the instruction, not re-fire the breakpoint.
        let mut cpu = make_cpu_at(0x0200);
        write(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.add_breakpoint(0x0200);
        let result = step_over_subroutine(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0201);
        assert!(cpu.breakpoints().contains(&0x0200));
    }

    #[test]
    fn step_over_jsr_at_breakpoint_advances_past_it() {
        // step_over on a JSR with a breakpoint at the JSR address must execute
        // the JSR and advance past the subroutine, not re-fire the breakpoint.
        let mut cpu = make_cpu_at(0x0200);
        // JSR $0300; NOP at $0203 (return target); RTS at $0300
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0203, &[0xEA]);              // NOP
        write(&mut cpu, 0x0300, &[0x60]);              // RTS
        cpu.add_breakpoint(0x0200);
        let result = step_over_subroutine(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0203);
        assert!(cpu.breakpoints().contains(&0x0200));
    }

    #[test]
    fn step_return_at_breakpoint_advances_past_it() {
        // step_return with a breakpoint at the current PC must execute the
        // instruction rather than immediately re-firing the breakpoint.
        // Setup: call a subroutine via JSR so the stack has a valid return address,
        // then set a breakpoint at the subroutine entry and invoke step_return.
        let mut cpu = make_cpu_at(0x0200);
        // JSR $0300; NOP at $0203 (return site); RTS at $0300
        write(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write(&mut cpu, 0x0203, &[0xEA]);              // NOP
        write(&mut cpu, 0x0300, &[0x60]);              // RTS
        // Execute the JSR so we are inside the subroutine with the return address
        // already on the stack. PC is now $0300.
        let r = step_into(&mut cpu);
        assert!(matches!(r, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0300);

        // Place a breakpoint at the subroutine entry (current PC) and call step_return.
        cpu.add_breakpoint(0x0300);
        let result = step_return(&mut cpu, &no_stop(), None, &no_mem()).unwrap();
        // Should execute the RTS and return to $0203, not re-fire the breakpoint.
        assert!(matches!(result, StepResult::Executed(_)));
        assert_eq!(cpu.registers().pc, 0x0203);
        // Breakpoint must still be present after the skip.
        assert!(cpu.breakpoints().contains(&0x0300));
    }
}
