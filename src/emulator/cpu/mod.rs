/// Pure ALU functions consumed by the CPU instruction dispatcher.
pub mod alu;
/// 256-entry opcode decode table, mnemonics, and addressing modes.
pub mod opcodes;
/// Processor status register (P) as a bitflags newtype over `u8`.
pub mod status;
/// CPU variant selection and invalid-opcode policy.
pub mod variant;

use crate::emulator::bus::Bus;
use crate::emulator::cpu::opcodes::{AddressingMode, DecodedOp, Mnemonic, decode_table};
use crate::emulator::cpu::status::StatusRegister;
use crate::emulator::cpu::variant::{CpuVariant, InvalidOpcodePolicy};
use crate::emulator::error::{BusError, CpuBuildError, ExecError};
use crate::emulator::exec::{ClockSpeed, StepResult};
use crate::emulator::bus::region::BusOp;

const STACK_BASE: u16 = 0x0100;
const RESET_VECTOR: u16 = 0xFFFC;
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
        let lo = self.bus_read(RESET_VECTOR)?;
        let hi = self.bus_read(RESET_VECTOR + 1)?;
        self.regs.pc = u16::from_le_bytes([lo, hi]);
        self.regs.s = 0xFF;
        self.regs.p = StatusRegister::UNUSED | StatusRegister::I;
        self.cycles = 0;
        self.waiting = false;
        self.stopped = false;
        Ok(())
    }

    /// Fetches, decodes, and executes one instruction. Returns the step result.
    pub fn step(&mut self) -> StepResult {
        if self.stopped {
            return StepResult::Stopped;
        }
        if self.waiting {
            return StepResult::Waiting;
        }

        let pc = self.regs.pc;
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
                let ret = self.regs.pc;
                self.push((ret >> 8) as u8)?;
                self.push(ret as u8)?;
                let p = (self.regs.p | StatusRegister::B | StatusRegister::UNUSED).to_byte();
                self.push(p)?;
                self.regs.p.insert(StatusRegister::I);
                self.regs.p.remove(StatusRegister::D);
                let lo = self.bus_read(IRQ_VECTOR)?;
                let hi = self.bus_read(IRQ_VECTOR + 1)?;
                self.regs.pc = u16::from_le_bytes([lo, hi]);
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
    use crate::emulator::bus::region::AddressRange;

    // Build a CPU with 64KB RAM and a reset vector pointing to `start`.
    fn make_cpu(start: u16) -> Cpu {
        let mut bus = Bus::config()
            .ram(AddressRange::new(0x0000, 0xFFFF))
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
        cpu.step();
        assert_eq!(cpu.regs.a, 0xAB);
    }

    #[test]
    fn zeropage_x_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x05;
        cpu.bus.write(0x0047, 0xCC).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB5, 0x42]); // LDA $42,X
        cpu.step();
        assert_eq!(cpu.regs.a, 0xCC);
    }

    #[test]
    fn zeropage_y_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.y = 0x03;
        cpu.bus.write(0x0045, 0x77).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB6, 0x42]); // LDX $42,Y
        cpu.step();
        assert_eq!(cpu.regs.x, 0x77);
    }

    #[test]
    fn absolute_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x1234, 0x99).unwrap();
        write_program(&mut cpu, 0x0200, &[0xAD, 0x34, 0x12]); // LDA $1234
        cpu.step();
        assert_eq!(cpu.regs.a, 0x99);
    }

    #[test]
    fn absolute_x_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x10;
        cpu.bus.write(0x1244, 0x55).unwrap();
        write_program(&mut cpu, 0x0200, &[0xBD, 0x34, 0x12]); // LDA $1234,X
        cpu.step();
        assert_eq!(cpu.regs.a, 0x55);
    }

    #[test]
    fn absolute_y_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.y = 0x04;
        cpu.bus.write(0x1238, 0x44).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB9, 0x34, 0x12]); // LDA $1234,Y
        cpu.step();
        assert_eq!(cpu.regs.a, 0x44);
    }

    #[test]
    fn indirect_mode() {
        let mut cpu = make_cpu(0x0200);
        // JMP ($0300): ptr at $0300/$0301 holds $0400
        cpu.bus.write(0x0300, 0x00).unwrap();
        cpu.bus.write(0x0301, 0x04).unwrap();
        write_program(&mut cpu, 0x0200, &[0x6C, 0x00, 0x03]); // JMP ($0300)
        cpu.step();
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
        cpu.step();
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
        cpu.step();
        assert_eq!(cpu.regs.a, 0xDD);
    }

    #[test]
    fn zeropage_indirect_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0020, 0x00).unwrap();
        cpu.bus.write(0x0021, 0x06).unwrap();
        cpu.bus.write(0x0600, 0xEE).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB2, 0x20]); // LDA ($20)
        cpu.step();
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
        cpu.step();
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
        cpu.step();
        // base is 4, +1 for page cross
        assert_eq!(cpu.cycles - cycles_before, 5);
    }

    // --- loads/stores ---

    #[test]
    fn lda_immediate_sets_nz() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xA9, 0x00]); // LDA #$00
        cpu.step();
        assert_eq!(cpu.regs.a, 0x00);
        assert!(cpu.regs.p.contains(StatusRegister::Z));
        assert!(!cpu.regs.p.contains(StatusRegister::N));
    }

    #[test]
    fn lda_negative_sets_n() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xA9, 0x80]); // LDA #$80
        cpu.step();
        assert!(cpu.regs.p.contains(StatusRegister::N));
    }

    #[test]
    fn sta_stores_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x42;
        write_program(&mut cpu, 0x0200, &[0x85, 0x50]); // STA $50
        cpu.step();
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0x42);
    }

    #[test]
    fn stz_stores_zero() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0xFF).unwrap();
        write_program(&mut cpu, 0x0200, &[0x64, 0x50]); // STZ $50
        cpu.step();
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0x00);
    }

    // --- transfers ---

    #[test]
    fn tax_transfer() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x42;
        write_program(&mut cpu, 0x0200, &[0xAA]); // TAX
        cpu.step();
        assert_eq!(cpu.regs.x, 0x42);
    }

    #[test]
    fn txs_does_not_set_flags() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x00;
        cpu.regs.p.remove(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0x9A]); // TXS
        cpu.step();
        assert_eq!(cpu.regs.s, 0x00);
        assert!(!cpu.regs.p.contains(StatusRegister::Z)); // TXS doesn't touch flags
    }

    // --- stack ops ---

    #[test]
    fn pha_pla_round_trip() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0xBE;
        write_program(&mut cpu, 0x0200, &[0x48, 0x68]); // PHA, PLA
        cpu.step(); // PHA
        cpu.regs.a = 0x00;
        cpu.step(); // PLA
        assert_eq!(cpu.regs.a, 0xBE);
    }

    #[test]
    fn php_plp_round_trip() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p = StatusRegister::N | StatusRegister::C | StatusRegister::UNUSED;
        write_program(&mut cpu, 0x0200, &[0x08, 0x28]); // PHP, PLP
        cpu.step();
        cpu.regs.p = StatusRegister::empty();
        cpu.step();
        assert!(cpu.regs.p.contains(StatusRegister::N));
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    #[test]
    fn phx_phy_plx_ply() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x12;
        cpu.regs.y = 0x34;
        write_program(&mut cpu, 0x0200, &[0xDA, 0x5A, 0x7A, 0xFA]); // PHX PHY PLY PLX
        cpu.step(); // PHX
        cpu.step(); // PHY
        cpu.regs.y = 0;
        cpu.step(); // PLY
        assert_eq!(cpu.regs.y, 0x34);
        cpu.regs.x = 0;
        cpu.step(); // PLX
        assert_eq!(cpu.regs.x, 0x12);
    }

    // --- branches ---

    #[test]
    fn bra_always_branches() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0x80, 0x02]); // BRA +2
        cpu.step();
        // PC was 0x0202 (after fetch), branch +2 → 0x0204
        assert_eq!(cpu.regs.pc, 0x0204);
    }

    #[test]
    fn bne_not_taken_when_zero() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p.insert(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0xD0, 0x10]); // BNE +16
        cpu.step();
        assert_eq!(cpu.regs.pc, 0x0202); // not taken
    }

    #[test]
    fn beq_taken_when_zero() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p.insert(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0xF0, 0x10]); // BEQ +16
        cpu.step();
        assert_eq!(cpu.regs.pc, 0x0212);
    }

    #[test]
    fn branch_backward() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0x80, 0xFE_u8]); // BRA -2 → loops to self
        let pc_before = cpu.regs.pc;
        cpu.step();
        assert_eq!(cpu.regs.pc, pc_before); // back to 0x0200
    }

    // --- jumps ---

    #[test]
    fn jsr_rts_round_trip() {
        let mut cpu = make_cpu(0x0200);
        // JSR $0300; at $0300: RTS
        write_program(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write_program(&mut cpu, 0x0300, &[0x60]);              // RTS
        cpu.step(); // JSR
        assert_eq!(cpu.regs.pc, 0x0300);
        cpu.step(); // RTS
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
        cpu.step();
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
        cpu.step();
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
        cpu.step();
        assert_eq!(cpu.regs.a, 0x30);
    }

    #[test]
    fn sbc_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x50;
        cpu.regs.p.insert(StatusRegister::C); // no borrow
        write_program(&mut cpu, 0x0200, &[0xE9, 0x10]); // SBC #$10
        cpu.step();
        assert_eq!(cpu.regs.a, 0x40);
    }

    // --- logic ---

    #[test]
    fn and_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0xFF;
        write_program(&mut cpu, 0x0200, &[0x29, 0x0F]); // AND #$0F
        cpu.step();
        assert_eq!(cpu.regs.a, 0x0F);
    }

    #[test]
    fn ora_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x0F;
        write_program(&mut cpu, 0x0200, &[0x09, 0xF0]); // ORA #$F0
        cpu.step();
        assert_eq!(cpu.regs.a, 0xFF);
    }

    #[test]
    fn eor_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0xFF;
        write_program(&mut cpu, 0x0200, &[0x49, 0xFF]); // EOR #$FF
        cpu.step();
        assert_eq!(cpu.regs.a, 0x00);
        assert!(cpu.regs.p.contains(StatusRegister::Z));
    }

    // --- shifts ---

    #[test]
    fn asl_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x41;
        write_program(&mut cpu, 0x0200, &[0x0A]); // ASL A
        cpu.step();
        assert_eq!(cpu.regs.a, 0x82);
        assert!(!cpu.regs.p.contains(StatusRegister::C));
        assert!(cpu.regs.p.contains(StatusRegister::N));
    }

    #[test]
    fn lsr_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x03;
        write_program(&mut cpu, 0x0200, &[0x4A]); // LSR A
        cpu.step();
        assert_eq!(cpu.regs.a, 0x01);
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    #[test]
    fn rol_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x80;
        cpu.regs.p.insert(StatusRegister::C);
        write_program(&mut cpu, 0x0200, &[0x2A]); // ROL A
        cpu.step();
        assert_eq!(cpu.regs.a, 0x01);
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    #[test]
    fn ror_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x01;
        cpu.regs.p.insert(StatusRegister::C);
        write_program(&mut cpu, 0x0200, &[0x6A]); // ROR A
        cpu.step();
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
        cpu.step();
        assert!(!cpu.regs.p.contains(StatusRegister::C));
        cpu.step();
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    // --- invalid opcode ---

    #[test]
    fn invalid_opcode_nop_policy_advances_pc() {
        let mut cpu = make_cpu(0x0200);
        // $02 is an illegal opcode (byte_len=1 in the ILL entry)
        write_program(&mut cpu, 0x0200, &[0x02]);
        cpu.step();
        assert_eq!(cpu.regs.pc, 0x0201);
    }

    #[test]
    fn invalid_opcode_error_policy() {
        let mut bus = Bus::config()
            .ram(AddressRange::new(0x0000, 0xFFFF))
            .unwrap()
            .build();
        bus.write(RESET_VECTOR, 0x00).unwrap();
        bus.write(RESET_VECTOR + 1, 0x02).unwrap();
        bus.write(0x0200, 0x02).unwrap(); // illegal opcode
        let mut cpu = Cpu::builder(CpuVariant::Cmos65C02)
            .invalid_opcode_policy(InvalidOpcodePolicy::Error)
            .bus(bus)
            .build()
            .unwrap();
        cpu.reset().unwrap();
        assert!(matches!(cpu.step(), StepResult::Error(ExecError::InvalidOpcode { .. })));
    }

    // --- WAI / STP ---

    #[test]
    fn wai_returns_waiting() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xCB]); // WAI
        cpu.step();
        assert!(matches!(cpu.step(), StepResult::Waiting));
    }

    #[test]
    fn stp_returns_stopped() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xDB]); // STP
        cpu.step();
        assert!(matches!(cpu.step(), StepResult::Stopped));
    }

    // --- WDC: RMB / SMB ---

    #[test]
    fn rmb_clears_bit() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0xFF).unwrap();
        write_program(&mut cpu, 0x0200, &[0x07, 0x50]); // RMB0 $50
        cpu.step();
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0xFE);
    }

    #[test]
    fn smb_sets_bit() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x00).unwrap();
        write_program(&mut cpu, 0x0200, &[0x87, 0x50]); // SMB0 $50
        cpu.step();
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0x01);
    }

    // --- WDC: BBR / BBS ---

    #[test]
    fn bbr_branches_when_bit_clear() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0xFE).unwrap(); // bit 0 clear
        // BBR0 $50, +4
        write_program(&mut cpu, 0x0200, &[0x0F, 0x50, 0x04]);
        cpu.step();
        // PC was 0x0203 after fetch, +4 = 0x0207
        assert_eq!(cpu.regs.pc, 0x0207);
    }

    #[test]
    fn bbr_not_taken_when_bit_set() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x01).unwrap(); // bit 0 set
        write_program(&mut cpu, 0x0200, &[0x0F, 0x50, 0x04]);
        cpu.step();
        assert_eq!(cpu.regs.pc, 0x0203); // not taken
    }

    #[test]
    fn bbs_branches_when_bit_set() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x01).unwrap(); // bit 0 set
        // BBS0 $50, +4
        write_program(&mut cpu, 0x0200, &[0x8F, 0x50, 0x04]);
        cpu.step();
        assert_eq!(cpu.regs.pc, 0x0207);
    }

    // --- device tick ---

    #[test]
    fn tick_called_with_cycle_count() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP = 2 cycles
        cpu.step();
        assert_eq!(cpu.cycles(), 2);
    }

    // --- cycles accumulate ---

    #[test]
    fn cycles_accumulate_over_steps() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA, 0xEA, 0xEA]); // 3x NOP
        cpu.step();
        cpu.step();
        cpu.step();
        assert_eq!(cpu.cycles(), 6);
    }
}
