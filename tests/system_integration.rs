use emma65::emulator::{
    Acia6551, AddressRange, Bus, ClockSpeed, Console, CpuBuilder, CpuVariant, DeviceId,
    InvalidOpcodePolicy, Mc6850, Mnemonic, PipeTransport, StepResult, Transport, Via6522,
};

const MAX_STEPS: u32 = 10_000;

/// Builds a CPU with 64 KB RAM and a small program written at `prog_addr`.
/// The reset vector is set to `prog_addr`.
fn build_cpu(prog_addr: u16, prog: &[u8]) -> emma65::emulator::Cpu {
    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
        .unwrap()
        .build();
    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(prog_addr + i as u16, b).unwrap();
    }
    cpu.bus_mut().write(0xFFFC, (prog_addr & 0xFF) as u8).unwrap();
    cpu.bus_mut().write(0xFFFD, (prog_addr >> 8) as u8).unwrap();
    cpu.reset().unwrap();
    cpu
}

/// Steps `cpu` until `StepResult::Stopped`, panicking on any error or if the step
/// budget is exhausted.
fn step_to_stop(cpu: &mut emma65::emulator::Cpu) {
    for _ in 0..MAX_STEPS {
        match cpu.step() {
            StepResult::Stopped => return,
            StepResult::Executed(_) | StepResult::Waiting => {}
            StepResult::Error(e) => panic!("CPU error: {e}"),
            StepResult::Breakpoint(_)
            | StepResult::WatchTriggered { .. }
            | StepResult::WatchError { .. } => unreachable!(),
        }
    }
    panic!("CPU did not reach STP within {MAX_STEPS} steps");
}

/// Steps `cpu` until `StepResult::Stopped`, panicking on any error or if the wall-clock
/// deadline is exceeded. Used when the CPU may spend many steps in WAI waiting for a
/// device interrupt that arrives asynchronously from another thread.
fn step_to_stop_deadline(cpu: &mut emma65::emulator::Cpu, deadline: std::time::Instant) {
    loop {
        if std::time::Instant::now() >= deadline {
            panic!("CPU did not reach STP within the time limit");
        }
        match cpu.step() {
            StepResult::Stopped => return,
            StepResult::Executed(_) | StepResult::Waiting => {}
            StepResult::Error(e) => panic!("CPU error: {e}"),
            StepResult::Breakpoint(_)
            | StepResult::WatchTriggered { .. }
            | StepResult::WatchError { .. } => unreachable!(),
        }
    }
}

// ---------------------------------------------------------------------------
// Category 1: Instruction integration
// ---------------------------------------------------------------------------

/// Verifies that `StepResult::Executed` carries the correct decoded-op fields
/// for a NOP instruction.
#[test]
fn step_result_carries_decoded_op() {
    // NOP at reset address, then STP so the CPU doesn't run off.
    let mut cpu = build_cpu(0x0200, &[0xEA, 0xDB]); // NOP, STP

    match cpu.step() {
        StepResult::Executed(op) => {
            assert_eq!(op.mnemonic, Mnemonic::Nop);
            assert_eq!(op.byte_len, 1);
            assert_eq!(op.base_cycles, 2);
        }
        _ => panic!("expected StepResult::Executed, got a different variant"),
    }
}

/// Verifies that a page-crossing indexed load incurs a 1-cycle penalty.
///
/// `LDA $00FF,X` with X=1 accesses $0100 (crosses page 0 → page 1).
/// Base cycles = 4; with page crossing = 5.
#[test]
fn page_crossing_adds_cycle() {
    // Program at $0200:
    //   LDX #$01       A2 01
    //   LDA $00FF,X    BD FF 00   -- effective addr $0100, crosses page boundary
    //   STP            DB
    let prog: &[u8] = &[0xA2, 0x01, 0xBD, 0xFF, 0x00, 0xDB];
    let mut cpu = build_cpu(0x0200, prog);
    // Place a sentinel value at the effective address $0100.
    cpu.bus_mut().write(0x0100, 0x42).unwrap();

    // LDX #$01 — 2 cycles
    let before_ldx = cpu.cycles();
    assert!(matches!(cpu.step(), StepResult::Executed(_)));
    assert_eq!(cpu.cycles() - before_ldx, 2);

    // LDA $00FF,X — should be 5 cycles (4 base + 1 page crossing)
    let before_lda = cpu.cycles();
    assert!(matches!(cpu.step(), StepResult::Executed(_)));
    let lda_cycles = cpu.cycles() - before_lda;
    assert_eq!(lda_cycles, 5, "expected 5 cycles for page-crossing LDA abs,X, got {lda_cycles}");
    assert_eq!(cpu.registers().a, 0x42);
}

