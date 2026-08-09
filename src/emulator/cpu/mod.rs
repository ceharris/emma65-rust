//! Emulated 6502 CPU; instruction fetch, decode, and execution
//! 
//! See the [`exec`](crate::emulator::exec) module for the high level interface for
//! executing 6502 instructions.

pub mod alu;
pub mod opcodes;
pub mod status;
pub mod variant;
pub mod trace;
pub mod vector;

use crate::emulator::bus::{Bus, BusOp, InterruptController};
use crate::emulator::error::{BusError, CpuBuildError, ExecError};
use crate::emulator::exec::ClockSpeed;
use crate::emulator::{TraceCallback, TraceKind, TraceRecord};
use crate::watch::{Operand, WatchContext, WatchError, WatchEvaluator};
use log::debug;
use opcodes::{AddressingMode, DecodedOp, Mnemonic, decode_table};
use status::StatusRegister;
use std::collections::HashSet;
use trace::TraceState;
use variant::{CpuVariant, InvalidOpcodePolicy};
use vector::{IdentityVectorResolver, VectorResolver};

const STACK_BASE: u16 = 0x0100;
/// Bus address of the RESET vector, read on power-on/reset.
pub const RESET_VECTOR: u16 = 0xFFFC;
/// Bus address of the NMI vector, read when servicing a non-maskable interrupt.
pub const NMI_VECTOR: u16 = 0xFFFA;
/// Bus address of the IRQ/BRK vector, read when servicing a maskable interrupt or `BRK`.
pub const IRQ_VECTOR: u16 = 0xFFFE;

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

