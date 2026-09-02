//! Wall-clock integration test for the MC6840 PTM, per the follow-up
//! proposed in #545 (which fixed #543, a Timer 3 prescaler jitter bug).
//!
//! Unlike the cycle-exact unit tests in `src/emulator/device/mc6840.rs` and
//! the IRQ-detection tests in `tests/system_integration.rs`, this drives a
//! real 6502 program under a throttled clock via `run()` and measures actual
//! elapsed wall-clock time between timer firings, using a memory-mapped
//! "instrumentation port" the program writes to at each period boundary —
//! the approach #543 asked for, to catch drift a cycle-accurate test with an
//! unthrottled clock can't see.
//!
//! All three timers get a baseline test; Timer 2's additionally runs over a
//! sustained interval (hundreds of periods, a couple of real seconds) rather
//! than just a handful, since #543's report was of drift noticed over
//! extended gameplay, not a few isolated timer firings.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use emma65::assembler::assemble;
use emma65::emulator::cpu::StepResult;
use emma65::emulator::device::Mc6840;
use emma65::emulator::{
    AddressRange, Bus, ClockSpeed, CpuBuilder, CpuVariant, DeviceId, InvalidOpcodePolicy, IoDevice, run,
};

/// A 1-byte memory-mapped device that records a wall-clock timestamp every
/// time the CPU writes to it. Used by the test programs below to mark period
/// boundaries without disturbing the timing being measured (the write itself
/// costs a fixed, small number of cycles like any other store).
struct InstrumentPort {
    address: u16,
    log: Arc<Mutex<Vec<Instant>>>,
}

impl InstrumentPort {
    fn new(address: u16, log: Arc<Mutex<Vec<Instant>>>) -> Self {
        Self { address, log }
    }
}

impl IoDevice for InstrumentPort {
    fn read(&mut self, _address: u16) -> u8 { 0 }
    fn write(&mut self, _address: u16, _value: u8) {
        self.log.lock().unwrap().push(Instant::now());
    }
    fn peek(&self, _address: u16) -> u8 { 0 }
    fn identity_address(&self) -> u16 { self.address }
}

/// Realistic clock speed matching the emulator's default profile (see
/// `emulator::config::default`), so measured periods correspond to a
/// plausible real-hardware configuration rather than an arbitrary value.
fn clock_speed() -> ClockSpeed {
    ClockSpeed::mhz(1.8432)
}

/// Builds the 6502 program (see module doc) that arms a single MC6840 timer
/// in continuous mode, then polls the status register in a loop, writing a
/// timestamp marker to the instrumentation port ($E010) at each period
/// boundary.
///
/// `cr_setup` is the assembler source for whatever CR1/CR2/CR3 writes are
/// needed to arm the timer under test -- supplied by the caller since each
/// timer's control register lives at a different offset (see the register
/// map at the top of `src/emulator/device/mc6840.rs`). `msb_offset`,
/// `latch_offset`, and `counter_offset` are that timer's MSB-buffer,
/// latch, and counter-read offsets from $E000. `wait_loop_prewrite`, if
/// non-empty, is inserted at the top of the polling loop before the status
/// check -- used by the Timer 3 test to rewrite CR3 on every iteration.
fn build_timer_program(
    cr_setup: &str,
    msb_offset: u16,
    latch_offset: u16,
    counter_offset: u16,
    latch: u16,
    periods: u8,
    wait_loop_prewrite: &str,
) -> String {
    format!(
        "\
.setcpu \"wdc65c02\"
.org $0200
start:
{cr_setup}
  LDA #${msb:02X}       ; latch MSB
  STA $E0{msb_offset:02X}
  LDA #${lsb:02X}       ; latch LSB -- triggers init, starts counting
  STA $E0{latch_offset:02X}
  LDA #$01
  STA $E010         ; start marker
  LDX #${periods:02X}
wait:
{wait_loop_prewrite}  LDA $E001         ; status register
  AND #$80
  BEQ wait
  LDA $E0{counter_offset:02X}   ; clears the latched IRQ, ready for next period
  LDA #$02
  STA $E010         ; period-boundary marker
  DEX
  BNE wait
  STP
",
        msb = (latch >> 8) as u8,
        lsb = (latch & 0xFF) as u8,
    )
}

/// Assembles `source`, loads it alongside an MC6840 at $E000 and an
/// [`InstrumentPort`] at $E010 into a fresh bus, and resets at the program's
/// origin. Returns the built `Cpu`, throttled at [`clock_speed`].
fn build_cpu(source: &str, port_log: Arc<Mutex<Vec<Instant>>>) -> emma65::emulator::Cpu {
    let program = assemble(source).unwrap_or_else(|errors| {
        panic!("failed to assemble test program: {errors:?}\nsource:\n{source}");
    });

    let ptm = Mc6840::new("mc6840").with_address(0xE000);
    let port = InstrumentPort::new(0xE010, port_log);

    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xDFFF), 0).unwrap()
        .device(AddressRange::new(0xE000, 0xE007), DeviceId(1), Box::new(ptm)).unwrap()
        .device(AddressRange::new(0xE010, 0xE010), DeviceId(2), Box::new(port)).unwrap()
        .ram_with_fill(AddressRange::new(0xE011, 0xFFFF), 0).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(clock_speed())
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