// ---------------------------------------------------------------------------
// Category 2: Full system
// ---------------------------------------------------------------------------

/// Full-system echo test: CPU polls Console device until a byte arrives, then
/// echoes it back, verifying end-to-end transport I/O.
#[test]
fn console_full_system_echo() {
    let (local, mut remote) = PipeTransport::pair().unwrap();
    let mut console = Console::new().with_address(0xDF00);
    console.attach_transport(Box::new(local));

    // 64 KB RAM; Console at $DF00–$DF01.
    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xDEFF), 0).unwrap()
        .device(AddressRange::new(0xDF00, 0xDF01), DeviceId(1), Box::new(console)).unwrap()
        .ram_with_fill(AddressRange::new(0xDF02, 0xFFFF), 0).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // Program at $0200:
    //   poll: LDA $DF01   AD 01 DF   -- latch poll; non-zero when byte available
    //         BEQ poll    F0 FB      -- offset -5 → back to LDA $DF01
    //         STA $DF00   8D 00 DF   -- echo latched byte to output
    //         STP         DB
    let prog: &[u8] = &[
        0xAD, 0x01, 0xDF,
        0xF0, 0xFB,
        0x8D, 0x00, 0xDF,
        0xDB,
    ];
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    // Send the byte *before* starting execution so the pipe buffer is ready.
    remote.send(0x55).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1));

    step_to_stop(&mut cpu);

    std::thread::sleep(std::time::Duration::from_millis(1));
    assert_eq!(remote.try_recv(), Some(0x55), "echoed byte should arrive on remote transport");
}

// ---------------------------------------------------------------------------
// Category 3: Device integration
// ---------------------------------------------------------------------------

/// CPU writes a byte to ACIA6551 TX register; byte appears on the remote transport.
#[test]
fn acia6551_transmit() {
    let (local, mut remote) = PipeTransport::pair().unwrap();
    let mut acia = Acia6551::new().with_address(0xDF00);
    acia.attach_transport(Box::new(local));

    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xDEFF), 0).unwrap()
        .device(AddressRange::new(0xDF00, 0xDF03), DeviceId(1), Box::new(acia)).unwrap()
        .ram_with_fill(AddressRange::new(0xDF04, 0xFFFF), 0).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // LDA #$41   A9 41
    // STA $DF00  8D 00 DF   -- write 'A' to ACIA TX
    // STP        DB
    let prog: &[u8] = &[0xA9, 0x41, 0x8D, 0x00, 0xDF, 0xDB];
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    step_to_stop(&mut cpu);

    std::thread::sleep(std::time::Duration::from_millis(1));
    assert_eq!(remote.try_recv(), Some(0x41), "ACIA TX byte should appear on remote transport");
}

