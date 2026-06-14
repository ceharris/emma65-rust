use std::sync::{Arc, Mutex};

use emma65::emulator::{
    AddressRange, BusOp, BusTraceCallback, ClockSpeed, Console, CpuBuilder, CpuVariant,
    DeviceId, InvalidOpcodePolicy, PipeTransport, StepResult, TraceRecord, run,
};

/// Collects bus trace records into a shared vec so tests can inspect them after execution.
struct CapturingCallback {
    records: Arc<Mutex<Vec<TraceRecord>>>,
}

impl BusTraceCallback for CapturingCallback {
    fn record(&mut self, rec: TraceRecord) {
        self.records.lock().unwrap().push(rec);
    }
}

/// Verifies that bytes written by a free-running CPU to a Console device appear on the
/// remote end of a PipeTransport.
///
/// The program writes 'A' ($41) and 'B' ($42) to the console output register then loops
/// forever. The test polls the remote pipe until both bytes arrive, then stops the CPU.
#[tokio::test]
async fn free_run_console_output() {
    use emma65::emulator::{Bus, Transport};

    let (local, mut remote) = PipeTransport::pair().unwrap();
    let mut console = Console::new();
    console.attach_transport(Box::new(local));

    // 64 KB RAM; Console at $F000–$F001.
    let bus = Bus::config()
        .ram(AddressRange::new(0x0000, 0xEFFF)).unwrap()
        .device(AddressRange::new(0xF000, 0xF001), DeviceId(1), Box::new(console)).unwrap()
        .ram(AddressRange::new(0xF002, 0xFFFF)).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // Program at $0200:
    //   LDA #$41   A9 41
    //   STA $F000  8D 00 F0   -- write 'A' to console
    //   LDA #$42   A9 42
    //   STA $F000  8D 00 F0   -- write 'B' to console
    //   BRA -2     80 FE      -- loop forever
    let prog: &[u8] = &[
        0xA9, 0x41,
        0x8D, 0x00, 0xF0,
        0xA9, 0x42,
        0x8D, 0x00, 0xF0,
        0x80, 0xFE,
    ];
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    // Reset vector → $0200
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    let handle = run(cpu);

    // Poll for both bytes (≤50ms).
    let mut received: Vec<u8> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    while received.len() < 2 && std::time::Instant::now() < deadline {
        if let Some(b) = remote.try_recv() {
            received.push(b);
        } else {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    let cpu = handle.take_cpu().await;

    assert_eq!(received, vec![0x41, 0x42], "expected 'A' then 'B' on the transport");
    assert!(cpu.cycles() > 0, "CPU should have executed at least one instruction");
}

/// Verifies that bus trace records captured during CPU execution carry the correct
/// addresses, values, operations, and monotonically non-decreasing timestamps.
///
/// The program executes `LDA #$55`, `STA $0300`, `LDA $0300`, `STP`. The test asserts:
/// - exactly one write to $0300 (value $55)
/// - at least one read from $0300 (value $55)
/// - at least one fetch of the LDA-immediate opcode byte ($A9) from $0200
/// - all timestamps are non-decreasing
#[test]
fn bus_trace_captures_reads_and_writes() {
    use emma65::emulator::Bus;

    let records = Arc::new(Mutex::new(Vec::<TraceRecord>::new()));

    let bus = Bus::config()
        .ram(AddressRange::new(0x0000, 0xFFFF)).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // Program at $0200:
    //   LDA #$55   A9 55
    //   STA $0300  8D 00 03
    //   LDA $0300  AD 00 03
    //   STP        DB
    let prog: &[u8] = &[
        0xA9, 0x55,
        0x8D, 0x00, 0x03,
        0xAD, 0x00, 0x03,
        0xDB,
    ];
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    // Reset vector → $0200
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    cpu.bus_mut().set_trace_callback(Some(Box::new(CapturingCallback {
        records: records.clone(),
    })));

    loop {
        match cpu.step() {
            StepResult::Stopped => break,
            StepResult::Error(e) => panic!("CPU error: {e}"),
            StepResult::Executed(_) | StepResult::Waiting => {}
            StepResult::Breakpoint(_)
            | StepResult::WatchTriggered { .. }
            | StepResult::WatchError { .. } => unreachable!(),
        }
    }

    let recs = records.lock().unwrap();

    // Exactly one write to $0300 with value $55.
    let writes_to_0300: Vec<_> = recs.iter()
        .filter(|r| r.addr == 0x0300 && r.op == BusOp::Write)
        .collect();
    assert_eq!(writes_to_0300.len(), 1, "expected exactly one write to $0300");
    assert_eq!(writes_to_0300[0].value, 0x55);

    // At least one read from $0300 with value $55.
    let reads_from_0300: Vec<_> = recs.iter()
        .filter(|r| r.addr == 0x0300 && r.op == BusOp::Read)
        .collect();
    assert!(!reads_from_0300.is_empty(), "expected at least one read from $0300");
    assert!(reads_from_0300.iter().any(|r| r.value == 0x55), "read from $0300 should yield $55");

    // At least one fetch of the LDA-immediate opcode ($A9) from $0200.
    let fetches_0200: Vec<_> = recs.iter()
        .filter(|r| r.addr == 0x0200 && r.op == BusOp::Read && r.value == 0xA9)
        .collect();
    assert!(!fetches_0200.is_empty(), "expected opcode fetch from $0200 (LDA #imm = $A9)");

    // Timestamps are monotonically non-decreasing.
    for window in recs.windows(2) {
        assert!(
            window[1].timestamp_ns >= window[0].timestamp_ns,
            "timestamps must be non-decreasing: {} then {}",
            window[0].timestamp_ns,
            window[1].timestamp_ns,
        );
    }
}