/// Runs `cpu` to completion (STP) under `run()`'s throttled clock, failing
/// the test if it doesn't stop within `timeout`.
async fn run_to_stop(cpu: emma65::emulator::Cpu, timeout: Duration) {
    let handle = run(cpu);
    match tokio::time::timeout(timeout, handle.wait()).await {
        Ok(StepResult::Stopped) => {}
        Ok(_) => panic!("expected StepResult::Stopped"),
        Err(_) => panic!("program did not reach STP within {timeout:?}"),
    }
}

/// A generous upper bound on how long a run of `periods` periods of
/// `expected_period` each should take, used only to bound
/// [`run_to_stop`]'s wait -- not part of the timing assertion itself, so it
/// errs on the side of slack rather than tightness.
fn generous_timeout(expected_period: Duration, periods: u8) -> Duration {
    expected_period * (2 * periods as u32 + 4)
}

/// Returns the wall-clock duration of each individual period, in order --
/// the difference between each pair of consecutive `timestamps` (the first
/// of which is the "start" marker written just before the timer was armed).
fn individual_periods(timestamps: &[Instant]) -> Vec<Duration> {
    timestamps.windows(2).map(|w| w[1] - w[0]).collect()
}

/// Asserts the *average* of `periods` is within `tolerance` of
/// `expected_period`.
///
/// A tolerance this generous (not a tight cycle-level bound) is deliberate:
/// this test measures real wall-clock time through OS thread scheduling and
/// `run()`'s batched throttle (see `run_loop` in `src/emulator/exec/mod.rs`),
/// both of which add noise a cycle-exact unit test doesn't have to account
/// for. What this is designed to catch is *drift on the order of the #543
/// bug* (double-digit percent, compounding per timer-register rewrite), not
/// sub-millisecond jitter.
fn assert_average_period_matches(periods: &[Duration], expected_period: Duration, tolerance: f64) {
    assert!(!periods.is_empty(), "need at least 1 measured period");
    let total: Duration = periods.iter().sum();
    let average = total / periods.len() as u32;

    let ratio = average.as_secs_f64() / expected_period.as_secs_f64();
    assert!(
        (1.0 - tolerance..=1.0 + tolerance).contains(&ratio),
        "average of {} measured periods was {average:?}, outside ±{:.0}% of expected {expected_period:?} \
         (ratio {ratio:.3}); all periods: {periods:?}",
        periods.len(),
        tolerance * 100.0,
    );
}

/// Asserts every individual period in `periods` is within `tolerance` of
/// `expected_period`.
///
/// This is a wider, per-sample check distinct from
/// [`assert_average_period_matches`]'s check on the *average* -- it's aimed
/// squarely at the symptom #543 reported ("the same nominal period elapses a
/// visibly inconsistent amount of real time"), i.e. period-to-period
/// inconsistency, which an average alone could mask if unusually long and
/// short periods happened to offset each other.
fn assert_periods_are_consistent(periods: &[Duration], expected_period: Duration, tolerance: f64) {
    let expected_secs = expected_period.as_secs_f64();
    for (i, period) in periods.iter().enumerate() {
        let ratio = period.as_secs_f64() / expected_secs;
        assert!(
            (1.0 - tolerance..=1.0 + tolerance).contains(&ratio),
            "period #{i} was {period:?}, outside ±{:.0}% of expected {expected_period:?} (ratio {ratio:.3})",
            tolerance * 100.0,
        );
    }
}

/// Timer 1 (no prescaler) in continuous mode, run for a handful of periods
/// with no other register traffic -- a baseline check that CR1 (reached
/// only indirectly, via CR2 bit 0 selecting it at offset 0) drives Timer 1's
/// period correctly, alongside the Timer 2 and Timer 3 coverage below.
#[tokio::test(flavor = "multi_thread")]
async fn timer1_continuous_period_matches_wall_clock() {
    const LATCH: u16 = 60_000;
    const PERIODS: u8 = 6;

    let expected_period = Duration::from_secs_f64(LATCH as f64 / clock_speed().hz_value().unwrap() as f64);

    let cr_setup = "\
  LDA #$01          ; CR2 bit 0: route offset-0 writes to CR1 instead of CR3; T2 left idle
  STA $E001         ; (external clock, unconnected, so it never advances)
  LDA #$42          ; CR1: T1 continuous, immediate init, IRQ enabled, internal clock
  STA $E000
";
    let source = build_timer_program(cr_setup, 2, 3, 2, LATCH, PERIODS, "");

    let log = Arc::new(Mutex::new(Vec::new()));
    let cpu = build_cpu(&source, log.clone());

    run_to_stop(cpu, generous_timeout(expected_period, PERIODS)).await;

    let timestamps = log.lock().unwrap().clone();
    assert_eq!(timestamps.len(), PERIODS as usize + 1, "expected 1 start marker + {PERIODS} period markers");
    let periods = individual_periods(&timestamps);
    assert_average_period_matches(&periods, expected_period, 0.25);
    assert_periods_are_consistent(&periods, expected_period, 0.35);
}