/// Remote sends a byte; CPU polls ACIA6551 RDRF status and reads the received byte.
#[test]
fn acia6551_receive() {
    let (local, mut remote) = PipeTransport::pair().unwrap();
    let mut acia = Acia6551::new().with_address(0xDF00);
    acia.attach_transport(Box::new(local));

    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xDEFF), 0).unwrap()
        .device(AddressRange::new(0xDF00, 0xDF03), DeviceId(1), Box::new(acia)).unwrap()
        .ram_with_fill(AddressRange::new(0xDF04, 0xFFFF), 0).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // Program: poll status bit 3 (RDRF) until set, then read data byte.
    // $0200: LDA $DF01   AD 01 DF   -- ACIA status register
    // $0203: AND #$08    29 08      -- isolate RDRF (bit 3)
    // $0205: BEQ poll    F0 F9      -- next PC=$0207, target=$0200, offset=-7=0xF9
    // $0207: LDA $DF00   AD 00 DF   -- read RX data (clears RDRF)
    // $020A: STA $0300   8D 00 03   -- store result
    // $020D: STP         DB
    let prog: &[u8] = &[
        0xAD, 0x01, 0xDF,
        0x29, 0x08,
        0xF0, 0xF9,
        0xAD, 0x00, 0xDF,
        0x8D, 0x00, 0x03,
        0xDB,
    ];
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    remote.send(0x55).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1));

    step_to_stop(&mut cpu);

    assert_eq!(
        cpu.bus_mut().peek(0x0300).unwrap(), 0x55,
        "received byte should be stored at $0300"
    );
}

/// CPU writes a byte via MC6850 TX, then waits for TDRE to restore; byte appears on transport.
/// Then verifies receive: remote sends a byte, CPU polls RDRF and reads it.
#[test]
fn mc6850_transmit_and_receive() {
    // --- TX ---
    {
        let (local, mut remote) = PipeTransport::pair().unwrap();
        let mut mc = Mc6850::new().with_address(0xDF00);
        mc.attach_transport(Box::new(local));

        let bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xDEFF), 0).unwrap()
            .device(AddressRange::new(0xDF00, 0xDF01), DeviceId(1), Box::new(mc)).unwrap()
            .ram_with_fill(AddressRange::new(0xDF02, 0xFFFF), 0).unwrap()
            .build();

        let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
            .clock_speed(ClockSpeed::unlimited())
            .invalid_opcode_policy(InvalidOpcodePolicy::Error)
            .bus(bus)
            .build()
            .unwrap();

        // $0200: LDA #$42   A9 42
        // $0202: STA $DF01  8D 01 DF   -- TX write; TDRE clears, restores on next tick
        // $0205: LDA $DF00  AD 00 DF   -- poll status
        // $0208: AND #$02   29 02      -- isolate TDRE (bit 1)
        // $020A: BEQ poll   F0 F9      -- next PC=$020C, target=$0205, offset=-7=0xF9
        // $020C: STP        DB
        let prog: &[u8] = &[
            0xA9, 0x42,
            0x8D, 0x01, 0xDF,
            0xAD, 0x00, 0xDF,
            0x29, 0x02,
            0xF0, 0xF9,
            0xDB,
        ];
        for (i, &b) in prog.iter().enumerate() {
            cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
        }
        cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
        cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
        cpu.reset().unwrap();

        step_to_stop(&mut cpu);
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_eq!(remote.try_recv(), Some(0x42), "MC6850 TX byte should appear on remote transport");
    }

    // --- RX ---
    {
        let (local, mut remote) = PipeTransport::pair().unwrap();
        let mut mc = Mc6850::new().with_address(0xDF00);
        mc.attach_transport(Box::new(local));

        let bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xDEFF), 0).unwrap()
            .device(AddressRange::new(0xDF00, 0xDF01), DeviceId(1), Box::new(mc)).unwrap()
            .ram_with_fill(AddressRange::new(0xDF02, 0xFFFF), 0).unwrap()
            .build();

        let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
            .clock_speed(ClockSpeed::unlimited())
            .invalid_opcode_policy(InvalidOpcodePolicy::Error)
            .bus(bus)
            .build()
            .unwrap();

        // $0200: LDA $DF00  AD 00 DF   -- status; bit 0 = RDRF
        // $0203: AND #$01   29 01
        // $0205: BEQ poll   F0 F9      -- next PC=$0207, target=$0200, offset=-7=0xF9
        // $0207: LDA $DF01  AD 01 DF   -- read data (clears RDRF)
        // $020A: STA $0300  8D 00 03
        // $020D: STP        DB
        let prog: &[u8] = &[
            0xAD, 0x00, 0xDF,
            0x29, 0x01,
            0xF0, 0xF9,
            0xAD, 0x01, 0xDF,
            0x8D, 0x00, 0x03,
            0xDB,
        ];
        for (i, &b) in prog.iter().enumerate() {
            cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
        }
        cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
        cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
        cpu.reset().unwrap();

        remote.send(0x77).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1));

        step_to_stop(&mut cpu);

        assert_eq!(
            cpu.bus_mut().peek(0x0300).unwrap(), 0x77,
            "MC6850 received byte should be stored at $0300"
        );
    }
}

