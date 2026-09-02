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

/// Given consecutive instrumentation-port timestamps (one per period
/// boundary, in order — the first being the "start" marker written just
/// before the timer was armed), asserts the average measured period is
/// within `tolerance` of `expected_period`.
///
/// A tolerance this generous (not a tight cycle-level bound) is deliberate:
/// this test measures real wall-clock time through OS thread scheduling and
/// `run()`'s batched throttle (see `run_loop` in `src/emulator/exec/mod.rs`),
/// both of which add noise a cycle-exact unit test doesn't have to account
/// for. What this test is designed to catch is *drift on the order of the
/// #543 bug* (double-digit percent, compounding per timer-register rewrite),
/// not sub-millisecond jitter.
fn assert_average_period_matches(timestamps: &[Instant], expected_period: Duration, tolerance: f64) {
    assert!(timestamps.len() >= 2, "need at least 2 timestamps to measure a period, got {}", timestamps.len());
    let total = *timestamps.last().unwrap() - timestamps[0];
    let periods = (timestamps.len() - 1) as u32;
    let average = total / periods;

    let expected_secs = expected_period.as_secs_f64();
    let average_secs = average.as_secs_f64();
    let ratio = average_secs / expected_secs;
    assert!(
        (1.0 - tolerance..=1.0 + tolerance).contains(&ratio),
        "average measured period {average:?} is outside ±{:.0}% of expected {expected_period:?} \
         (ratio {ratio:.3}); all timestamps: {timestamps:?}",
        tolerance * 100.0,
    );
}

/// Timer 2 (no prescaler) in continuous mode, run for several periods with no
/// other register traffic, confirms each period elapses close to
/// `latch / clock_hz` in real time.
#[tokio::test(flavor = "multi_thread")]
async fn timer2_continuous_period_matches_wall_clock() {
    const LATCH: u16 = 60_000;
    const PERIODS: u8 = 6;

    let expected_period = Duration::from_secs_f64(LATCH as f64 / clock_speed().hz_value().unwrap() as f64);

    let source = format!(
        "\
.setcpu \"wdc65c02\"
.org $0200
start:
  LDA #$42          ; CR2: T2 continuous, immediate init, IRQ enabled, internal clock
  STA $E001
  LDA #${msb:02X}       ; latch MSB
  STA $E004
  LDA #${lsb:02X}       ; latch LSB -- triggers init, starts counting
  STA $E005
  LDA #$01
  STA $E010         ; start marker
  LDX #${periods:02X}
wait:
  LDA $E001         ; status register
  AND #$80
  BEQ wait
  LDA $E004         ; read T2 counter MSB -- clears the latched IRQ, ready for next period
  LDA #$02
  STA $E010         ; period-boundary marker
  DEX
  BNE wait
  STP
",
        msb = (LATCH >> 8) as u8,
        lsb = (LATCH & 0xFF) as u8,
        periods = PERIODS,
    );

    let log = Arc::new(Mutex::new(Vec::new()));
    let cpu = build_cpu(&source, log.clone());

    run_to_stop(cpu, expected_period * (PERIODS as u32 + 2)).await;

    let timestamps = log.lock().unwrap().clone();
    assert_eq!(timestamps.len(), PERIODS as usize + 1, "expected 1 start marker + {PERIODS} period markers");
    assert_average_period_matches(&timestamps, expected_period, 0.25);
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
/// against the pre-#545 code.
#[tokio::test(flavor = "multi_thread")]
async fn timer3_prescaled_period_matches_wall_clock_despite_cr3_rewrites() {
    const LATCH: u16 = 5_000;
    const PERIODS: u8 = 6;
    const CR3: u8 = 0x43; // prescale=1, internal clock=1, mode=continuous/immediate-init, IRQ enable=1

    let expected_period =
        Duration::from_secs_f64((LATCH as u64 * 8) as f64 / clock_speed().hz_value().unwrap() as f64);

    let source = format!(
        "\
.setcpu \"wdc65c02\"
.org $0200
start:
  LDA #${cr3:02X}
  STA $E000         ; CR3: arm the prescaler (off-to-on transition)
  LDA #${msb:02X}       ; latch MSB
  STA $E006
  LDA #${lsb:02X}       ; latch LSB -- triggers init, starts counting
  STA $E007
  LDA #$01
  STA $E010         ; start marker
  LDX #${periods:02X}
wait:
  LDA #${cr3:02X}
  STA $E000         ; rewrite CR3 every iteration -- prescale bit stays set throughout
  LDA $E001         ; status register
  AND #$80
  BEQ wait
  LDA $E006         ; read T3 counter MSB -- clears the latched IRQ, ready for next period
  LDA #$02
  STA $E010         ; period-boundary marker
  DEX
  BNE wait
  STP
",
        cr3 = CR3,
        msb = (LATCH >> 8) as u8,
        lsb = (LATCH & 0xFF) as u8,
        periods = PERIODS,
    );

    let log = Arc::new(Mutex::new(Vec::new()));
    let cpu = build_cpu(&source, log.clone());

    run_to_stop(cpu, expected_period * (PERIODS as u32 + 2)).await;

    let timestamps = log.lock().unwrap().clone();
    assert_eq!(timestamps.len(), PERIODS as usize + 1, "expected 1 start marker + {PERIODS} period markers");
    assert_average_period_matches(&timestamps, expected_period, 0.25);
}