/// Timer 2 (no prescaler) in continuous mode, run over a *sustained*
/// interval -- 200 periods, a little over two real seconds -- with no other
/// register traffic. #543's report was of drift noticed over extended
/// gameplay, not a handful of timer firings, so this checks that neither the
/// average period nor any individual period drifts over a run long enough
/// to resemble that.
#[tokio::test(flavor = "multi_thread")]
async fn timer2_continuous_period_stable_over_sustained_run() {
    const LATCH: u16 = 20_000;
    const PERIODS: u8 = 200;

    let expected_period = Duration::from_secs_f64(LATCH as f64 / clock_speed().hz_value().unwrap() as f64);

    let cr_setup = "\
  LDA #$42          ; CR2: T2 continuous, immediate init, IRQ enabled, internal clock
  STA $E001
";
    let source = build_timer_program(cr_setup, 4, 5, 4, LATCH, PERIODS, "");

    let log = Arc::new(Mutex::new(Vec::new()));
    let cpu = build_cpu(&source, log.clone());

    run_to_stop(cpu, generous_timeout(expected_period, PERIODS)).await;

    let timestamps = log.lock().unwrap().clone();
    assert_eq!(timestamps.len(), PERIODS as usize + 1, "expected 1 start marker + {PERIODS} period markers");
    let periods = individual_periods(&timestamps);
    // Tighter than the short baseline tests: 200 samples average out
    // scheduling noise well, so a real systematic drift stands out clearly.
    assert_average_period_matches(&periods, expected_period, 0.15);
    // Wider than the average check: a single period is more exposed to a
    // stray scheduling hiccup, especially at this shorter ~11ms period.
    assert_periods_are_consistent(&periods, expected_period, 0.50);
}

/// Regression test tied directly to #545: Timer 3 with the divide-by-8
/// prescaler enabled, run for several periods while the polling loop
/// rewrites CR3 (with the prescale bit staying set) on *every* iteration --
/// exactly the "firmware rewrites CR3 for an unrelated reason while
/// prescaling stays enabled" scenario that used to reset the prescaler's
/// in-flight count and inject jitter every rewrite (see the fix in
/// `Mc6840::write` and `rewriting_cr3_with_prescale_bit_unchanged_preserves_prescaler_count`
/// in `src/emulator/device/mc6840.rs`).
///
/// Before the fix, resetting the prescaler on every one of these rewrites
/// (which happen far more often than once per 8-cycle prescale division)
/// would have made Timer 3 undercount dramatically, inflating the measured
/// period well outside this test's tolerance -- this test would have failed
/// (timed out) against the pre-#545 code.
#[tokio::test(flavor = "multi_thread")]
async fn timer3_prescaled_period_matches_wall_clock_despite_cr3_rewrites() {
    const LATCH: u16 = 5_000;
    const PERIODS: u8 = 6;
    const CR3: u8 = 0x43; // prescale=1, internal clock=1, mode=continuous/immediate-init, IRQ enable=1

    let expected_period =
        Duration::from_secs_f64((LATCH as u64 * 8) as f64 / clock_speed().hz_value().unwrap() as f64);

    let cr_setup = format!(
        "\
  LDA #${CR3:02X}
  STA $E000         ; CR3: arm the prescaler (off-to-on transition)
"
    );
    let wait_loop_prewrite = format!(
        "\
  LDA #${CR3:02X}
  STA $E000         ; rewrite CR3 every iteration -- prescale bit stays set throughout
"
    );
    let source = build_timer_program(&cr_setup, 6, 7, 6, LATCH, PERIODS, &wait_loop_prewrite);

    let log = Arc::new(Mutex::new(Vec::new()));
    let cpu = build_cpu(&source, log.clone());

    run_to_stop(cpu, generous_timeout(expected_period, PERIODS)).await;

    let timestamps = log.lock().unwrap().clone();
    assert_eq!(timestamps.len(), PERIODS as usize + 1, "expected 1 start marker + {PERIODS} period markers");
    let periods = individual_periods(&timestamps);
    assert_average_period_matches(&periods, expected_period, 0.25);
    assert_periods_are_consistent(&periods, expected_period, 0.40);
}