// ---------------------------------------------------------------------------
// Category 4: Interrupt-driven device I/O
// ---------------------------------------------------------------------------

/// ACIA6551 RX interrupt: remote sends one byte; CPU uses WAI to suspend until the IRQ fires,
/// ISR reads the byte and stores it, RTI resumes at STP.
///
/// ACIA command = $00 (IRD=0 → RX IRQ enabled; TIC=00 → TX IRQ disabled).
/// IRQ vector ($FFFE/$FFFF) points to ISR at $0400.
/// ISR: LDA $DF00 (clears RDRF, deasserts IRQ); STA $0300; RTI.
#[test]
fn acia6551_irq_driven_receive() {
    let (local, mut remote) = PipeTransport::pair().unwrap();
    let mut acia = Acia6551::new().with_address(0xDF00);
    acia.attach_transport(Box::new(local));

    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xDEFF), 0).unwrap()
        .device(AddressRange::new(0xDF00, 0xDF03), DeviceId(1), Box::new(acia)).unwrap()
        .ram_with_fill(AddressRange::new(0xDF04, 0xFFFF), 0).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // ISR at $0400:
    //   LDA $DF00   AD 00 DF   -- read RX data (clears RDRF, deasserts IRQ)
    //   STA $0300   8D 00 03   -- store received byte
    //   RTI         40
    let isr: &[u8] = &[0xAD, 0x00, 0xDF, 0x8D, 0x00, 0x03, 0x40];
    for (i, &b) in isr.iter().enumerate() {
        cpu.bus_mut().write(0x0400 + i as u16, b).unwrap();
    }

    // IRQ vector → $0400
    cpu.bus_mut().write(0xFFFE, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFF, 0x04).unwrap();

    // Program at $0200:
    //   LDA #$00    A9 00       -- RX IRQ enabled (IRD=0, TIC=00)
    //   STA $DF02   8D 02 DF   -- write command register
    //   CLI         58          -- enable IRQ
    //   WAI         CB          -- suspend until IRQ fires
    //   STP         DB          -- RTI resumes here
    let prog: &[u8] = &[
        0xA9, 0x00, 0x8D, 0x02, 0xDF,
        0x58,
        0xCB,
        0xDB,
    ];
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    // Send the byte from a background thread after a short delay so the CPU reaches
    // WAI before the byte arrives (avoiding the IRQ firing before WAI executes).
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(5));
        remote.send(0x55).unwrap();
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    step_to_stop_deadline(&mut cpu, deadline);

    assert_eq!(
        cpu.bus_mut().peek(0x0300).unwrap(), 0x55,
        "ISR should have stored the received byte at $0300"
    );
}