/// Result returned by [`Cpu::step()`].
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
    /// CPU was reset via the interrupt controller
    Reset,
    /// A fatal execution error occurred.
    Error(ExecError),
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
    /// True when tracing bus operations.
    tracing: bool,
    /// Monotonic clock state; updated by `Cpu::step()` before each instruction.
    trace_state: TraceState,
    /// Optional callback invoked on every `read()` and `write()` (not `peek`).
    trace_callback: Option<Box<dyn TraceCallback>>,
    /// Resolves the effective address for a RESET/NMI/IRQ/BRK vector fetch.
    /// Defaults to [`IdentityVectorResolver`] unless a custom resolver was
    /// supplied via [`CpuBuilder::vector_resolver`].
    vector_resolver: Box<dyn VectorResolver>,

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

    /// Evaluates each of `evaluator`'s watchpoints against this CPU's current
    /// register and (peek-only, side-effect-free) bus state.
    ///
    /// Independent of `self.evaluator` — the CPU's own watch-triggered
    /// halting evaluator used by `step()` — and does not affect execution.
    /// For a caller-owned evaluator driving a display-only watchpoint view.
    pub fn evaluate_watchpoints(&self, evaluator: &mut WatchEvaluator) -> Vec<Result<Operand, WatchError>> {
        let ctx = CpuWatchContext { regs: &self.regs, bus: &self.bus };
        evaluator.evaluate_each(&ctx)
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

    /// Installs a trace callback. Pass `None` to remove an existing callback.
    ///
    /// When set, the callback is invoked on every `read()` and `write()`, but never on `peek`.
    pub fn set_trace_callback(&mut self, callback: Option<Box<dyn TraceCallback>>) {
        self.tracing = callback.is_some();
        self.trace_callback = callback;
    }

    /// Flushes the trace callback, if one is installed, making every record
    /// emitted so far visible to an independent reader of the trace stream.
    /// No-op when tracing is off. Callers that drive execution in batches
    /// (e.g. the debugger's step/run commands) should call this once after
    /// each batch completes rather than after every instruction.
    pub fn flush_trace(&mut self) {
        if let Some(cb) = &mut self.trace_callback {
            cb.flush();
        }
    }

    /// Reads the reset vector (through the installed [`VectorResolver`]) and
    /// initializes registers. Clears WAI/STP state.
    pub fn reset(&mut self) -> Result<(), ExecError> {
        self.bus_reset();
        let vector_addr = self.vector_resolver.resolve(RESET_VECTOR, &self.interrupts);
        let lo = self.bus_read(vector_addr)?;
        let hi = self.bus_read(vector_addr + 1)?;
        self.regs.pc = u16::from_le_bytes([lo, hi]);
        self.regs.s = 0xFF;
        self.regs.p = StatusRegister::UNUSED | StatusRegister::I;
        self.cycles = 0;
        self.waiting = false;
        self.stopped = false;
        debug!("6502 CPU reset");
        Ok(())
    }

    /// Returns `true` if at least one currently-active IRQ source is recognized by the
    /// installed [`VectorResolver`] (i.e. not masked out via
    /// [`VectorResolver::irq_mask`]). Unlike [`InterruptController::irq_active`], which
    /// reflects the raw physical IRQ line, this is what actually gates IRQ servicing and
    /// WAI wake-up — a source can assert the line without being recognized here.
    fn irq_recognized(&self) -> bool {
        self.interrupts.active_sources_mask() & self.vector_resolver.irq_mask() != 0
    }

    /// Fetches, decodes, and executes one instruction. Returns the step result.
    /// Skips a breakpoint at `skip_pc` if specified.
    pub fn step(&mut self, skip_pc: Option<u16>, check_breakpoints: bool) -> StepResult {
        if self.stopped {
            return StepResult::Stopped;
        }

        if self.waiting {
            // Tick devices and poll for interrupts; stay in WAI until one arrives.
            self.bus.tick_devices(1);
            self.interrupts.poll_devices(self.bus.device_interrupt_states());
            if !self.irq_recognized() && !self.interrupts.nmi_pending() {
                return StepResult::Waiting;
            }
            self.waiting = false;
            // Fall through to service the interrupt below.
        }

        if self.tracing {
            self.trace_state.begin_instruction(self.regs);
        }

        let pc = self.regs.pc;

        // Breakpoint and watch checks — skipped for skip_pc so the debugger can
        // advance past an address it is already halted at.
        if check_breakpoints && skip_pc != Some(pc) {
            if !self.breakpoints.is_empty() && self.breakpoints.contains(&pc) {
                return StepResult::Breakpoint(pc);
            }

            if !self.evaluator.is_empty() {
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
        }

        // RESET takes priority over NMI and IRQ
        if self.interrupts.take_reset() {
            return if let Err(e) = self.reset() {
                StepResult::Error(e)
            } else {
                StepResult::Reset
            }
        }
        // NMI takes priority over IRQ.
        if self.interrupts.take_nmi() {
            let interrupt_result =
                self.service_interrupt(NMI_VECTOR, false);
            return self.map_interrupt_result(interrupt_result);
        }
        if self.irq_recognized() && !self.regs.p.contains(StatusRegister::I) {
            let interrupt_result =
                self.service_interrupt(IRQ_VECTOR, false);
            self.interrupts.consume_irq_pulses();
            return self.map_interrupt_result(interrupt_result);
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
                    self.finish_cycle(decoded.base_cycles);
                    self.emit_cycles_trace(decoded.base_cycles);
                    return StepResult::Executed(decoded);
                }
                InvalidOpcodePolicy::Error => {
                    return StepResult::Error(ExecError::InvalidOpcode { addr: pc, opcode });
                }
            }
        }

        match self.execute(decoded) {
            Ok((result, cycles)) => {
                self.finish_cycle(cycles);
                self.emit_cycles_trace(cycles);
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
    ///
    /// The offset byte is always read via the bus, even when `cond` is false:
    /// real hardware always fetches both instruction bytes regardless of
    /// whether the branch is taken, and bus tracing/disassembly reconstruction
    /// depends on that read happening for every instruction.
    fn branch(&mut self, cond: bool, pc: u16) -> Result<u8, ExecError> {
        let offset = self.bus_read(pc + 1)? as i8;
        if !cond {
            return Ok(0);
        }
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

    // The offset byte (pc + 2) is always read via the bus, even when the bit
    // test doesn't trigger the branch — see `branch()`'s doc comment.
    fn bbr(&mut self, pc: u16, bit: u8) -> Result<u8, ExecError> {
        let zp = self.bus_read(pc + 1)? as u16;
        let val = self.bus_read(zp)?;
        let offset = self.bus_read(pc + 2)? as i8;
        if val & (1 << bit) == 0 {
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
        let offset = self.bus_read(pc + 2)? as i8;
        if val & (1 << bit) != 0 {
            let target = self.regs.pc.wrapping_add(offset as u16);
            self.regs.pc = target;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    // --- interrupt helpers ---

    /// Map the result from interrupt execution to the appropriate step result
    fn map_interrupt_result(&mut self, interrupt_result: Result<u8, ExecError>) -> StepResult {
        match interrupt_result {
            Ok(cycles) => {
                self.finish_cycle(cycles);
                self.emit_cycles_trace(cycles);
                StepResult::Executed(self.table[0x00])
            }
            Err(e) => StepResult::Error(e),
        }
    }

    /// Pushes PC and P, resolves `vector_addr` through the installed
    /// [`VectorResolver`] and reads the two vector bytes from the resolved
    /// address, and sets the I flag. `vector_addr` is always the nominal
    /// RESET/NMI/IRQ address; the resolver may redirect the actual read.
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
        let resolved_addr = self.vector_resolver.resolve(vector_addr, &self.interrupts);
        let lo = self.bus_read(resolved_addr)?;
        let hi = self.bus_read(resolved_addr + 1)?;
        self.regs.pc = u16::from_le_bytes([lo, hi]);
        Ok(7)
    }

    /// Advances cumulative cycle count, ticks all bus devices, and re-polls
    /// interrupt state. Called once per step after cycles for that step are known,
    /// regardless of which path (interrupt service, invalid opcode, normal
    /// execution) produced them.
    fn finish_cycle(&mut self, cycles: u8) {
        self.cycles += cycles as u64;
        self.bus.tick_devices(cycles as u32);
        self.interrupts.poll_devices(self.bus.device_interrupt_states());
    }

    // --- status flag helpers ---

    fn set_nz(&mut self, val: u8) {
        self.regs.p.set(StatusRegister::N, val & 0x80 != 0);
        self.regs.p.set(StatusRegister::Z, val == 0);
    }

    // --- bus helpers ---

    fn bus_read(&mut self, addr: u16) -> Result<u8, ExecError> {
        let result = self.bus.read(addr).map_err(|e| match e {
            BusError::Unmapped { addr } => ExecError::UnmappedAddress { addr, op: BusOp::Read },
            BusError::RomWrite { addr } => ExecError::UnmappedAddress { addr, op: BusOp::Read },
        });
        if let Ok(value) = result {
            self.emit_trace(addr, value, BusOp::Read);
        }
        result
    }

    fn bus_write(&mut self, addr: u16, value: u8) -> Result<(), ExecError> {
        let result = self.bus.write(addr, value).map_err(|e| match e {
            BusError::Unmapped { addr } => ExecError::UnmappedAddress { addr, op: BusOp::Write },
            BusError::RomWrite { addr } => ExecError::RomWrite { addr, value },
        });
        if result.is_ok() {
            self.emit_trace(addr, value, BusOp::Write);
        }
        result
    }

    fn bus_reset(&mut self) {
        self.bus.reset_devices();
    }

    fn emit_trace(&mut self, addr: u16, value: u8, op: BusOp) {
        if self.trace_callback.is_none() {
            return;
        }
        let instr_id = self.trace_state.current_instr_id();
        if let Some(regs) = self.trace_state.take_pending_registers(instr_id)
            && let Some(cb) = &mut self.trace_callback
        {
            cb.record(TraceRecord { instr_id, kind: TraceKind::Registers(regs) });
        }
        let kind = match op {
            BusOp::Read => TraceKind::Read { addr, value },
            BusOp::Write => TraceKind::Write { addr, value },
        };
        if let Some(cb) = &mut self.trace_callback {
            cb.record(TraceRecord { instr_id, kind });
        }
    }

    /// Emits a `Cycles` trace record for the instruction that just finished
    /// executing, once its total cycle count (base plus any addressing-mode
    /// or branch-taken extra cycles) is known. A no-op when no trace callback
    /// is installed.
    fn emit_cycles_trace(&mut self, cycles: u8) {
        if let Some(cb) = &mut self.trace_callback {
            let instr_id = self.trace_state.current_instr_id();
            cb.record(TraceRecord { instr_id, kind: TraceKind::Cycles(cycles) });
        }
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
    vector_resolver: Option<Box<dyn VectorResolver>>,
}

impl CpuBuilder {
    /// Creates a new builder for the given CPU variant.
    pub fn new(variant: CpuVariant) -> Self {
        Self {
            variant,
            invalid_opcode_policy: InvalidOpcodePolicy::Nop,
            clock_speed: ClockSpeed::unlimited(),
            bus: None,
            vector_resolver: None,
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

    /// Installs a custom interrupt-vector resolver, consulted whenever the
    /// CPU fetches the RESET, NMI, or IRQ/BRK vector (models the WDC65C02
    /// VPB pin). If not called, `build()` installs [`IdentityVectorResolver`].
    /// The resolver is fixed for the lifetime of the `Cpu`.
    pub fn vector_resolver(mut self, resolver: Box<dyn VectorResolver>) -> Self {
        self.vector_resolver = Some(resolver);
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
            tracing: false,
            trace_state: TraceState::new(),
            trace_callback: None,
            vector_resolver: self.vector_resolver.unwrap_or_else(|| Box::new(IdentityVectorResolver)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::bus::AddressRange;
    use crate::emulator::bus::Bus;

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
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0xAB);
    }

    #[test]
    fn zeropage_x_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x05;
        cpu.bus.write(0x0047, 0xCC).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB5, 0x42]); // LDA $42,X
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0xCC);
    }

    #[test]
    fn zeropage_y_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.y = 0x03;
        cpu.bus.write(0x0045, 0x77).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB6, 0x42]); // LDX $42,Y
        cpu.step(None, true);
        assert_eq!(cpu.regs.x, 0x77);
    }

    #[test]
    fn absolute_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x1234, 0x99).unwrap();
        write_program(&mut cpu, 0x0200, &[0xAD, 0x34, 0x12]); // LDA $1234
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x99);
    }

    #[test]
    fn absolute_x_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x10;
        cpu.bus.write(0x1244, 0x55).unwrap();
        write_program(&mut cpu, 0x0200, &[0xBD, 0x34, 0x12]); // LDA $1234,X
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x55);
    }

    #[test]
    fn absolute_y_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.y = 0x04;
        cpu.bus.write(0x1238, 0x44).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB9, 0x34, 0x12]); // LDA $1234,Y
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x44);
    }

    #[test]
    fn indirect_mode() {
        let mut cpu = make_cpu(0x0200);
        // JMP ($0300): ptr at $0300/$0301 holds $0400
        cpu.bus.write(0x0300, 0x00).unwrap();
        cpu.bus.write(0x0301, 0x04).unwrap();
        write_program(&mut cpu, 0x0200, &[0x6C, 0x00, 0x03]); // JMP ($0300)
        cpu.step(None, true);
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
        cpu.step(None, true);
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
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0xDD);
    }

    #[test]
    fn zeropage_indirect_mode() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0020, 0x00).unwrap();
        cpu.bus.write(0x0021, 0x06).unwrap();
        cpu.bus.write(0x0600, 0xEE).unwrap();
        write_program(&mut cpu, 0x0200, &[0xB2, 0x20]); // LDA ($20)
        cpu.step(None, true);
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
        cpu.step(None, true);
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
        cpu.step(None, true);
        // base is 4, +1 for page cross
        assert_eq!(cpu.cycles - cycles_before, 5);
    }

    // --- loads/stores ---

    #[test]
    fn lda_immediate_sets_nz() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xA9, 0x00]); // LDA #$00
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x00);
        assert!(cpu.regs.p.contains(StatusRegister::Z));
        assert!(!cpu.regs.p.contains(StatusRegister::N));
    }

    #[test]
    fn lda_negative_sets_n() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xA9, 0x80]); // LDA #$80
        cpu.step(None, true);
        assert!(cpu.regs.p.contains(StatusRegister::N));
    }

    #[test]
    fn sta_stores_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x42;
        write_program(&mut cpu, 0x0200, &[0x85, 0x50]); // STA $50
        cpu.step(None, true);
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0x42);
    }

    #[test]
    fn stz_stores_zero() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0xFF).unwrap();
        write_program(&mut cpu, 0x0200, &[0x64, 0x50]); // STZ $50
        cpu.step(None, true);
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0x00);
    }

    // --- transfers ---

    #[test]
    fn tax_transfer() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x42;
        write_program(&mut cpu, 0x0200, &[0xAA]); // TAX
        cpu.step(None, true);
        assert_eq!(cpu.regs.x, 0x42);
    }

    #[test]
    fn txs_does_not_set_flags() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x00;
        cpu.regs.p.remove(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0x9A]); // TXS
        cpu.step(None, true);
        assert_eq!(cpu.regs.s, 0x00);
        assert!(!cpu.regs.p.contains(StatusRegister::Z)); // TXS doesn't touch flags
    }

    // --- stack ops ---

    #[test]
    fn pha_pla_round_trip() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0xBE;
        write_program(&mut cpu, 0x0200, &[0x48, 0x68]); // PHA, PLA
        cpu.step(None, true); // PHA
        cpu.regs.a = 0x00;
        cpu.step(None, true); // PLA
        assert_eq!(cpu.regs.a, 0xBE);
    }

    #[test]
    fn php_plp_round_trip() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p = StatusRegister::N | StatusRegister::C | StatusRegister::UNUSED;
        write_program(&mut cpu, 0x0200, &[0x08, 0x28]); // PHP, PLP
        cpu.step(None, true);
        cpu.regs.p = StatusRegister::empty();
        cpu.step(None, true);
        assert!(cpu.regs.p.contains(StatusRegister::N));
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    #[test]
    fn phx_phy_plx_ply() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0x12;
        cpu.regs.y = 0x34;
        write_program(&mut cpu, 0x0200, &[0xDA, 0x5A, 0x7A, 0xFA]); // PHX PHY PLY PLX
        cpu.step(None, true); // PHX
        cpu.step(None, true); // PHY
        cpu.regs.y = 0;
        cpu.step(None, true); // PLY
        assert_eq!(cpu.regs.y, 0x34);
        cpu.regs.x = 0;
        cpu.step(None, true); // PLX
        assert_eq!(cpu.regs.x, 0x12);
    }

    // --- branches ---

    #[test]
    fn bra_always_branches() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0x80, 0x02]); // BRA +2
        cpu.step(None, true);
        // PC was 0x0202 (after fetch), branch +2 → 0x0204
        assert_eq!(cpu.regs.pc, 0x0204);
    }

    #[test]
    fn bne_not_taken_when_zero() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p.insert(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0xD0, 0x10]); // BNE +16
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0202); // not taken
    }

    #[test]
    fn not_taken_branch_still_reads_offset_byte() {
        // Real hardware always fetches both bytes of a branch instruction,
        // whether taken or not; bus tracing / trace-reconstruction (see
        // TraceDisassembler) depends on that read happening every time.
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p.insert(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0xD0, 0x10]); // BNE +16, not taken (Z set)

        let cb = Box::new(CapturingCallback(Vec::new()));
        let cb_ptr = &*cb as *const CapturingCallback as *mut CapturingCallback;
        cpu.set_trace_callback(Some(cb));

        cpu.step(None, true);

        let records = unsafe { &(*cb_ptr).0 };
        assert!(
            records.iter().any(|r| r.kind == TraceKind::Read { addr: 0x0201, value: 0x10 }),
            "offset byte at pc+1 must be read even when the branch is not taken"
        );
    }

    #[test]
    fn beq_taken_when_zero() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.p.insert(StatusRegister::Z);
        write_program(&mut cpu, 0x0200, &[0xF0, 0x10]); // BEQ +16
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0212);
    }

    #[test]
    fn branch_backward() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0x80, 0xFE_u8]); // BRA -2 → loops to self
        let pc_before = cpu.regs.pc;
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, pc_before); // back to 0x0200
    }

    // --- jumps ---

    #[test]
    fn jsr_rts_round_trip() {
        let mut cpu = make_cpu(0x0200);
        // JSR $0300; at $0300: RTS
        write_program(&mut cpu, 0x0200, &[0x20, 0x00, 0x03]); // JSR $0300
        write_program(&mut cpu, 0x0300, &[0x60]);              // RTS
        cpu.step(None, true); // JSR
        assert_eq!(cpu.regs.pc, 0x0300);
        cpu.step(None, true); // RTS
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
        cpu.step(None, true);
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
        cpu.step(None, true);
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
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x30);
    }

    #[test]
    fn sbc_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x50;
        cpu.regs.p.insert(StatusRegister::C); // no borrow
        write_program(&mut cpu, 0x0200, &[0xE9, 0x10]); // SBC #$10
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x40);
    }

    // --- logic ---

    #[test]
    fn and_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0xFF;
        write_program(&mut cpu, 0x0200, &[0x29, 0x0F]); // AND #$0F
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x0F);
    }

    #[test]
    fn ora_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x0F;
        write_program(&mut cpu, 0x0200, &[0x09, 0xF0]); // ORA #$F0
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0xFF);
    }

    #[test]
    fn eor_immediate() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0xFF;
        write_program(&mut cpu, 0x0200, &[0x49, 0xFF]); // EOR #$FF
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x00);
        assert!(cpu.regs.p.contains(StatusRegister::Z));
    }

    // --- shifts ---

    #[test]
    fn asl_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x41;
        write_program(&mut cpu, 0x0200, &[0x0A]); // ASL A
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x82);
        assert!(!cpu.regs.p.contains(StatusRegister::C));
        assert!(cpu.regs.p.contains(StatusRegister::N));
    }

    #[test]
    fn lsr_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x03;
        write_program(&mut cpu, 0x0200, &[0x4A]); // LSR A
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x01);
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    #[test]
    fn rol_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x80;
        cpu.regs.p.insert(StatusRegister::C);
        write_program(&mut cpu, 0x0200, &[0x2A]); // ROL A
        cpu.step(None, true);
        assert_eq!(cpu.regs.a, 0x01);
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    #[test]
    fn ror_accumulator() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.a = 0x01;
        cpu.regs.p.insert(StatusRegister::C);
        write_program(&mut cpu, 0x0200, &[0x6A]); // ROR A
        cpu.step(None, true);
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
        cpu.step(None, true);
        assert!(!cpu.regs.p.contains(StatusRegister::C));
        cpu.step(None, true);
        assert!(cpu.regs.p.contains(StatusRegister::C));
    }

    // --- invalid opcode ---

    #[test]
    fn invalid_opcode_nop_policy_advances_pc() {
        let mut cpu = make_cpu(0x0200);
        // $CB is WAI — valid only on WDC; invalid (1 byte) on Cmos65C02
        write_program(&mut cpu, 0x0200, &[0xCB]);
        cpu.step(None, true);
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
        assert!(matches!(cpu.step(None, true), StepResult::Error(ExecError::InvalidOpcode { .. })));
    }

    // --- WAI / STP ---

    #[test]
    fn wai_returns_waiting() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xCB]); // WAI
        cpu.step(None, true);
        assert!(matches!(cpu.step(None, true), StepResult::Waiting));
    }

    #[test]
    fn stp_returns_stopped() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xDB]); // STP
        cpu.step(None, true);
        assert!(matches!(cpu.step(None, true), StepResult::Stopped));
    }

    // --- WDC: RMB / SMB ---

    #[test]
    fn rmb_clears_bit() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0xFF).unwrap();
        write_program(&mut cpu, 0x0200, &[0x07, 0x50]); // RMB0 $50
        cpu.step(None, true);
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0xFE);
    }

    #[test]
    fn smb_sets_bit() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x00).unwrap();
        write_program(&mut cpu, 0x0200, &[0x87, 0x50]); // SMB0 $50
        cpu.step(None, true);
        assert_eq!(cpu.bus.read(0x0050).unwrap(), 0x01);
    }

    // --- WDC: BBR / BBS ---

    #[test]
    fn bbr_branches_when_bit_clear() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0xFE).unwrap(); // bit 0 clear
        // BBR0 $50, +4
        write_program(&mut cpu, 0x0200, &[0x0F, 0x50, 0x04]);
        cpu.step(None, true);
        // PC was 0x0203 after fetch, +4 = 0x0207
        assert_eq!(cpu.regs.pc, 0x0207);
    }

    #[test]
    fn bbr_not_taken_when_bit_set() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x01).unwrap(); // bit 0 set
        write_program(&mut cpu, 0x0200, &[0x0F, 0x50, 0x04]);
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0203); // not taken
    }

    #[test]
    fn bbr_not_taken_still_reads_offset_byte() {
        // See `not_taken_branch_still_reads_offset_byte`: BBRn/BBSn are
        // 3-byte instructions and must always fetch all 3 bytes via the bus.
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x01).unwrap(); // bit 0 set -> not taken
        write_program(&mut cpu, 0x0200, &[0x0F, 0x50, 0x04]); // BBR0 $50, +4

        let cb = Box::new(CapturingCallback(Vec::new()));
        let cb_ptr = &*cb as *const CapturingCallback as *mut CapturingCallback;
        cpu.set_trace_callback(Some(cb));

        cpu.step(None, true);

        let records = unsafe { &(*cb_ptr).0 };
        assert!(
            records.iter().any(|r| r.kind == TraceKind::Read { addr: 0x0202, value: 0x04 }),
            "offset byte at pc+2 must be read even when the branch is not taken"
        );
    }

    #[test]
    fn bbs_branches_when_bit_set() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(0x0050, 0x01).unwrap(); // bit 0 set
        // BBS0 $50, +4
        write_program(&mut cpu, 0x0200, &[0x8F, 0x50, 0x04]);
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0207);
    }

    // --- device tick ---

    #[test]
    fn tick_called_with_cycle_count() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP = 2 cycles
        cpu.step(None, true);
        assert_eq!(cpu.cycles(), 2);
    }

    // --- cycles accumulate ---

    #[test]
    fn cycles_accumulate_over_steps() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA, 0xEA, 0xEA]); // 3x NOP
        cpu.step(None, true);
        cpu.step(None, true);
        cpu.step(None, true);
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
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0400);
        assert!(cpu.regs.p.contains(StatusRegister::I));
    }

    #[test]
    fn irq_with_i_set_does_not_vector() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.regs.p.insert(StatusRegister::I);
        cpu.interrupts_mut().assert_irq(crate::emulator::bus::IrqSource(1));
        cpu.step(None, true);
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
        cpu.step(None, true);
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
    fn irq_pulse_auto_releases_after_service() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(IRQ_VECTOR, 0x00).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0x04).unwrap();
        cpu.regs.p.remove(StatusRegister::I);
        cpu.interrupts_mut().assert_irq_pulse(crate::emulator::bus::IrqSource(1));
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0400);
        assert!(!cpu.interrupts().irq_active(), "pulsed IRQ source should auto-release once serviced");
    }

    #[test]
    fn irq_pulse_does_not_release_other_manually_asserted_sources() {
        let mut cpu = make_cpu(0x0200);
        use crate::emulator::bus::IrqSource;
        cpu.bus.write(IRQ_VECTOR, 0x00).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0x04).unwrap();
        cpu.regs.p.remove(StatusRegister::I);
        cpu.interrupts_mut().assert_irq(IrqSource(1));
        cpu.interrupts_mut().assert_irq_pulse(IrqSource(2));
        cpu.step(None, true);
        assert!(cpu.interrupts().irq_active(), "manually-asserted source should remain active");
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

    // --- RESET (via interrupt controller) ---

    #[test]
    fn reset_returns_expected_step_result() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(RESET_VECTOR, 0x00).unwrap();
        cpu.bus.write(RESET_VECTOR + 1, 0x03).unwrap();
        cpu.interrupts_mut().signal_reset();
        let result = cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0300);
        assert!(cpu.regs.p.contains(StatusRegister::I));
        assert!(matches!(result, StepResult::Reset));
    }

    // --- NMI ---

    #[test]
    fn nmi_vectors_through_nmi_vector() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(NMI_VECTOR, 0x00).unwrap();
        cpu.bus.write(NMI_VECTOR + 1, 0x03).unwrap();
        cpu.regs.p.insert(StatusRegister::I); // I set — NMI ignores it
        cpu.interrupts_mut().signal_nmi();
        cpu.step(None, true);
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
        cpu.step(None, true);
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
        cpu.step(None, true);
        // Should vector through NMI, not IRQ
        assert_eq!(cpu.regs.pc, 0x0300);
    }

    // --- vector resolver ---

    #[test]
    fn default_resolver_is_identity_for_reset() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(RESET_VECTOR, 0x00).unwrap();
        cpu.bus.write(RESET_VECTOR + 1, 0x03).unwrap();
        cpu.reset().unwrap();
        assert_eq!(cpu.regs.pc, 0x0300);
    }

    struct RemapResolver {
        from: u16,
        to: u16,
    }

    impl VectorResolver for RemapResolver {
        fn resolve(&self, vector_addr: u16, _interrupts: &InterruptController) -> u16 {
            if vector_addr == self.from { self.to } else { vector_addr }
        }
    }

    fn make_cpu_with_resolver(start: u16, resolver: RemapResolver) -> Cpu {
        let mut bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        bus.write(RESET_VECTOR, (start & 0xFF) as u8).unwrap();
        bus.write(RESET_VECTOR + 1, (start >> 8) as u8).unwrap();
        Cpu::builder(CpuVariant::Wdc65C02)
            .bus(bus)
            .vector_resolver(Box::new(resolver))
            .build()
            .unwrap()
    }

    #[test]
    fn reset_uses_resolver_remapped_vector() {
        let mut cpu = make_cpu_with_resolver(0x0200, RemapResolver { from: RESET_VECTOR, to: 0x0500 });
        // Decoy at the nominal address proves redirection, not coincidence.
        cpu.bus.write(RESET_VECTOR, 0xAD).unwrap();
        cpu.bus.write(RESET_VECTOR + 1, 0xDE).unwrap();
        cpu.bus.write(0x0500, 0x00).unwrap();
        cpu.bus.write(0x0501, 0x06).unwrap();
        cpu.reset().unwrap();
        assert_eq!(cpu.regs.pc, 0x0600);
    }

    #[test]
    fn nmi_uses_resolver_remapped_vector() {
        let mut cpu = make_cpu_with_resolver(0x0200, RemapResolver { from: NMI_VECTOR, to: 0x0500 });
        cpu.reset().unwrap();
        cpu.bus.write(NMI_VECTOR, 0xAD).unwrap();
        cpu.bus.write(NMI_VECTOR + 1, 0xDE).unwrap();
        cpu.bus.write(0x0500, 0x00).unwrap();
        cpu.bus.write(0x0501, 0x06).unwrap();
        cpu.interrupts_mut().signal_nmi();
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0600);
    }

    #[test]
    fn irq_uses_resolver_remapped_vector() {
        let mut cpu = make_cpu_with_resolver(0x0200, RemapResolver { from: IRQ_VECTOR, to: 0x0500 });
        cpu.reset().unwrap();
        cpu.bus.write(IRQ_VECTOR, 0xAD).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0xDE).unwrap();
        cpu.bus.write(0x0500, 0x00).unwrap();
        cpu.bus.write(0x0501, 0x06).unwrap();
        cpu.regs.p.remove(StatusRegister::I);
        cpu.interrupts_mut().assert_irq(crate::emulator::bus::IrqSource(1));
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0600);
    }

    #[test]
    fn brk_uses_resolver_remapped_vector() {
        let mut cpu = make_cpu_with_resolver(0x0200, RemapResolver { from: IRQ_VECTOR, to: 0x0500 });
        cpu.reset().unwrap();
        cpu.bus.write(IRQ_VECTOR, 0xAD).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0xDE).unwrap();
        cpu.bus.write(0x0500, 0x00).unwrap();
        cpu.bus.write(0x0501, 0x06).unwrap();
        write_program(&mut cpu, 0x0200, &[0x00, 0xEA]); // BRK (pad byte)
        cpu.step(None, true);
        assert_eq!(cpu.regs.pc, 0x0600);
    }

    // --- WAI ---

    #[test]
    fn wai_returns_waiting_until_irq() {
        let mut cpu = make_cpu(0x0200);
        cpu.bus.write(IRQ_VECTOR, 0x00).unwrap();
        cpu.bus.write(IRQ_VECTOR + 1, 0x04).unwrap();
        cpu.regs.p.remove(StatusRegister::I);
        write_program(&mut cpu, 0x0200, &[0xCB]); // WAI
        cpu.step(None, true); // execute WAI — sets waiting=true
        assert!(matches!(cpu.step(None, true), StepResult::Waiting)); // no interrupt yet
        cpu.interrupts_mut().assert_irq(crate::emulator::bus::IrqSource(1));
        cpu.step(None, true); // wakes and services IRQ
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
        cpu.step(None, true); // execute WAI
        cpu.interrupts_mut().signal_nmi();
        cpu.step(None, true); // wakes and services NMI
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
        cpu.step(None, true)
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
        cpu.step(None, true); // execute STP
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
        let result = cpu.step(None, true);
        assert!(matches!(result, StepResult::Breakpoint(0x0200)));
        // Instruction must NOT have been executed — PC must not have advanced.
        assert_eq!(cpu.regs.pc, 0x0200);
    }

    #[test]
    fn breakpoint_removal_allows_execution() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.add_breakpoint(0x0200);
        assert!(matches!(cpu.step(None, true), StepResult::Breakpoint(0x0200)));
        // Remove the breakpoint; next step should execute.
        let removed = cpu.remove_breakpoint(0x0200);
        assert!(removed);
        assert!(matches!(cpu.step(None, true), StepResult::Executed(_)));
        assert_eq!(cpu.regs.pc, 0x0201);
    }

    #[test]
    fn clear_breakpoints_allows_execution() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        cpu.add_breakpoint(0x0200);
        cpu.add_breakpoint(0x0201);
        cpu.clear_breakpoints();
        assert!(matches!(cpu.step(None, true), StepResult::Executed(_)));
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
        let result = cpu.step(None, true);
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
        assert!(matches!(cpu.step(None, true), StepResult::Executed(_)));
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
        let result = cpu.step(None, true);
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

    // --- Cpu::evaluate_watchpoints ---

    #[test]
    fn evaluate_watchpoints_returns_values_against_live_cpu_state() {
        let mut cpu = make_cpu(0x0200);
        cpu.registers_mut().a = 0x42;
        let mut compiler = make_compiler();
        let mut evaluator = WatchEvaluator::new();
        let wp_true = compiler.compile("A == $42", &mut evaluator).unwrap();
        let wp_false = compiler.compile("A == 0", &mut evaluator).unwrap();
        let wp_error = compiler.compile("A / 0", &mut evaluator).unwrap();
        evaluator.add(wp_true);
        evaluator.add(wp_false);
        evaluator.add(wp_error);
        let results = cpu.evaluate_watchpoints(&mut evaluator);
        assert_eq!(results, vec![
            Ok(1),
            Ok(0),
            Err(WatchError::DivisionByZero),
        ]);
    }

    #[test]
    fn evaluate_watchpoints_is_independent_of_cpu_evaluator() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xEA]); // NOP
        // A == 0 is true from the start (A is 0 after reset), but this
        // evaluator is never installed via cpu.evaluator_mut(), so it must
        // have no effect on cpu.step().
        let mut compiler = make_compiler();
        let mut evaluator = WatchEvaluator::new();
        let wp = compiler.compile("A == 0", &mut evaluator).unwrap();
        evaluator.add(wp);
        let _ = cpu.evaluate_watchpoints(&mut evaluator);
        assert!(matches!(cpu.step(None, true), StepResult::Executed(_)));
        assert_eq!(cpu.regs.pc, 0x0201);
    }


    struct CapturingCallback(Vec<TraceRecord>);

    impl TraceCallback for CapturingCallback {
        fn record(&mut self, rec: TraceRecord) {
            self.0.push(rec);
        }
    }

    fn traced_cpu() -> (Cpu, *mut CapturingCallback) {
        let bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        let mut cpu = Cpu::builder(CpuVariant::Wdc65C02)
            .bus(bus)
            .build()
            .unwrap();
        cpu.reset().unwrap();
        let cb = Box::new(CapturingCallback(Vec::new()));
        let ptr = &*cb as *const CapturingCallback as *mut CapturingCallback;
        cpu.set_trace_callback(Some(cb));
        (cpu, ptr)
    }

    #[test]
    fn trace_callback_receives_read() {
        let (mut cpu, cb_ptr) = traced_cpu();
        cpu.bus_write(0x0100, 0x42).unwrap();
        // Clear the write record; we only care about the read.
        unsafe { (*cb_ptr).0.clear(); }
        cpu.bus_read(0x0100).unwrap();
        let records = unsafe { &(*cb_ptr).0 };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, TraceKind::Read { addr: 0x0100, value: 0x42 });
    }

    #[test]
    fn trace_callback_receives_write() {
        let (mut cpu, cb_ptr) = traced_cpu();
        cpu.bus_write(0x0200, 0xAB).unwrap();
        let records = unsafe { &(*cb_ptr).0 };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, TraceKind::Write { addr: 0x0200, value: 0xAB });
    }

    #[test]
    fn trace_callback_not_invoked_when_none() {
        let mut cpu = make_cpu(0);
        // No callback installed — just verifies no panic.
        cpu.bus_write(0x0100, 0x42).unwrap();
        cpu.bus_read(0x0100).unwrap();
    }

    #[test]
    fn trace_records_group_by_instr_id() {
        let (mut cpu, cb_ptr) = traced_cpu();

        // Simulate two instructions, each with two bus accesses.
        let regs1 = cpu.regs;
        cpu.trace_state.begin_instruction(regs1);
        cpu.bus_write(0x0100, 0x01).unwrap();
        cpu.bus_write(0x0101, 0x02).unwrap();

        let regs2 = cpu.regs;
        cpu.trace_state.begin_instruction(regs2);
        cpu.bus_write(0x0102, 0x03).unwrap();
        cpu.bus_write(0x0103, 0x04).unwrap();

        let records = unsafe { &(*cb_ptr).0 };
        // Registers, Write, Write, Registers, Write, Write.
        assert_eq!(records.len(), 6);
        assert!(matches!(records[0].kind, TraceKind::Registers(_)));
        assert_eq!(records[0].instr_id, records[1].instr_id);
        assert_eq!(records[1].instr_id, records[2].instr_id);
        assert!(matches!(records[3].kind, TraceKind::Registers(_)));
        assert_eq!(records[3].instr_id, records[4].instr_id);
        assert_eq!(records[4].instr_id, records[5].instr_id);
        assert!(records[3].instr_id > records[0].instr_id);
    }

    #[test]
    fn set_trace_callback_none_removes_callback() {
        let (mut cpu, cb_ptr) = traced_cpu();
        cpu.set_trace_callback(None);
        assert!(!cpu.tracing);
        cpu.bus_write(0x0100, 0xFF).unwrap();
        let records = unsafe { &(*cb_ptr).0 };
        assert!(records.is_empty());
    }

    #[test]
    fn step_emits_registers_record_with_correct_starting_pc() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xA9, 0x55, 0x8D, 0x00, 0x03]); // LDA #$55 ; STA $0300

        let cb = Box::new(CapturingCallback(Vec::new()));
        let cb_ptr = &*cb as *const CapturingCallback as *mut CapturingCallback;
        cpu.set_trace_callback(Some(cb));

        assert!(matches!(cpu.step(None, true), StepResult::Executed(_))); // LDA
        assert!(matches!(cpu.step(None, true), StepResult::Executed(_))); // STA

        let records = unsafe { &(*cb_ptr).0 };
        let registers_records: Vec<Registers> = records
            .iter()
            .filter_map(|r| match r.kind {
                TraceKind::Registers(regs) => Some(regs),
                _ => None,
            })
            .collect();
        assert_eq!(registers_records.len(), 2);
        assert_eq!(registers_records[0].pc, 0x0200);
        assert_eq!(registers_records[1].pc, 0x0202);
    }

    #[test]
    fn step_emits_cycles_record_matching_total_cycle_count() {
        let mut cpu = make_cpu(0x0200);
        cpu.regs.x = 0xFF;
        cpu.bus.write(0x0300, 0x42).unwrap();
        // NOP = 2 cycles; LDA $0201,X crosses a page (base 4 + 1 extra = 5 cycles).
        write_program(&mut cpu, 0x0200, &[0xEA, 0xBD, 0x01, 0x02]);

        let cb = Box::new(CapturingCallback(Vec::new()));
        let cb_ptr = &*cb as *const CapturingCallback as *mut CapturingCallback;
        cpu.set_trace_callback(Some(cb));

        assert!(matches!(cpu.step(None, true), StepResult::Executed(_))); // NOP
        assert!(matches!(cpu.step(None, true), StepResult::Executed(_))); // LDA $0201,X

        let records = unsafe { &(*cb_ptr).0 };
        let cycles_records: Vec<(u64, u8)> = records
            .iter()
            .filter_map(|r| match r.kind {
                TraceKind::Cycles(cycles) => Some((r.instr_id, cycles)),
                _ => None,
            })
            .collect();
        assert_eq!(cycles_records, vec![(0, 2), (1, 5)]);

        // The Cycles record is the last record emitted for its instruction.
        let last_kind_per_instr: Vec<_> = records.iter().map(|r| (r.instr_id, &r.kind)).collect();
        for &(instr_id, _) in &cycles_records {
            let last_for_instr = last_kind_per_instr.iter().rev().find(|(id, _)| *id == instr_id).unwrap();
            assert!(matches!(last_for_instr.1, TraceKind::Cycles(_)));
        }
    }

    #[test]
    fn breakpoint_halted_instruction_emits_no_trace_records() {
        let mut cpu = make_cpu(0x0200);
        write_program(&mut cpu, 0x0200, &[0xA9, 0x55]);
        cpu.add_breakpoint(0x0200);

        let cb = Box::new(CapturingCallback(Vec::new()));
        let cb_ptr = &*cb as *const CapturingCallback as *mut CapturingCallback;
        cpu.set_trace_callback(Some(cb));

        assert!(matches!(cpu.step(None, true), StepResult::Breakpoint(0x0200)));

        let records = unsafe { &(*cb_ptr).0 };
        assert!(records.is_empty());
    }
}
