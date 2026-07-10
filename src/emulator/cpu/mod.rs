//! Emulated 6502 CPU; instruction fetch, decode, and execution
//! 
//! See the [`exec`](crate::emulator::exec) module for the high level interface for
//! executing 6502 instructions.

pub mod alu;
pub mod opcodes;
pub mod status;
pub mod variant;

use std::collections::HashSet;
use log::debug;
use crate::emulator::bus::{Bus, BusOp, InterruptController};
use crate::emulator::cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic, decode_table};
use crate::emulator::cpu::status::StatusRegister;
use crate::emulator::cpu::variant::{CpuVariant, InvalidOpcodePolicy};
use crate::emulator::error::{BusError, CpuBuildError, ExecError};
use crate::emulator::exec::{ClockSpeed, StepResult};
use crate::watch::{Operand, WatchContext, WatchEvaluator};

const STACK_BASE: u16 = 0x0100;
const RESET_VECTOR: u16 = 0xFFFC;
const NMI_VECTOR: u16 = 0xFFFA;
const IRQ_VECTOR: u16 = 0xFFFE;

/// The CPU's general-purpose and special-purpose registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registers {
    /// Accumulator
    pub a: u8,
    /// X index register
    pub x: u8,
    /// Y index register
    pub y: u8,
    /// Stack pointer (points to next free slot; stack is at 0x0100–0x01FF)
    pub s: u8,
    /// Program counter
    pub pc: u16,
    /// Processor status flags
    pub p: StatusRegister,
}

impl Registers {
    fn new() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            s: 0xFF,
            pc: 0,
            p: StatusRegister::UNUSED | StatusRegister::I,
        }
    }
}

/// The 65C02 CPU: registers, bus, decode table, and execution state.
pub struct Cpu {
    /// General-purpose and special-purpose registers (A, X, Y, S, PC, P).
    regs: Registers,
    /// The memory bus; owns all RAM, ROM, and IO device regions.
    bus: Bus,
    /// Interrupt controller; tracks IRQ sources and pending NMI.
    interrupts: InterruptController,
    /// Watch expression evaluator; owns watchpoints and variable storage.
    evaluator: WatchEvaluator,
    /// PC addresses that trigger a `StepResult::Breakpoint` before execution.
    breakpoints: HashSet<u16>,
    /// Pre-built 256-entry decode table for the active variant; indexed by opcode byte.
    table: [DecodedOp; 256],
    /// Selects the instruction set (CMOS 65C02 or WDC 65C02).
    variant: CpuVariant,
    /// Governs how unrecognized or variant-invalid opcodes are handled.
    invalid_opcode_policy: InvalidOpcodePolicy,
    /// Target clock frequency; used by free-running mode to throttle execution.
    clock_speed: ClockSpeed,
    /// Cumulative clock cycles elapsed since the last `reset()`.
    cycles: u64,
    /// True when a WAI instruction has been executed and the CPU is waiting for an interrupt.
    waiting: bool,
    /// True when a STP instruction has been executed; only `reset()` clears this.
    stopped: bool,
}

impl Cpu {
    /// Returns a `CpuBuilder` for constructing a `Cpu` with the given variant.
    pub fn builder(variant: CpuVariant) -> CpuBuilder {
        CpuBuilder::new(variant)
    }

    /// Returns a reference to the current register state.
    pub fn registers(&self) -> &Registers {
        &self.regs
    }

    /// Returns a mutable reference to the register state.
    pub fn registers_mut(&mut self) -> &mut Registers {
        &mut self.regs
    }

    /// Returns a reference to the bus.
    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    /// Returns a mutable reference to the bus.
    pub fn bus_mut(&mut self) -> &mut Bus {
        &mut self.bus
    }

    /// Returns a reference to the interrupt controller.
    pub fn interrupts(&self) -> &InterruptController {
        &self.interrupts
    }

    /// Returns a mutable reference to the interrupt controller.
    pub fn interrupts_mut(&mut self) -> &mut InterruptController {
        &mut self.interrupts
    }

    /// Returns `true` if the CPU is in WAI state, waiting for an interrupt.
    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    /// Returns `true` if the CPU is in STP state.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Returns a reference to the watch evaluator.
    pub fn evaluator(&self) -> &WatchEvaluator {
        &self.evaluator
    }

    /// Returns a mutable reference to the watch evaluator.
    pub fn evaluator_mut(&mut self) -> &mut WatchEvaluator {
        &mut self.evaluator
    }

    /// Adds `addr` to the breakpoint set.
    pub fn add_breakpoint(&mut self, addr: u16) {
        self.breakpoints.insert(addr);
    }

    /// Removes `addr` from the breakpoint set. Returns `true` if it was present.
    pub fn remove_breakpoint(&mut self, addr: u16) -> bool {
        self.breakpoints.remove(&addr)
    }