/// ACIA6551 TX interrupt: CPU uses IRQ-driven transmit to send 3 bytes.
///
/// ACIA command = $06 (IRD=1 → RX IRQ disabled; TIC=01 → TX IRQ enabled).
/// IRQ fires immediately when TDRE is set. ISR sends the next byte; after the last
/// byte is sent, ISR disables TX IRQ (TIC=00) so no further interrupts fire.
/// Main program spins on a zero-page counter until all bytes are sent, then STPs.
#[test]
fn acia6551_irq_driven_transmit() {
    let (local, mut remote) = PipeTransport::pair().unwrap();
    let mut acia = Acia6551::new().with_address(0xDF00);
    acia.attach_transport(Box::new(local));

    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xDEFF), 0).unwrap()
        .device(AddressRange::new(0xDF00, 0xDF03), DeviceId(1), Box::new(acia)).unwrap()
        .ram_with_fill(AddressRange::new(0xDF04, 0xFFFF), 0).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // Data to transmit at $0300
    cpu.bus_mut().write(0x0300, 0x41).unwrap(); // 'A'
    cpu.bus_mut().write(0x0301, 0x42).unwrap(); // 'B'
    cpu.bus_mut().write(0x0302, 0x43).unwrap(); // 'C'

    // ISR at $0400:
    //   LDX $01       A6 01       -- load index (ZP $01)
    //   LDA $0300,X   BD 00 03   -- load next byte
    //   STA $DF00     8D 00 DF   -- send via TX (clears TDRE, IRQ deasserts until restored)
    //   INX           E8
    //   STX $01       86 01       -- advance index
    //   DEC $00       C6 00       -- decrement remaining count
    //   BNE done      D0 05       -- if bytes remain, RTI and wait for next TDRE
    //   LDA #$02      A9 02       -- IRD=1, TIC=00: disable TX IRQ
    //   STA $DF02     8D 02 DF
    //   done: RTI    40
    let isr: &[u8] = &[
        0xA6, 0x01,
        0xBD, 0x00, 0x03,
        0x8D, 0x00, 0xDF,
        0xE8,
        0x86, 0x01,
        0xC6, 0x00,
        0xD0, 0x05,
        0xA9, 0x02,
        0x8D, 0x02, 0xDF,
        0x40,
    ];
    for (i, &b) in isr.iter().enumerate() {
        cpu.bus_mut().write(0x0400 + i as u16, b).unwrap();
    }

    // IRQ vector → $0400
    cpu.bus_mut().write(0xFFFE, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFF, 0x04).unwrap();

    // Program at $0200:
    //   LDA #$03    A9 03       -- 3 bytes remaining
    //   STA $00     85 00       -- store count in ZP $00
    //   LDA #$00    A9 00       -- index = 0
    //   STA $01     85 01       -- store index in ZP $01
    //   LDA #$06    A9 06       -- IRD=1 (RX IRQ off), TIC=01 (TX IRQ on)
    //   STA $DF02   8D 02 DF   -- write command register
    //   CLI         58          -- enable IRQ
    //   poll:
    //   LDA $00     A5 00       -- load remaining count
    //   BNE poll    D0 FD      -- loop until zero (offset -3)
    //   STP         DB
    let prog: &[u8] = &[
        0xA9, 0x03, 0x85, 0x00,
        0xA9, 0x00, 0x85, 0x01,
        0xA9, 0x06, 0x8D, 0x02, 0xDF,
        0x58,
        0xA5, 0x00,
        0xD0, 0xFD,
        0xDB,
    ];
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    step_to_stop(&mut cpu);

    std::thread::sleep(std::time::Duration::from_millis(1));
    assert_eq!(remote.try_recv(), Some(0x41), "first TX byte should be 'A'");
    assert_eq!(remote.try_recv(), Some(0x42), "second TX byte should be 'B'");
    assert_eq!(remote.try_recv(), Some(0x43), "third TX byte should be 'C'");
}

/// MC6850 RX interrupt: remote sends one byte; CPU uses WAI to suspend until the IRQ fires,
/// ISR reads the byte and stores it, RTI resumes at STP.
///
/// MC6850 control = $81 (RIE=1 → RX IRQ enabled; TC=00 → TX IRQ disabled; CD=01).
/// IRQ vector ($FFFE/$FFFF) points to ISR at $0400.
/// ISR: LDA $DF01 (clears RDRF, deasserts IRQ); STA $0300; RTI.
#[test]
fn mc6850_irq_driven_receive() {
    let (local, mut remote) = PipeTransport::pair().unwrap();
    let mut mc = Mc6850::new().with_address(0xDF00);
    mc.attach_transport(Box::new(local));

    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xDEFF), 0).unwrap()
        .device(AddressRange::new(0xDF00, 0xDF01), DeviceId(1), Box::new(mc)).unwrap()
        .ram_with_fill(AddressRange::new(0xDF02, 0xFFFF), 0).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // ISR at $0400:
    //   LDA $DF01   AD 01 DF   -- read RX data (clears RDRF, deasserts IRQ)
    //   STA $0300   8D 00 03
    //   RTI         40
    let isr: &[u8] = &[0xAD, 0x01, 0xDF, 0x8D, 0x00, 0x03, 0x40];
    for (i, &b) in isr.iter().enumerate() {
        cpu.bus_mut().write(0x0400 + i as u16, b).unwrap();
    }

    // IRQ vector → $0400
    cpu.bus_mut().write(0xFFFE, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFF, 0x04).unwrap();

    // Program at $0200:
    //   LDA #$81    A9 81       -- RIE=1, TC=00, CD=01
    //   STA $DF00   8D 00 DF   -- write control register
    //   CLI         58
    //   WAI         CB
    //   STP         DB
    let prog: &[u8] = &[
        0xA9, 0x81, 0x8D, 0x00, 0xDF,
        0x58,
        0xCB,
        0xDB,
    ];
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    // Send the byte from a background thread after a short delay so the CPU reaches
    // WAI before the byte arrives (avoiding the IRQ firing before WAI executes).
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(5));
        remote.send(0x77).unwrap();
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    step_to_stop_deadline(&mut cpu, deadline);

    assert_eq!(
        cpu.bus_mut().peek(0x0300).unwrap(), 0x77,
        "ISR should have stored the received byte at $0300"
    );
}

/// MC6850 TX interrupt: CPU uses IRQ-driven transmit to send 3 bytes.
///
/// MC6850 control = $42 (TC=10 → TX IRQ enabled; RIE=0; CD=10).
/// IRQ fires immediately when TDRE is set. ISR sends the next byte; after the last
/// byte is sent, ISR disables TX IRQ (TC=00) so no further interrupts fire.
/// Main program spins on a zero-page counter until all bytes are sent, then STPs.
#[test]
fn mc6850_irq_driven_transmit() {
    let (local, mut remote) = PipeTransport::pair().unwrap();
    let mut mc = Mc6850::new().with_address(0xDF00);
    mc.attach_transport(Box::new(local));

    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xDEFF), 0).unwrap()
        .device(AddressRange::new(0xDF00, 0xDF01), DeviceId(1), Box::new(mc)).unwrap()
        .ram_with_fill(AddressRange::new(0xDF02, 0xFFFF), 0).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // Data to transmit at $0300
    cpu.bus_mut().write(0x0300, 0x41).unwrap(); // 'A'
    cpu.bus_mut().write(0x0301, 0x42).unwrap(); // 'B'
    cpu.bus_mut().write(0x0302, 0x43).unwrap(); // 'C'

    // ISR at $0400:
    //   LDX $01       A6 01
    //   LDA $0300,X   BD 00 03
    //   STA $DF01     8D 01 DF   -- send byte (clears TDRE until next tick)
    //   INX           E8
    //   STX $01       86 01
    //   DEC $00       C6 00
    //   BNE done      D0 05
    //   LDA #$02      A9 02      -- TC=00 (TX IRQ disabled), CD=10
    //   STA $DF00     8D 00 DF
    //   done: RTI    40
    let isr: &[u8] = &[
        0xA6, 0x01,
        0xBD, 0x00, 0x03,
        0x8D, 0x01, 0xDF,
        0xE8,
        0x86, 0x01,
        0xC6, 0x00,
        0xD0, 0x05,
        0xA9, 0x02,
        0x8D, 0x00, 0xDF,
        0x40,
    ];
    for (i, &b) in isr.iter().enumerate() {
        cpu.bus_mut().write(0x0400 + i as u16, b).unwrap();
    }

    // IRQ vector → $0400
    cpu.bus_mut().write(0xFFFE, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFF, 0x04).unwrap();

    // Program at $0200:
    //   LDA #$03    A9 03
    //   STA $00     85 00
    //   LDA #$00    A9 00
    //   STA $01     85 01
    //   LDA #$42    A9 42       -- TC=10 (TX IRQ enabled), RIE=0, CD=10
    //   STA $DF00   8D 00 DF
    //   CLI         58
    //   poll:
    //   LDA $00     A5 00
    //   BNE poll    D0 FD
    //   STP         DB
    let prog: &[u8] = &[
        0xA9, 0x03, 0x85, 0x00,
        0xA9, 0x00, 0x85, 0x01,
        0xA9, 0x42, 0x8D, 0x00, 0xDF,
        0x58,
        0xA5, 0x00,
        0xD0, 0xFD,
        0xDB,
    ];
    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    step_to_stop(&mut cpu);

    std::thread::sleep(std::time::Duration::from_millis(1));
    assert_eq!(remote.try_recv(), Some(0x41), "first TX byte should be 'A'");
    assert_eq!(remote.try_recv(), Some(0x42), "second TX byte should be 'B'");
    assert_eq!(remote.try_recv(), Some(0x43), "third TX byte should be 'C'");
}