    /// Clears all breakpoints.
    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
    }

    /// Returns the current breakpoint set.
    pub fn breakpoints(&self) -> &HashSet<u16> {
        &self.breakpoints
    }

    /// Returns the CPU variant.
    pub fn variant(&self) -> CpuVariant {
        self.variant
    }

    /// Returns the configured target clock speed.
    pub fn clock_speed(&self) -> ClockSpeed {
        self.clock_speed
    }

    /// Returns the total number of clock cycles elapsed since construction or the last reset.
    pub fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Reads the reset vector and initializes registers. Clears WAI/STP state.
    pub fn reset(&mut self) -> Result<(), ExecError> {
        self.bus_reset();
        let lo = self.bus_read(RESET_VECTOR)?;
        let hi = self.bus_read(RESET_VECTOR + 1)?;
        self.regs.pc = u16::from_le_bytes([lo, hi]);
        self.regs.s = 0xFF;
        self.regs.p = StatusRegister::UNUSED | StatusRegister::I;
        self.cycles = 0;
        self.waiting = false;
        self.stopped = false;
        debug!("6502 CPU reset");
        Ok(())
    }

    /// Fetches, decodes, and executes one instruction. Returns the step result.
    /// Skips a breakpoint at `skip_pc` if specified.
    pub fn step(&mut self, skip_pc: Option<u16>) -> StepResult {
        if self.stopped {
            return StepResult::Stopped;
        }

        if self.waiting {
            // Tick devices and poll for interrupts; stay in WAI until one arrives.
            self.bus.tick_devices(1);
            self.poll_interrupts();
            if !self.interrupts.irq_active() && !self.interrupts.nmi_pending() {
                return StepResult::Waiting;
            }
            self.waiting = false;
            // Fall through to service the interrupt below.
        }

        self.bus.advance_trace_timestamp();
        let pc = self.regs.pc;

        // Breakpoint and watch checks — skipped for skip_pc so the debugger can
        // advance past an address it is already halted at.
        if skip_pc != Some(pc) {
            if self.breakpoints.contains(&pc) {
                return StepResult::Breakpoint(pc);
            }

            let watch_result = {
                let ctx = CpuWatchContext { regs: &self.regs, bus: &self.bus };
                self.evaluator.evaluate_all(&ctx)
            };
            match watch_result {
                Ok(Some(index)) => return StepResult::WatchTriggered { watch_index: index, pc },
                Err((index, error)) => return StepResult::WatchError { watch_index: index, pc, error },
                Ok(None) => {}
            }
        }

        // NMI takes priority over IRQ.
        if self.interrupts.take_nmi() {
            return match self.service_interrupt(NMI_VECTOR, false) {
                Ok(cycles) => {
                    self.cycles += cycles as u64;
                    self.bus.tick_devices(cycles as u32);
                    self.poll_interrupts();
                    StepResult::Executed(self.table[0x00]) // placeholder decoded for interrupt
                }
                Err(e) => StepResult::Error(e),
            };
        }

        if self.interrupts.irq_active() && !self.regs.p.contains(StatusRegister::I) {
            return match self.service_interrupt(IRQ_VECTOR, false) {
                Ok(cycles) => {
                    self.cycles += cycles as u64;
                    self.bus.tick_devices(cycles as u32);
                    self.poll_interrupts();
                    StepResult::Executed(self.table[0x00])
                }
                Err(e) => StepResult::Error(e),
            };
        }

        let opcode = match self.bus_read(pc) {
            Ok(b) => b,
            Err(e) => return StepResult::Error(e),
        };

        let decoded = self.table[opcode as usize];

        if !decoded.is_valid {
            match self.invalid_opcode_policy {
                InvalidOpcodePolicy::Nop => {
                    self.regs.pc = self.regs.pc.wrapping_add(decoded.byte_len as u16);
                    let cycles = decoded.base_cycles;
                    self.cycles += cycles as u64;
                    self.bus.tick_devices(cycles as u32);
                    self.poll_interrupts();
                    return StepResult::Executed(decoded);
                }
                InvalidOpcodePolicy::Error => {
                    return StepResult::Error(ExecError::InvalidOpcode { addr: pc, opcode });
                }
            }
        }

        match self.execute(decoded) {
            Ok((result, cycles)) => {
                self.cycles += cycles as u64;
                self.bus.tick_devices(cycles as u32);
                self.poll_interrupts();
                result
            }
            Err(e) => StepResult::Error(e),
        }
    }

    // --- private execution core ---

    fn execute(&mut self, decoded: DecodedOp) -> Result<(StepResult, u8), ExecError> {
        let pc = self.regs.pc;
        // Advance PC past the instruction bytes; some instructions (JMP, JSR, branches, RTI, RTS)
        // override this below.
        self.regs.pc = pc.wrapping_add(decoded.byte_len as u16);

        let mut extra_cycles: u8 = 0;

        match decoded.mnemonic {
            // --- Load/Store ---
            Mnemonic::Lda => {
                let (val, xc) = self.read_operand(decoded.mode, pc)?;
                extra_cycles += xc;
                self.regs.a = val;
                self.set_nz(val);
            }
            Mnemonic::Ldx => {
                let (val, xc) = self.read_operand(decoded.mode, pc)?;
                extra_cycles += xc;
                self.regs.x = val;
                self.set_nz(val);
            }
            Mnemonic::Ldy => {
                let (val, xc) = self.read_operand(decoded.mode, pc)?;
                extra_cycles += xc;
                self.regs.y = val;
                self.set_nz(val);
            }
            Mnemonic::Sta => {
                let addr = self.effective_addr(decoded.mode, pc, false)?;
                self.bus_write(addr, self.regs.a)?;
            }
            Mnemonic::Stx => {
                let addr = self.effective_addr(decoded.mode, pc, false)?;
                self.bus_write(addr, self.regs.x)?;
            }
            Mnemonic::Sty => {
                let addr = self.effective_addr(decoded.mode, pc, false)?;
                self.bus_write(addr, self.regs.y)?;
            }
            Mnemonic::Stz => {
                let addr = self.effective_addr(decoded.mode, pc, false)?;
                self.bus_write(addr, 0)?;
            }

            // --- Transfers ---
            Mnemonic::Tax => { self.regs.x = self.regs.a; self.set_nz(self.regs.x); }
            Mnemonic::Tay => { self.regs.y = self.regs.a; self.set_nz(self.regs.y); }
            Mnemonic::Txa => { self.regs.a = self.regs.x; self.set_nz(self.regs.a); }
            Mnemonic::Tya => { self.regs.a = self.regs.y; self.set_nz(self.regs.a); }
            Mnemonic::Tsx => { self.regs.x = self.regs.s; self.set_nz(self.regs.x); }
            Mnemonic::Txs => { self.regs.s = self.regs.x; }

            // --- Stack ---
            Mnemonic::Pha => self.push(self.regs.a)?,
            Mnemonic::Php => {
                let p = (self.regs.p | StatusRegister::B | StatusRegister::UNUSED).to_byte();
                self.push(p)?;
            }
            Mnemonic::Phx => self.push(self.regs.x)?,
            Mnemonic::Phy => self.push(self.regs.y)?,
            Mnemonic::Pla => {
                let val = self.pop()?;
                self.regs.a = val;
                self.set_nz(val);
            }
            Mnemonic::Plp => {
                let val = self.pop()?;
                self.regs.p = StatusRegister::from_byte(val) | StatusRegister::UNUSED;
            }
            Mnemonic::Plx => {
                let val = self.pop()?;
                self.regs.x = val;
                self.set_nz(val);
            }
            Mnemonic::Ply => {
                let val = self.pop()?;
                self.regs.y = val;
                self.set_nz(val);
            }

            // --- Arithmetic ---
            Mnemonic::Adc => {
                let (val, xc) = self.read_operand(decoded.mode, pc)?;
                extra_cycles += xc;
                let result = if self.regs.p.contains(StatusRegister::D) {
                    alu::adc_bcd(self.regs.a, val, self.regs.p)
                } else {
                    alu::adc_binary(self.regs.a, val, self.regs.p)
                };
                self.regs.a = result.value;
                self.regs.p = result.status;
            }
            Mnemonic::Sbc => {
                let (val, xc) = self.read_operand(decoded.mode, pc)?;
                extra_cycles += xc;
                let result = if self.regs.p.contains(StatusRegister::D) {
                    alu::sbc_bcd(self.regs.a, val, self.regs.p)
                } else {
                    alu::sbc_binary(self.regs.a, val, self.regs.p)
                };
                self.regs.a = result.value;
                self.regs.p = result.status;
            }

            // --- Logic ---
            Mnemonic::And => {
                let (val, xc) = self.read_operand(decoded.mode, pc)?;
                extra_cycles += xc;
                let r = alu::and(self.regs.a, val, self.regs.p);
                self.regs.a = r.value;
                self.regs.p = r.status;
            }
            Mnemonic::Ora => {
                let (val, xc) = self.read_operand(decoded.mode, pc)?;
                extra_cycles += xc;
                let r = alu::ora(self.regs.a, val, self.regs.p);
                self.regs.a = r.value;
                self.regs.p = r.status;
            }
            Mnemonic::Eor => {
                let (val, xc) = self.read_operand(decoded.mode, pc)?;
                extra_cycles += xc;
                let r = alu::eor(self.regs.a, val, self.regs.p);
                self.regs.a = r.value;
                self.regs.p = r.status;
            }

            // --- Compare ---
            Mnemonic::Cmp => {
                let (val, xc) = self.read_operand(decoded.mode, pc)?;
                extra_cycles += xc;
                self.regs.p = alu::compare(self.regs.a, val, self.regs.p);
            }
            Mnemonic::Cpx => {
                let (val, _) = self.read_operand(decoded.mode, pc)?;
                self.regs.p = alu::compare(self.regs.x, val, self.regs.p);
            }
            Mnemonic::Cpy => {
                let (val, _) = self.read_operand(decoded.mode, pc)?;
                self.regs.p = alu::compare(self.regs.y, val, self.regs.p);
            }

            // --- Shifts ---
            Mnemonic::Asl => {
                if decoded.mode == AddressingMode::Accumulator {
                    let r = alu::asl(self.regs.a, self.regs.p);
                    self.regs.a = r.value;
                    self.regs.p = r.status;
                } else {
                    let addr = self.effective_addr(decoded.mode, pc, false)?;
                    let val = self.bus_read(addr)?;
                    let r = alu::asl(val, self.regs.p);
                    self.bus_write(addr, r.value)?;
                    self.regs.p = r.status;
                }
            }
            Mnemonic::Lsr => {
                if decoded.mode == AddressingMode::Accumulator {
                    let r = alu::lsr(self.regs.a, self.regs.p);
                    self.regs.a = r.value;
                    self.regs.p = r.status;
                } else {
                    let addr = self.effective_addr(decoded.mode, pc, false)?;
                    let val = self.bus_read(addr)?;
                    let r = alu::lsr(val, self.regs.p);
                    self.bus_write(addr, r.value)?;
                    self.regs.p = r.status;
                }
            }
            Mnemonic::Rol => {
                if decoded.mode == AddressingMode::Accumulator {
                    let r = alu::rol(self.regs.a, self.regs.p);
                    self.regs.a = r.value;
                    self.regs.p = r.status;
                } else {
                    let addr = self.effective_addr(decoded.mode, pc, false)?;
                    let val = self.bus_read(addr)?;
                    let r = alu::rol(val, self.regs.p);
                    self.bus_write(addr, r.value)?;
                    self.regs.p = r.status;
                }
            }
            Mnemonic::Ror => {
                if decoded.mode == AddressingMode::Accumulator {
                    let r = alu::ror(self.regs.a, self.regs.p);
                    self.regs.a = r.value;
                    self.regs.p = r.status;
                } else {
                    let addr = self.effective_addr(decoded.mode, pc, false)?;
                    let val = self.bus_read(addr)?;
                    let r = alu::ror(val, self.regs.p);
                    self.bus_write(addr, r.value)?;
                    self.regs.p = r.status;
                }
            }

            // --- Inc/Dec ---
            Mnemonic::Inc => {
                if decoded.mode == AddressingMode::Accumulator {
                    let r = alu::inc(self.regs.a, self.regs.p);
                    self.regs.a = r.value;
                    self.regs.p = r.status;
                } else {
                    let addr = self.effective_addr(decoded.mode, pc, false)?;
                    let val = self.bus_read(addr)?;
                    let r = alu::inc(val, self.regs.p);
                    self.bus_write(addr, r.value)?;
                    self.regs.p = r.status;
                }
            }
            Mnemonic::Dec => {
                if decoded.mode == AddressingMode::Accumulator {
                    let r = alu::dec(self.regs.a, self.regs.p);
                    self.regs.a = r.value;
                    self.regs.p = r.status;
                } else {
                    let addr = self.effective_addr(decoded.mode, pc, false)?;
                    let val = self.bus_read(addr)?;
                    let r = alu::dec(val, self.regs.p);
                    self.bus_write(addr, r.value)?;
                    self.regs.p = r.status;
                }
            }
            Mnemonic::Inx => { let r = alu::inc(self.regs.x, self.regs.p); self.regs.x = r.value; self.regs.p = r.status; }
            Mnemonic::Dex => { let r = alu::dec(self.regs.x, self.regs.p); self.regs.x = r.value; self.regs.p = r.status; }
            Mnemonic::Iny => { let r = alu::inc(self.regs.y, self.regs.p); self.regs.y = r.value; self.regs.p = r.status; }
            Mnemonic::Dey => { let r = alu::dec(self.regs.y, self.regs.p); self.regs.y = r.value; self.regs.p = r.status; }

            // --- Bit ---
            Mnemonic::Bit => {
                let (val, _) = self.read_operand(decoded.mode, pc)?;
                if decoded.mode == AddressingMode::Immediate {
                    self.regs.p = alu::bit_imm(self.regs.a, val, self.regs.p);
                } else {
                    self.regs.p = alu::bit_mem(self.regs.a, val, self.regs.p);
                }
            }
            Mnemonic::Trb => {
                let addr = self.effective_addr(decoded.mode, pc, false)?;
                let val = self.bus_read(addr)?;
                let r = alu::trb(self.regs.a, val, self.regs.p);
                self.bus_write(addr, r.value)?;
                self.regs.p = r.status;
            }
            Mnemonic::Tsb => {
                let addr = self.effective_addr(decoded.mode, pc, false)?;
                let val = self.bus_read(addr)?;
                let r = alu::tsb(self.regs.a, val, self.regs.p);
                self.bus_write(addr, r.value)?;
                self.regs.p = r.status;
            }

            // --- Flag ops ---
            Mnemonic::Clc => self.regs.p.remove(StatusRegister::C),
            Mnemonic::Sec => self.regs.p.insert(StatusRegister::C),
            Mnemonic::Cli => self.regs.p.remove(StatusRegister::I),
            Mnemonic::Sei => self.regs.p.insert(StatusRegister::I),
            Mnemonic::Cld => self.regs.p.remove(StatusRegister::D),
            Mnemonic::Sed => self.regs.p.insert(StatusRegister::D),
            Mnemonic::Clv => self.regs.p.remove(StatusRegister::V),

            // --- Jumps ---
            Mnemonic::Jmp => {
                let addr = self.effective_addr(decoded.mode, pc, false)?;
                self.regs.pc = addr;
            }
            Mnemonic::Jsr => {
                // PC is already advanced to pc+3 above; push pc+2 (return addr - 1)
                let ret = self.regs.pc.wrapping_sub(1);
                self.push((ret >> 8) as u8)?;
                self.push(ret as u8)?;
                let lo = self.bus_read(pc + 1)?;
                let hi = self.bus_read(pc + 2)?;
                self.regs.pc = u16::from_le_bytes([lo, hi]);
            }
            Mnemonic::Rts => {
                let lo = self.pop()?;
                let hi = self.pop()?;
                self.regs.pc = u16::from_le_bytes([lo, hi]).wrapping_add(1);
            }
            Mnemonic::Rti => {
                let p = self.pop()?;
                self.regs.p = StatusRegister::from_byte(p) | StatusRegister::UNUSED;
                let lo = self.pop()?;
                let hi = self.pop()?;
                self.regs.pc = u16::from_le_bytes([lo, hi]);
            }

            // --- Branches ---
            Mnemonic::Bra => extra_cycles += self.branch(true, pc)?,
            Mnemonic::Bcc => extra_cycles += self.branch(!self.regs.p.contains(StatusRegister::C), pc)?,
            Mnemonic::Bcs => extra_cycles += self.branch(self.regs.p.contains(StatusRegister::C), pc)?,
            Mnemonic::Beq => extra_cycles += self.branch(self.regs.p.contains(StatusRegister::Z), pc)?,
            Mnemonic::Bne => extra_cycles += self.branch(!self.regs.p.contains(StatusRegister::Z), pc)?,
            Mnemonic::Bmi => extra_cycles += self.branch(self.regs.p.contains(StatusRegister::N), pc)?,
            Mnemonic::Bpl => extra_cycles += self.branch(!self.regs.p.contains(StatusRegister::N), pc)?,
            Mnemonic::Bvc => extra_cycles += self.branch(!self.regs.p.contains(StatusRegister::V), pc)?,
            Mnemonic::Bvs => extra_cycles += self.branch(self.regs.p.contains(StatusRegister::V), pc)?,

            // --- BRK ---
            Mnemonic::Brk => {
                // BRK is 2 bytes; PC was already advanced to pc+2 above.
                // service_interrupt pushes the current PC (already at pc+2) and P with B set.
                self.service_interrupt(IRQ_VECTOR, true)?;
            }

            // --- NOP ---
            Mnemonic::Nop => {}

            // --- WDC-only: WAI / STP ---
            Mnemonic::Wai => { self.waiting = true; }
            Mnemonic::Stp => { self.stopped = true; }

            // --- WDC-only: RMB / SMB ---
            Mnemonic::Rmb0 => self.rmb(pc, 0)?,
            Mnemonic::Rmb1 => self.rmb(pc, 1)?,
            Mnemonic::Rmb2 => self.rmb(pc, 2)?,
            Mnemonic::Rmb3 => self.rmb(pc, 3)?,
            Mnemonic::Rmb4 => self.rmb(pc, 4)?,
            Mnemonic::Rmb5 => self.rmb(pc, 5)?,
            Mnemonic::Rmb6 => self.rmb(pc, 6)?,
            Mnemonic::Rmb7 => self.rmb(pc, 7)?,
            Mnemonic::Smb0 => self.smb(pc, 0)?,
            Mnemonic::Smb1 => self.smb(pc, 1)?,
            Mnemonic::Smb2 => self.smb(pc, 2)?,
            Mnemonic::Smb3 => self.smb(pc, 3)?,
            Mnemonic::Smb4 => self.smb(pc, 4)?,
            Mnemonic::Smb5 => self.smb(pc, 5)?,
            Mnemonic::Smb6 => self.smb(pc, 6)?,
            Mnemonic::Smb7 => self.smb(pc, 7)?,

            // --- WDC-only: BBR / BBS ---
            Mnemonic::Bbr0 => extra_cycles += self.bbr(pc, 0)?,
            Mnemonic::Bbr1 => extra_cycles += self.bbr(pc, 1)?,
            Mnemonic::Bbr2 => extra_cycles += self.bbr(pc, 2)?,
            Mnemonic::Bbr3 => extra_cycles += self.bbr(pc, 3)?,
            Mnemonic::Bbr4 => extra_cycles += self.bbr(pc, 4)?,
            Mnemonic::Bbr5 => extra_cycles += self.bbr(pc, 5)?,
            Mnemonic::Bbr6 => extra_cycles += self.bbr(pc, 6)?,
            Mnemonic::Bbr7 => extra_cycles += self.bbr(pc, 7)?,
            Mnemonic::Bbs0 => extra_cycles += self.bbs(pc, 0)?,
            Mnemonic::Bbs1 => extra_cycles += self.bbs(pc, 1)?,
            Mnemonic::Bbs2 => extra_cycles += self.bbs(pc, 2)?,
            Mnemonic::Bbs3 => extra_cycles += self.bbs(pc, 3)?,
            Mnemonic::Bbs4 => extra_cycles += self.bbs(pc, 4)?,
            Mnemonic::Bbs5 => extra_cycles += self.bbs(pc, 5)?,
            Mnemonic::Bbs6 => extra_cycles += self.bbs(pc, 6)?,
            Mnemonic::Bbs7 => extra_cycles += self.bbs(pc, 7)?,

            // ILL is caught by is_valid above; unreachable here.
            Mnemonic::Ill => {
                return Err(ExecError::InvalidOpcode { addr: pc, opcode: decoded.opcode });
            }
            // Bbc is not in the 65C02 opcode table — unreachable
            Mnemonic::Bbc => {
                return Err(ExecError::InvalidOpcode { addr: pc, opcode: decoded.opcode });
            }
        }

        let total_cycles = decoded.base_cycles + extra_cycles;
        Ok((StepResult::Executed(decoded), total_cycles))
    }

    // --- addressing mode resolution ---

    /// Resolves `mode` to an effective address, optionally detecting page-crossing.
    /// Returns `(addr, page_crossed_extra_cycles)`.
    fn effective_addr_with_penalty(
        &mut self,
        mode: AddressingMode,
        pc: u16,
        penalize_page_cross: bool,
    ) -> Result<(u16, u8), ExecError> {
        let addr = match mode {
            AddressingMode::ZeroPage => {
                self.bus_read(pc + 1)? as u16
            }
            AddressingMode::ZeroPageX => {
                let base = self.bus_read(pc + 1)?;
                base.wrapping_add(self.regs.x) as u16
            }
            AddressingMode::ZeroPageY => {
                let base = self.bus_read(pc + 1)?;
                base.wrapping_add(self.regs.y) as u16
            }
            AddressingMode::Absolute => {
                let lo = self.bus_read(pc + 1)?;
                let hi = self.bus_read(pc + 2)?;
                u16::from_le_bytes([lo, hi])
            }
            AddressingMode::AbsoluteX => {
                let lo = self.bus_read(pc + 1)?;
                let hi = self.bus_read(pc + 2)?;
                let base = u16::from_le_bytes([lo, hi]);
                let addr = base.wrapping_add(self.regs.x as u16);
                let xc = if penalize_page_cross && page_crossed(base, addr) { 1 } else { 0 };
                return Ok((addr, xc));
            }
            AddressingMode::AbsoluteY => {
                let lo = self.bus_read(pc + 1)?;
                let hi = self.bus_read(pc + 2)?;
                let base = u16::from_le_bytes([lo, hi]);
                let addr = base.wrapping_add(self.regs.y as u16);
                let xc = if penalize_page_cross && page_crossed(base, addr) { 1 } else { 0 };
                return Ok((addr, xc));
            }
            AddressingMode::Indirect => {
                let lo = self.bus_read(pc + 1)?;
                let hi = self.bus_read(pc + 2)?;
                let ptr = u16::from_le_bytes([lo, hi]);
                let alo = self.bus_read(ptr)?;
                let ahi = self.bus_read(ptr.wrapping_add(1))?;
                u16::from_le_bytes([alo, ahi])
            }
            AddressingMode::IndirectX => {
                let base = self.bus_read(pc + 1)?;
                let ptr = base.wrapping_add(self.regs.x) as u16;
                let alo = self.bus_read(ptr)?;
                let ahi = self.bus_read((ptr + 1) & 0x00FF)?;
                u16::from_le_bytes([alo, ahi])
            }
            AddressingMode::IndirectY => {
                let zp = self.bus_read(pc + 1)? as u16;
                let alo = self.bus_read(zp)?;
                let ahi = self.bus_read((zp + 1) & 0x00FF)?;
                let base = u16::from_le_bytes([alo, ahi]);
                let addr = base.wrapping_add(self.regs.y as u16);
                let xc = if penalize_page_cross && page_crossed(base, addr) { 1 } else { 0 };
                return Ok((addr, xc));
            }
            AddressingMode::ZeroPageIndirect => {
                let zp = self.bus_read(pc + 1)? as u16;
                let alo = self.bus_read(zp)?;
                let ahi = self.bus_read((zp + 1) & 0x00FF)?;
                u16::from_le_bytes([alo, ahi])
            }
            AddressingMode::AbsoluteIndirectX => {
                let lo = self.bus_read(pc + 1)?;
                let hi = self.bus_read(pc + 2)?;
                let base = u16::from_le_bytes([lo, hi]);
                let ptr = base.wrapping_add(self.regs.x as u16);
                let alo = self.bus_read(ptr)?;
                let ahi = self.bus_read(ptr.wrapping_add(1))?;
                u16::from_le_bytes([alo, ahi])
            }
            // These modes don't produce a simple address or are handled separately
            AddressingMode::Implied
            | AddressingMode::Accumulator
            | AddressingMode::Immediate
            | AddressingMode::Relative
            | AddressingMode::ZeroPageRelative => {
                return Err(ExecError::InvalidOpcode { addr: pc, opcode: 0 });
            }
        };
        Ok((addr, 0))
    }

    fn effective_addr(
        &mut self,
        mode: AddressingMode,
        pc: u16,
        penalize_page_cross: bool,
    ) -> Result<u16, ExecError> {
        Ok(self.effective_addr_with_penalty(mode, pc, penalize_page_cross)?.0)
    }

    /// Reads an 8-bit operand for the given mode. Returns `(value, extra_cycles)`.
    fn read_operand(
        &mut self,
        mode: AddressingMode,
        pc: u16,
    ) -> Result<(u8, u8), ExecError> {
        match mode {
            AddressingMode::Immediate => {
                Ok((self.bus_read(pc + 1)?, 0))
            }
            AddressingMode::Accumulator => Ok((self.regs.a, 0)),
            _ => {
                let (addr, xc) = self.effective_addr_with_penalty(mode, pc, true)?;
                Ok((self.bus_read(addr)?, xc))
            }
        }
    }

    // --- branch helper ---

    /// Executes a relative branch if `cond` is true. Returns extra cycles consumed.
    fn branch(&mut self, cond: bool, pc: u16) -> Result<u8, ExecError> {
        if !cond {
            return Ok(0);
        }
        let offset = self.bus_read(pc + 1)? as i8;
        // PC is already at pc+2 (after the 2-byte branch instruction)
        let target = self.regs.pc.wrapping_add(offset as u16);
        let page_extra = if page_crossed(self.regs.pc, target) { 1u8 } else { 0 };
        self.regs.pc = target;
        Ok(1 + page_extra)
    }

    // --- WDC bit-manipulation helpers ---

    fn rmb(&mut self, pc: u16, bit: u8) -> Result<(), ExecError> {
        let zp = self.bus_read(pc + 1)? as u16;
        let val = self.bus_read(zp)?;
        self.bus_write(zp, val & !(1 << bit))
    }

    fn smb(&mut self, pc: u16, bit: u8) -> Result<(), ExecError> {
        let zp = self.bus_read(pc + 1)? as u16;
        let val = self.bus_read(zp)?;
        self.bus_write(zp, val | (1 << bit))
    }

    fn bbr(&mut self, pc: u16, bit: u8) -> Result<u8, ExecError> {
        let zp = self.bus_read(pc + 1)? as u16;
        let val = self.bus_read(zp)?;
        if val & (1 << bit) == 0 {
            let offset = self.bus_read(pc + 2)? as i8;
            let target = self.regs.pc.wrapping_add(offset as u16);
            self.regs.pc = target;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    fn bbs(&mut self, pc: u16, bit: u8) -> Result<u8, ExecError> {
        let zp = self.bus_read(pc + 1)? as u16;
        let val = self.bus_read(zp)?;
        if val & (1 << bit) != 0 {
            let offset = self.bus_read(pc + 2)? as i8;
            let target = self.regs.pc.wrapping_add(offset as u16);
            self.regs.pc = target;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    // --- interrupt helpers ---

    /// Pushes PC and P, reads the vector at `vector_addr`, and sets the I flag.
    /// Returns the cycle count for the interrupt sequence (7 cycles).
    fn service_interrupt(&mut self, vector_addr: u16, is_brk: bool) -> Result<u8, ExecError> {
        let pc = self.regs.pc;
        self.push((pc >> 8) as u8)?;
        self.push(pc as u8)?;
        let p = if is_brk {
            (self.regs.p | StatusRegister::B | StatusRegister::UNUSED).to_byte()
        } else {
            (self.regs.p & !StatusRegister::B | StatusRegister::UNUSED).to_byte()
        };
        self.push(p)?;
        self.regs.p.insert(StatusRegister::I);
        self.regs.p.remove(StatusRegister::D);
        let lo = self.bus_read(vector_addr)?;
        let hi = self.bus_read(vector_addr + 1)?;
        self.regs.pc = u16::from_le_bytes([lo, hi]);
        Ok(7)
    }

    /// Polls all devices and syncs their IRQ and NMI state into the interrupt controller.
    fn poll_interrupts(&mut self) {
        let states: Vec<_> = self.bus.device_irq_states();
        self.interrupts.poll_devices(states.into_iter());
        if self.bus.take_device_nmi() {
            self.interrupts.signal_nmi();
        }
    }

    // --- status flag helpers ---

    fn set_nz(&mut self, val: u8) {
        self.regs.p.set(StatusRegister::N, val & 0x80 != 0);
        self.regs.p.set(StatusRegister::Z, val == 0);
    }

    // --- bus helpers ---

    fn bus_read(&mut self, addr: u16) -> Result<u8, ExecError> {
        self.bus.read(addr).map_err(|e| match e {
            BusError::Unmapped { addr } => ExecError::UnmappedAddress { addr, op: BusOp::Read },
            BusError::RomWrite { addr } => ExecError::UnmappedAddress { addr, op: BusOp::Read },
        })
    }

    fn bus_write(&mut self, addr: u16, value: u8) -> Result<(), ExecError> {
        self.bus.write(addr, value).map_err(|e| match e {
            BusError::Unmapped { addr } => ExecError::UnmappedAddress { addr, op: BusOp::Write },
            BusError::RomWrite { addr } => ExecError::RomWrite { addr, value },
        })
    }

    fn bus_reset(&mut self) {
        self.bus.reset_devices();
    }

    // --- stack helpers ---

    fn push(&mut self, value: u8) -> Result<(), ExecError> {
        let addr = STACK_BASE | self.regs.s as u16;
        self.bus_write(addr, value)?;
        self.regs.s = self.regs.s.wrapping_sub(1);
        Ok(())
    }

    fn pop(&mut self) -> Result<u8, ExecError> {
        self.regs.s = self.regs.s.wrapping_add(1);
        let addr = STACK_BASE | self.regs.s as u16;
        self.bus_read(addr)
    }
}

fn page_crossed(base: u16, addr: u16) -> bool {
    (base & 0xFF00) != (addr & 0xFF00)
}

/// Register IDs used by `CpuWatchContext` and returned by `map_register_name`.
const REG_A: Operand  = 0;
const REG_X: Operand  = 1;
const REG_Y: Operand  = 2;
const REG_P: Operand  = 3;
const REG_S: Operand  = 4;
const REG_PC: Operand = 5;

/// Flag IDs used by `CpuWatchContext` and returned by `map_flag_name`.
/// Each ID is the bit mask of the flag in the status register.
const FLAG_C: Operand = 0x01;
const FLAG_Z: Operand = 0x02;
const FLAG_I: Operand = 0x04;
const FLAG_D: Operand = 0x08;
const FLAG_B: Operand = 0x10;
const FLAG_V: Operand = 0x40;
const FLAG_N: Operand = 0x80;

/// Maps a register name to its `Operand` ID for use with `WatchCompiler`.
///
/// Accepts upper- and lower-case names: `A`/`a`, `X`/`x`, `Y`/`y`, `P`/`p`,
/// `S`/`s`, and `PC`/`pc`. Returns `None` for unrecognized names.
pub fn map_register_name(name: &str) -> Option<Operand> {
    match name {
        "A" | "a" => Some(REG_A),
        "X" | "x" => Some(REG_X),
        "Y" | "y" => Some(REG_Y),
        "P" | "p" => Some(REG_P),
        "S" | "s" => Some(REG_S),
        "PC" | "pc" | "Pc" | "pC" => Some(REG_PC),
        _ => None,
    }
}

/// Maps a flag name to its `Operand` ID (bit mask in P) for use with `WatchCompiler`.
///
/// Accepts upper- and lower-case names: `C`/`c`, `Z`/`z`, `I`/`i`, `D`/`d`,
/// `B`/`b`, `V`/`v`, `N`/`n`. Returns `None` for unrecognized names.
pub fn map_flag_name(name: &str) -> Option<Operand> {
    match name {
        "C" | "c" => Some(FLAG_C),
        "Z" | "z" => Some(FLAG_Z),
        "I" | "i" => Some(FLAG_I),
        "D" | "d" => Some(FLAG_D),
        "B" | "b" => Some(FLAG_B),
        "V" | "v" => Some(FLAG_V),
        "N" | "n" => Some(FLAG_N),
        _ => None,
    }
}

/// Borrows CPU state to implement `WatchContext` with side-effect-free memory reads.
struct CpuWatchContext<'a> {
    regs: &'a Registers,
    bus: &'a Bus,
}

impl WatchContext for CpuWatchContext<'_> {
    fn read_register_u32(&self, id: Operand) -> Operand {
        match id {
            REG_A  => self.regs.a as Operand,
            REG_X  => self.regs.x as Operand,
            REG_Y  => self.regs.y as Operand,
            REG_P  => self.regs.p.to_byte() as Operand,
            REG_S  => self.regs.s as Operand,
            REG_PC => self.regs.pc as Operand,
            _      => 0,
        }
    }

    fn read_register_i32(&self, id: Operand) -> Operand {
        match id {
            REG_A  => (self.regs.a as i8) as u32,
            REG_X  => (self.regs.x as i8) as u32,
            REG_Y  => (self.regs.y as i8) as u32,
            REG_P  => (self.regs.p.to_byte() as i8) as u32,
            REG_S  => (self.regs.s as i8) as u32,
            REG_PC => (self.regs.pc as i16) as u32,
            _      => 0,
        }
    }

    fn read_flag(&self, flag_id: Operand) -> Operand {
        (self.regs.p.to_byte() as Operand & flag_id != 0) as Operand
    }

    fn read_mem_u32(&self, addr: u16, width: u8) -> u32 {
        match width {
            1 => self.bus.peek(addr).unwrap_or(0) as u32,
            2 => {
                let lo = self.bus.peek(addr).unwrap_or(0) as u32;
                let hi = self.bus.peek(addr.wrapping_add(1)).unwrap_or(0) as u32;
                lo | (hi << 8)
            }
            4 => {
                let mut val = 0u32;
                for i in 0..4u16 {
                    let b = self.bus.peek(addr.wrapping_add(i)).unwrap_or(0) as u32;
                    val |= b << (i * 8);
                }
                val
            }
            _ => 0,
        }
    }

    fn read_mem_i32(&self, addr: u16, width: u8) -> u32 {
        match width {
            1 => (self.bus.peek(addr).unwrap_or(0) as i8) as u32,
            2 => {
                let lo = self.bus.peek(addr).unwrap_or(0) as u16;
                let hi = self.bus.peek(addr.wrapping_add(1)).unwrap_or(0) as u16;
                ((lo | (hi << 8)) as i16) as u32
            }
            4 => self.read_mem_u32(addr, 4),
            _ => 0,
        }
    }
}

/// Builder for `Cpu`.
pub struct CpuBuilder {
    variant: CpuVariant,
    invalid_opcode_policy: InvalidOpcodePolicy,
    clock_speed: ClockSpeed,
    bus: Option<Bus>,
}

impl CpuBuilder {
    /// Creates a new builder for the given CPU variant.
    pub fn new(variant: CpuVariant) -> Self {
        Self {
            variant,
            invalid_opcode_policy: InvalidOpcodePolicy::Nop,
            clock_speed: ClockSpeed::unlimited(),
            bus: None,
        }
    }

    /// Sets the invalid-opcode handling policy.
    pub fn invalid_opcode_policy(mut self, policy: InvalidOpcodePolicy) -> Self {
        self.invalid_opcode_policy = policy;
        self
    }

    /// Sets the target clock speed.
    pub fn clock_speed(mut self, speed: ClockSpeed) -> Self {
        self.clock_speed = speed;
        self
    }

    /// Provides the memory bus.
    pub fn bus(mut self, bus: Bus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Consumes the builder and returns a `Cpu`, or an error if required fields are missing.
    pub fn build(self) -> Result<Cpu, CpuBuildError> {
        let bus = self.bus.ok_or(CpuBuildError::NoBus)?;
        let table = decode_table(self.variant);
        Ok(Cpu {
            regs: Registers::new(),
            bus,
            interrupts: InterruptController::new(),
            evaluator: WatchEvaluator::new(),
            breakpoints: HashSet::new(),
            table,
            variant: self.variant,
            invalid_opcode_policy: self.invalid_opcode_policy,
            clock_speed: self.clock_speed,
            cycles: 0,
            waiting: false,
            stopped: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::bus::Bus;
    use crate::emulator::bus::AddressRange;

    // Build a CPU with 64KB RAM and a reset vector pointing to `start`.
    fn make_cpu(start: u16) -> Cpu {
        let mut bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        bus.write(RESET_VECTOR, (start & 0xFF) as u8).unwrap();
        bus.write(RESET_VECTOR + 1, (start >> 8) as u8).unwrap();
        let mut cpu = Cpu::builder(CpuVariant::Wdc65C02)
            .bus(bus)
            .build()
            .unwrap();
        cpu.reset().unwrap();
        cpu
    }

    fn write_program(cpu: &mut Cpu, addr: u16, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            cpu.bus.write(addr + i as u16, b).unwrap();
        }
    }

    // --- reset ---

    #[test]
    fn reset_reads_vector() {
        let cpu = make_cpu(0x0400);
        assert_eq!(cpu.regs.pc, 0x0400);
        assert!(cpu.regs.p.contains(StatusRegister::I));
        assert_eq!(cpu.regs.s, 0xFF);
    }

    // --- addressing modes ---

    #[test]
    fn zeropage_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0042, 0xAB).unwrap();
        write_program(&mut cpu, 0x0200, &[0xA5, 0x42]); // LDA $42
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0xAB);
    }

    #[test]
    fn zeropage_x_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x05;
        cpu.bus.write(0x0047, 0xCC).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB5, 0x42]); // LDA $42,X
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0xCC);
    }

    #[test]
    fn zeropage_y_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.y = 0x03;
        cpu.bus.write(0x0045, 0x77).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB6, 0x42]); // LDX $42,Y
        cpu.step(None);
        assert_eq!(cpu.regs.x, 0x77);
    }

    #[test]
    fn absolute_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x1234, 0x99).unwrap();
        write_program(&mut cpu, 0x0200, &[0xAD, 0x34, 0x12]); // LDA $1234
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x99);
    }

    #[test]
    fn absolute_x_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x10;
        cpu.bus.write(0x1244, 0x55).unwrap();
        write_program(&mut cpu, 0x0200, &[0xBD, 0x34, 0x12]); // LDA $1234,X
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x55);
    }

    #[test]
    fn absolute_y_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.y = 0x04;
        cpu.bus.write(0x1238, 0x44).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB9, 0x34, 0x12]); // LDA $1234,Y
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x44);
    }

    #[test]
    fn indirect_mode() {
        let mut cpu = make_cpu(0x0200);
        // JMP ($0300): ptr at $0300/$0301 holds $0400
        cpu.bus.write(0x0300, 0x00).unwrap();
        cpu.bus.write(0x0301, 0x04).unwrap();
        write_program(&mut cpu, 0x0200, &[0x6C, 0x00, 0x03]); // JMP ($0300)
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0400);
    }

    #[test]
    fn indirect_x_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x04;
        // (indirect,X): zp+X = $10, ptr at $10/$11 holds $0500
        cpu.bus.write(0x0010, 0x00).unwrap();
        cpu.bus.write(0x0011, 0x05).unwrap();
        cpu.bus.write(0x0500, 0xBB).unwrap();
        write_program(&mut cpu, 0x0200, &[0xA1, 0x0C]); // LDA ($0C,X)
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0xBB);
    }

    #[test]
    fn indirect_y_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.y = 0x02;
        // (indirect),Y: ptr at $10/$11 holds base $0500, +Y = $0502
        cpu.bus.write(0x0010, 0x00).unwrap();
        cpu.bus.write(0x0011, 0x05).unwrap();
        cpu.bus.write(0x0502, 0xDD).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB1, 0x10]); // LDA ($10),Y
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0xDD);
    }

    #[test]
    fn zeropage_indirect_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0020, 0x00).unwrap();
        cpu.bus.write(0x0021, 0x06).unwrap();
        cpu.bus.write(0x0600, 0xEE).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB2, 0x20]); // LDA ($20)
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0xEE);
    }

    #[test]
    fn absolute_indirect_x_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x02;
        // JMP ($0300,X): ptr at $0302 holds $0500
        cpu.bus.write(0x0302, 0x00).unwrap();
        cpu.bus.write(0x0303, 0x05).unwrap();
        write_program(&mut cpu, 0x0200, &[0x7C, 0x00, 0x03]); // JMP ($0300,X)
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0500);
    }

    #[test]
    fn page_crossing_adds_cycle() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0xFF;
        // LDA $0201,X crosses from page 02 to page 03
        cpu.bus.write(0x0300, 0x42).unwrap();
        write_program(&mut cpu, 0x0200, &[0xBD, 0x01, 0x02]); // LDA $0201,X
        let cycles_before = cpu.cycles;
        cpu.step(None);
        // base is 4, +1 for page cross
        assert_eq!(cpu.cycles - cycles_before, 5);
    }

    // --- loads/stores ---

    #[test]
    fn lda_immediate_sets_nz() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xA9, 0x00]); // LDA #$00
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x00);
        assert!(cpu.regs.p.contains(StatusRegister::Z));
        assert!(!cpu.regs.p.contains(StatusRegister::N));
    }

    #[test]
    fn lda_negative_sets_n() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xA9, 0x80]); // LDA #$80
        cpu.step(None);
        assert!(cpu.regs.p.contains(StatusRegister::N));
    }

    #[test]
    fn sta_stores_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x42;
        write_program(&mut cpu, 0x0200, &[0x85, 0x50]); // STA $50
        cpu.step(None);
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0x42);
    }

    #[test]
    fn stz_stores_zero() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0xFF).unwrap();
        write_program(&mut cpu, 0x0200, &[0x64, 0x50]); // STZ $50
        cpu.step(None);
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0x00);
    }

    // --- transfers ---

    #[test]
    fn tax_transfer() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x42;
        write_program(&mut cpu, 0x0200, &[0xAA]); // TAX
        cpu.step(None);
        assert_eq!(cpu.regs.x, 0x42);
    }

    #[test]
    fn txs_does_not_set_flags() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x00;
        cpu.regs.p.remove(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0x9A]); // TXS
        cpu.step(None);
        assert_eq!(cpu.regs.s, 0x00);
        assert!(!cpu.regs.p.contains(StatusRegister::Z)); // TXS doesn't touch flags
    }

    // --- stack ops ---

    #[test]
    fn pha_pla_round_trip() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0xBE;
        write_program(&mut cpu, 0x0200, &[0x48, 0x68]); // PHA, PLA
        cpu.step(None); // PHA
        cpu.regs.a = 0x00;
        cpu.step(None); // PLA
        assert_eq!(cpu.regs.a, 0xBE);
    }

    #[test]
    fn php_plp_round_trip() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p = StatusRegister::N | StatusRegister::C | StatusRegister::UNUSED;
        write_program(&mut cpu, 0x0200, &[0x08, 0x28]); // PHP, PLP
        cpu.step(None);
        cpu.regs.p = StatusRegister::empty();
        cpu.step(None);
        assert!(cpu.regs.p.contains(StatusRegister::N));
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    #[test]
    fn phx_phy_plx_ply() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x12;
        cpu.regs.y = 0x34;
        write_program(&mut cpu, 0x0200, &[0xDA, 0x5A, 0x7A, 0xFA]); // PHX PHY PLY PLX
        cpu.step(None); // PHX
        cpu.step(None); // PHY
        cpu.regs.y = 0;
        cpu.step(None); // PLY
        assert_eq!(cpu.regs.y, 0x34);
        cpu.regs.x = 0;
        cpu.step(None); // PLX
        assert_eq!(cpu.regs.x, 0x12);
    }

    // --- branches ---

    #[test]
    fn bra_always_branches() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0x80, 0x02]); // BRA +2
        cpu.step(None);
        // PC was 0x0202 (after fetch), branch +2 → 0x0204
        assert_eq!(cpu.regs.pc, 0x0204);
    }

    #[test]
    fn bne_not_taken_when_zero() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p.insert(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0xD0, 0x10]); // BNE +16
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0202); // not taken
    }

    #[test]
    fn beq_taken_when_zero() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p.insert(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0xF0, 0x10]); // BEQ +16
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0212);
    }

    #[test]
    fn branch_backward() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0x80, 0xFE_u8]); // BRA -2 → loops to self
        let pc_before = cpu.regs.pc;
        cpu.step(None);
        assert_eq!(cpu.regs.pc, pc_before); // back to 0x0200
    }

    // --- jumps ---

    #[test]
    fn jsr_rts_round_trip() {
        let mut cpu = make_cpu(0x0200);
        // JSR $0300; at $0300: RTS
        write_program(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write_program(&mut cpu, 0x0300, &[0x60]);              // RTS
        cpu.step(None); // JSR
        assert_eq!(cpu.regs.pc, 0x0300);
        cpu.step(None); // RTS
        assert_eq!(cpu.regs.pc, 0x0203); // return to instruction after JSR
    }

    // --- BRK ---

    #[test]
    fn brk_pushes_pc_and_p_reads_irq_vector() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(IRQ_VECTOR, 0x00).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0x04).unwrap();
        cpu.regs.p = StatusRegister::UNUSED;
        write_program(&mut cpu, 0x0200, &[0x00, 0xEA]); // BRK (pad byte)
        let s_before = cpu.regs.s;
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0400);
        // 3 bytes pushed (PC hi, PC lo, P)
        assert_eq!(cpu.regs.s, s_before.wrapping_sub(3));
        // B and UNUSED set in pushed P
        let pushed_p = cpu.bus.read(STACK_BASE | s_before.wrapping_sub(2) as u16).unwrap();
        assert!(pushed_p & StatusRegister::B.bits() != 0);
    }

    // --- RTI ---

    #[test]
    fn rti_restores_flags_and_pc() {
        let mut cpu = make_cpu(0x0200);
        // Manually push: PC=$0300 (hi then lo), P=$C5
        let s = cpu.regs.s;
        cpu.bus.write(STACK_BASE | s as u16, 0x03).unwrap();       // PC hi
        cpu.bus.write(STACK_BASE | s.wrapping_sub(1) as u16, 0x00).unwrap(); // PC lo
        cpu.bus.write(STACK_BASE | s.wrapping_sub(2) as u16, 0xC5).unwrap(); // P
        cpu.regs.s = s.wrapping_sub(3);
        write_program(&mut cpu, 0x0200, &[0x40]); // RTI
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0300);
        assert!(cpu.regs.p.contains(StatusRegister::N));
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    // --- arithmetic ---

    #[test]
    fn adc_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x10;
        cpu.regs.p.remove(StatusRegister::C);
        write_program(&mut cpu, 0x0200, &[0x69, 0x20]); // ADC #$20
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x30);
    }

    #[test]
    fn sbc_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x50;
        cpu.regs.p.insert(StatusRegister::C); // no borrow
        write_program(&mut cpu, 0x0200, &[0xE9, 0x10]); // SBC #$10
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x40);
    }

    // --- logic ---

    #[test]
    fn and_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0xFF;
        write_program(&mut cpu, 0x0200, &[0x29, 0x0F]); // AND #$0F
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x0F);
    }

    #[test]
    fn ora_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x0F;
        write_program(&mut cpu, 0x0200, &[0x09, 0xF0]); // ORA #$F0
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0xFF);
    }

    #[test]
    fn eor_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0xFF;
        write_program(&mut cpu, 0x0200, &[0x49, 0xFF]); // EOR #$FF
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x00);
        assert!(cpu.regs.p.contains(StatusRegister::Z));
    }

    // --- shifts ---

    #[test]
    fn asl_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x41;
        write_program(&mut cpu, 0x0200, &[0x0A]); // ASL A
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x82);
        assert!(!cpu.regs.p.contains(StatusRegister::C));
        assert!(cpu.regs.p.contains(StatusRegister::N));
    }

    #[test]
    fn lsr_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x03;
        write_program(&mut cpu, 0x0200, &[0x4A]); // LSR A
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x01);
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    #[test]
    fn rol_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x80;
        cpu.regs.p.insert(StatusRegister::C);
        write_program(&mut cpu, 0x0200, &[0x2A]); // ROL A
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x01);
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    #[test]
    fn ror_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x01;
        cpu.regs.p.insert(StatusRegister::C);
        write_program(&mut cpu, 0x0200, &[0x6A]); // ROR A
        cpu.step(None);
        assert_eq!(cpu.regs.a, 0x80);
        assert!(cpu.regs.p.contains(StatusRegister::C));
        assert!(cpu.regs.p.contains(StatusRegister::N));
    }

    // --- flag manipulation ---

    #[test]
    fn clc_sec() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p.insert(StatusRegister::C);
        write_program(&mut cpu, 0x0200, &[0x18, 0x38]); // CLC, SEC
        cpu.step(None);
        assert!(!cpu.regs.p.contains(StatusRegister::C));
        cpu.step(None);
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    // --- invalid opcode ---

    #[test]
    fn invalid_opcode_nop_policy_advances_pc() {
        let mut cpu = make_cpu(0x0200);
        // $CB is WAI — valid only on WDC; invalid (1 byte) on Cmos65C02
        write_program(&mut cpu, 0x0200, &[0xCB]);
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0201);
    }

    #[test]
    fn invalid_opcode_error_policy() {
        let mut bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        bus.write(RESET_VECTOR, 0x00).unwrap();
        bus.write(RESET_VECTOR + 1, 0x02).unwrap();
        bus.write(0x0200, 0xCB).unwrap(); // WAI — invalid on Cmos65C02 variant
        let mut cpu = Cpu::builder(CpuVariant::Cmos65C02)
            .invalid_opcode_policy(InvalidOpcodePolicy::Error)
            .bus(bus)
            .build()
            .unwrap();
        cpu.reset().unwrap();
        assert!(matches!(cpu.step(None), StepResult::Error(ExecError::InvalidOpcode { .. })));
    }

    // --- WAI / STP ---

    #[test]
    fn wai_returns_waiting() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xCB]); // WAI
        cpu.step(None);
        assert!(matches!(cpu.step(None), StepResult::Waiting));
    }

    #[test]
    fn stp_returns_stopped() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xDB]); // STP
        cpu.step(None);
        assert!(matches!(cpu.step(None), StepResult::Stopped));
    }

    // --- WDC: RMB / SMB ---

    #[test]
    fn rmb_clears_bit() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0xFF).unwrap();
        write_program(&mut cpu, 0x0200, &[0x07, 0x50]); // RMB0 $50
        cpu.step(None);
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0xFE);
    }

    #[test]
    fn smb_sets_bit() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x00).unwrap();
        write_program(&mut cpu, 0x0200, &[0x87, 0x50]); // SMB0 $50
        cpu.step(None);
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0x01);
    }

    // --- WDC: BBR / BBS ---

    #[test]
    fn bbr_branches_when_bit_clear() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0xFE).unwrap(); // bit 0 clear
        // BBR0 $50, +4
        write_program(&mut cpu, 0x0200, &[0x0F, 0x50, 0x04]);
        cpu.step(None);
        // PC was 0x0203 after fetch, +4 = 0x0207
        assert_eq!(cpu.regs.pc, 0x0207);
    }

    #[test]
    fn bbr_not_taken_when_bit_set() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x01).unwrap(); // bit 0 set
        write_program(&mut cpu, 0x0200, &[0x0F, 0x50, 0x04]);
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0203); // not taken
    }

    #[test]
    fn bbs_branches_when_bit_set() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x01).unwrap(); // bit 0 set
        // BBS0 $50, +4
        write_program(&mut cpu, 0x0200, &[0x8F, 0x50, 0x04]);
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0207);
    }

    // --- device tick ---

    #[test]
    fn tick_called_with_cycle_count() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP = 2 cycles
        cpu.step(None);
        assert_eq!(cpu.cycles(), 2);
    }

    // --- cycles accumulate ---

    #[test]
    fn cycles_accumulate_over_steps() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA, 0xEA, 0xEA]); // 3x NOP
        cpu.step(None);
        cpu.step(None);
        cpu.step(None);
        assert_eq!(cpu.cycles(), 6);
    }

    // --- IRQ ---

    #[test]
    fn irq_with_i_clear_vectors_through_irq_vector() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(IRQ_VECTOR, 0x00).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0x04).unwrap();
        cpu.regs.p.remove(StatusRegister::I);
        cpu.interrupts_mut().assert_irq(crate::emulator::bus::IrqSource(1));
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0400);
        assert!(cpu.regs.p.contains(StatusRegister::I));
    }

    #[test]
    fn irq_with_i_set_does_not_vector() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.regs.p.insert(StatusRegister::I);
        cpu.interrupts_mut().assert_irq(crate::emulator::bus::IrqSource(1));
        cpu.step(None);
        // NOP executes normally; PC advances past it
        assert_eq!(cpu.regs.pc, 0x0201);
    }

    #[test]
    fn irq_pushes_correct_state() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(IRQ_VECTOR, 0x00).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0x04).unwrap();
        cpu.regs.p = StatusRegister::UNUSED | StatusRegister::C; // I clear, C set
        let s_before = cpu.regs.s;
        cpu.interrupts_mut().assert_irq(crate::emulator::bus::IrqSource(1));
        cpu.step(None);
        // 3 bytes pushed: PC hi, PC lo, P
        assert_eq!(cpu.regs.s, s_before.wrapping_sub(3));
        // Pushed PC should be 0x0200 (PC at time of IRQ)
        let pushed_pc_hi = cpu.bus.read(STACK_BASE | s_before as u16).unwrap();
        let pushed_pc_lo = cpu.bus.read(STACK_BASE | s_before.wrapping_sub(1) as u16).unwrap();
        assert_eq!(u16::from_le_bytes([pushed_pc_lo, pushed_pc_hi]), 0x0200);
        // Pushed P should not have B set
        let pushed_p = cpu.bus.read(STACK_BASE | s_before.wrapping_sub(2) as u16).unwrap();
        assert_eq!(pushed_p & StatusRegister::B.bits(), 0);
    }

    #[test]
    fn multi_source_irq_stays_active_after_partial_release() {
        let mut cpu = make_cpu(0x0200);
        use crate::emulator::bus::IrqSource;
        cpu.interrupts_mut().assert_irq(IrqSource(1));
        cpu.interrupts_mut().assert_irq(IrqSource(2));
        cpu.interrupts_mut().release_irq(IrqSource(1));
        assert!(cpu.interrupts().irq_active());
        cpu.interrupts_mut().release_irq(IrqSource(2));
        assert!(!cpu.interrupts().irq_active());
    }

    // --- NMI ---

    #[test]
    fn nmi_vectors_through_nmi_vector() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(NMI_VECTOR, 0x00).unwrap();
        cpu.bus.write(NMI_VECTOR + 1, 0x03).unwrap();
        cpu.regs.p.insert(StatusRegister::I); // I set — NMI ignores it
        cpu.interrupts_mut().signal_nmi();
        cpu.step(None);
        assert_eq!(cpu.regs.pc, 0x0300);
        assert!(cpu.regs.p.contains(StatusRegister::I));
    }

    #[test]
    fn nmi_pushes_correct_state() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(NMI_VECTOR, 0x00).unwrap();
        cpu.bus.write(NMI_VECTOR + 1, 0x03).unwrap();
        cpu.regs.p = StatusRegister::UNUSED | StatusRegister::C;
        let s_before = cpu.regs.s;
        cpu.interrupts_mut().signal_nmi();
        cpu.step(None);
        assert_eq!(cpu.regs.s, s_before.wrapping_sub(3));
        // Pushed P should not have B set
        let pushed_p = cpu.bus.read(STACK_BASE | s_before.wrapping_sub(2) as u16).unwrap();
        assert_eq!(pushed_p & StatusRegister::B.bits(), 0);
    }

    #[test]
    fn nmi_has_priority_over_simultaneous_irq() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(NMI_VECTOR, 0x00).unwrap();
        cpu.bus.write(NMI_VECTOR + 1, 0x03).unwrap();
        cpu.bus.write(IRQ_VECTOR, 0x00).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0x04).unwrap();
        cpu.regs.p.remove(StatusRegister::I);
        cpu.interrupts_mut().signal_nmi();
        cpu.interrupts_mut().assert_irq(crate::emulator::bus::IrqSource(1));
        cpu.step(None);
        // Should vector through NMI, not IRQ
        assert_eq!(cpu.regs.pc, 0x0300);
    }

    // --- WAI ---

    #[test]
    fn wai_returns_waiting_until_irq() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(IRQ_VECTOR, 0x00).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0x04).unwrap();
        cpu.regs.p.remove(StatusRegister::I);
        write_program(&mut cpu, 0x0200, &[0xCB]); // WAI
        cpu.step(None); // execute WAI — sets waiting=true
        assert!(matches!(cpu.step(None), StepResult::Waiting)); // no interrupt yet
        cpu.interrupts_mut().assert_irq(crate::emulator::bus::IrqSource(1));
        cpu.step(None); // wakes and services IRQ
        assert_eq!(cpu.regs.pc, 0x0400);
        assert!(!cpu.is_waiting());
    }

    #[test]
    fn wai_wakes_on_nmi() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(NMI_VECTOR, 0x00).unwrap();
        cpu.bus.write(NMI_VECTOR + 1, 0x03).unwrap();
        cpu.regs.p.insert(StatusRegister::I); // I set — NMI still wakes
        write_program(&mut cpu, 0x0200, &[0xCB]); // WAI
        cpu.step(None); // execute WAI
        cpu.interrupts_mut().signal_nmi();
        cpu.step(None); // wakes and services NMI
        assert_eq!(cpu.regs.pc, 0x0300);
        assert!(!cpu.is_waiting());
    }

    // --- CpuWatchContext reads ---

    // Compiles `expr` as a single watchpoint, steps once (NOP at $0200), and
    // returns the StepResult. The watch fires before instruction execution, so
    // the instruction at $0200 is never fetched.
    fn watch_step(cpu: &mut Cpu, expr: &str) -> StepResult {
        let mut compiler = make_compiler();
        let wp = compiler.compile(expr, cpu.evaluator_mut()).unwrap();
        cpu.evaluator_mut().add(wp);
        cpu.step(None)
    }

    #[test]
    fn watch_context_reads_register_a() {
        let mut cpu = make_cpu(0x0200);
        cpu.registers_mut().a = 0x42;
        assert!(matches!(watch_step(&mut cpu, "A == $42"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_register_x() {
        let mut cpu = make_cpu(0x0200);
        cpu.registers_mut().x = 0x05;
        assert!(matches!(watch_step(&mut cpu, "X == 5"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_register_y() {
        let mut cpu = make_cpu(0x0200);
        cpu.registers_mut().y = 0x10;
        assert!(matches!(watch_step(&mut cpu, "Y == $10"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_register_p() {
        // After reset: P = UNUSED | I = 0x24.
        let mut cpu = make_cpu(0x0200);
        assert!(matches!(watch_step(&mut cpu, "P == $24"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_register_s() {
        // After reset: S = 0xFF.
        let mut cpu = make_cpu(0x0200);
        assert!(matches!(watch_step(&mut cpu, "S == $FF"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_register_pc() {
        let mut cpu = make_cpu(0x0200);
        assert!(matches!(watch_step(&mut cpu, "PC == $200"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_register_a_signed() {
        // A = 0x80 → signed read gives -128 → as u32 = 0xFFFFFF80.
        let mut cpu = make_cpu(0x0200);
        cpu.registers_mut().a = 0x80;
        assert!(matches!(watch_step(&mut cpu, "+A == $FFFFFF80"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_flag() {
        let mut cpu = make_cpu(0x0200);
        cpu.registers_mut().p.insert(StatusRegister::C);
        assert!(matches!(watch_step(&mut cpu, "`C == 1"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_mem_byte() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus_mut().write(0x0050, 0xAA).unwrap();
        assert!(matches!(watch_step(&mut cpu, "B[$50] == $AA"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_mem_byte_signed() {
        // 0xAA as i8 = -86; sign-extended to u32 = 0xFFFFFFAA.
        let mut cpu = make_cpu(0x0200);
        cpu.bus_mut().write(0x0050, 0xAA).unwrap();
        assert!(matches!(watch_step(&mut cpu, "+b[$50] == $FFFFFFAA"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_mem_word() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus_mut().write(0x0050, 0x55).unwrap();
        cpu.bus_mut().write(0x0051, 0xAA).unwrap();
        assert!(matches!(watch_step(&mut cpu, "W[$50] == $AA55"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_mem_word_wraps() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus_mut().write(0xFFFF, 0x55).unwrap();
        cpu.bus_mut().write(0x0000, 0xAA).unwrap();
        assert!(matches!(watch_step(&mut cpu, "W[$FFFF] == $AA55"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_mem_dword() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus_mut().write(0x0050, 0x55).unwrap();
        cpu.bus_mut().write(0x0051, 0xAA).unwrap();
        cpu.bus_mut().write(0x0052, 0x55).unwrap();
        cpu.bus_mut().write(0x0053, 0xAA).unwrap();
        assert!(matches!(watch_step(&mut cpu, "D[$50] == $AA55AA55"), StepResult::WatchTriggered { .. }));
    }

    #[test]
    fn watch_context_reads_mem_dword_wraps() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus_mut().write(0xFFFE, 0x55).unwrap();
        cpu.bus_mut().write(0xFFFF, 0xAA).unwrap();
        cpu.bus_mut().write(0x0000, 0x55).unwrap();
        cpu.bus_mut().write(0x0001, 0xAA).unwrap();
        assert!(matches!(watch_step(&mut cpu, "D[$FFFE] == $AA55AA55"), StepResult::WatchTriggered { .. }));
    }

    // --- STP ---

    #[test]
    fn stp_cleared_by_reset() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xDB]); // STP
        cpu.step(None); // execute STP
        assert!(cpu.is_stopped());
        cpu.reset().unwrap();
        assert!(!cpu.is_stopped());
    }

    // --- breakpoints ---

    #[test]
    fn breakpoint_at_pc_returns_breakpoint_before_execution() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.add_breakpoint(0x0200);
        let result = cpu.step(None);
        assert!(matches!(result, StepResult::Breakpoint(0x0200)));
        // Instruction must NOT have been executed — PC must not have advanced.
        assert_eq!(cpu.regs.pc, 0x0200);
    }

    #[test]
    fn breakpoint_removal_allows_execution() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.add_breakpoint(0x0200);
        assert!(matches!(cpu.step(None), StepResult::Breakpoint(0x0200)));
        // Remove the breakpoint; next step should execute.
        let removed = cpu.remove_breakpoint(0x0200);
        assert!(removed);
        assert!(matches!(cpu.step(None), StepResult::Executed(_)));
        assert_eq!(cpu.regs.pc, 0x0201);
    }

    #[test]
    fn clear_breakpoints_allows_execution() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.add_breakpoint(0x0200);
        cpu.add_breakpoint(0x0201);
        cpu.clear_breakpoints();
        assert!(matches!(cpu.step(None), StepResult::Executed(_)));
        assert_eq!(cpu.regs.pc, 0x0201);
    }

    // --- watch expressions ---

    fn make_compiler() -> crate::watch::WatchCompiler {
        crate::watch::WatchCompiler::new(map_register_name, map_flag_name, |_| None)
    }

    #[test]
    fn watch_triggered_returns_watch_triggered_before_execution() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        // Watchpoint: A == 0 (true from the start, since A is 0 after reset).
        let mut compiler = make_compiler();
        let wp = compiler.compile("A == 0", cpu.evaluator_mut()).unwrap();
        cpu.evaluator_mut().add(wp);
        let result = cpu.step(None);
        assert!(matches!(result, StepResult::WatchTriggered { watch_index: 0, pc: 0x0200 }));
        // Instruction must NOT have executed — PC unchanged.
        assert_eq!(cpu.regs.pc, 0x0200);
    }

    #[test]
    fn watch_not_triggered_allows_execution() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        // Watchpoint: A == 1 (false, since A starts at 0).
        let mut compiler = make_compiler();
        let wp = compiler.compile("A == 1", cpu.evaluator_mut()).unwrap();
        cpu.evaluator_mut().add(wp);
        assert!(matches!(cpu.step(None), StepResult::Executed(_)));
        assert_eq!(cpu.regs.pc, 0x0201);
    }

    #[test]
    fn watch_error_returns_watch_error_before_execution() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        // Watchpoint: A / 0 — always produces a division-by-zero error.
        let mut compiler = make_compiler();
        let wp = compiler.compile("A / 0", cpu.evaluator_mut()).unwrap();
        cpu.evaluator_mut().add(wp);
        let result = cpu.step(None);
        assert!(matches!(
            result,
            StepResult::WatchError {
                watch_index: 0,
                pc: 0x0200,
                error: crate::watch::WatchError::DivisionByZero,
            }
        ));
        // Instruction must NOT have executed.
        assert_eq!(cpu.regs.pc, 0x0200);
    }
}