/// VIA6522 Timer 1 fires after counting down from a known value; IFR bit 6 is set.
///
/// Timer 1 is loaded with 20 cycles via latches, then started by writing counter-high.
/// The CPU runs a NOP sled to accumulate cycles, then polls IFR until bit 6 (T1) is set.
/// The test verifies the CPU reaches STP, confirming the timer fired.
#[test]
fn via6522_timer1_sets_ifr() {
    let via = Via6522::new().with_address(0xE000);

    let bus = Bus::config()
        .ram_with_fill(AddressRange::new(0x0000, 0xDFFF), 0).unwrap()
        .device(AddressRange::new(0xE000, 0xE00F), DeviceId(1), Box::new(via)).unwrap()
        .ram_with_fill(AddressRange::new(0xE010, 0xFFFF), 0).unwrap()
        .build();

    let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
        .clock_speed(ClockSpeed::unlimited())
        .invalid_opcode_policy(InvalidOpcodePolicy::Error)
        .bus(bus)
        .build()
        .unwrap();

    // Program at $0200:
    //   LDA #20        A9 14       -- T1 latch-low value (20 cycles)
    //   STA $E006      8D 06 E0   -- write T1 latch-low ($E006)
    //   LDA #0         A9 00
    //   STA $E005      8D 05 E0   -- write T1 counter-high (loads latch, starts timer)
    //   NOP × 10       EA × 10    -- burn ~20 cycles (10 × 2 cycles each)
    //   ; poll IFR bit 6 (T1 flag)
    //   poll:
    //   LDA $E00D      AD 0D E0
    //   AND #$40       29 40
    //   BEQ poll       F0 F8
    //   STP            DB
    let mut prog: Vec<u8> = vec![
        0xA9, 0x14,
        0x8D, 0x06, 0xE0,
        0xA9, 0x00,
        0x8D, 0x05, 0xE0,
    ];
    prog.extend(std::iter::repeat(0xEA).take(10)); // 10× NOP
    prog.extend_from_slice(&[
        0xAD, 0x0D, 0xE0,
        0x29, 0x40,
        0xF0, 0xF8,
        0xDB,
    ]);

    for (i, &b) in prog.iter().enumerate() {
        cpu.bus_mut().write(0x0200 + i as u16, b).unwrap();
    }
    cpu.bus_mut().write(0xFFFC, 0x00).unwrap();
    cpu.bus_mut().write(0xFFFD, 0x02).unwrap();
    cpu.reset().unwrap();

    step_to_stop(&mut cpu);

    // IFR bit 6 should be set (T1 fired).
    let ifr = cpu.bus_mut().peek(0xE00D).unwrap();
    assert_ne!(ifr & 0x40, 0, "VIA IFR bit 6 (T1) should be set after timer underflow, got IFR={ifr:#04X}");
}
