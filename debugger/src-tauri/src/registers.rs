//! Register panel: register snapshot/edit commands and changed-flag tracking.

use std::sync::Mutex;

use emma65::emulator::StatusRegister;
use tauri::{AppHandle, Emitter, State};

use crate::cpu_bus::{CpuBusCache, snapshot_cpu_bus};
use crate::disassembly::LiveSnapshotRx;
use crate::CpuState;

/// Bitmask of P-register bits that changed on the most recent step.
///
/// Reset to 0 on session start; updated by `step_into` and read by `get_registers`.
pub struct ChangedFlagsState(pub Mutex<u8>);

/// Register snapshot returned to the frontend.
#[derive(Clone, serde::Serialize)]
pub struct RegisterSnapshot {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub s: u8,
    pub pc: u16,
    /// Processor status byte.
    pub p: u8,
    /// Bitmask of P-register bits that changed on the most recent step (0 on initial load).
    pub changed_flags: u8,
    /// True when the CPU executed STP and is now halted; auto-step should stop.
    pub cpu_stopped: bool,
    /// True when the CPU executed WAI and is waiting for an interrupt.
    pub cpu_waiting: bool,
    /// True when the post-step PC matches a breakpoint address; auto-step should stop.
    pub breakpoint_hit: bool,
}

/// Identifies which CPU register a `set_register` call targets.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisterField {
    A,
    X,
    Y,
    S,
    Pc,
    P,
}

/// Validates that `value` fits in a `u8`, for the byte-sized register fields.
fn single_byte(value: u32, field: RegisterField) -> Result<u8, String> {
    value.try_into().map_err(|_| format!("{field:?} value out of range: must be 0-255"))
}

/// Sets a single CPU register to `value`, interpreted per `field`'s width.
///
/// Only callable while the CPU is stopped (not free-running). Emits
/// `debugger-halted` with the (possibly unchanged) PC so the disassembly view
/// re-centers and the stack view refreshes, covering PC/S edits.
#[tauri::command]
pub fn set_register(
    app: AppHandle,
    field: RegisterField,
    value: u32,
    cpu_state: State<CpuState>,
    changed_flags_state: State<ChangedFlagsState>,
    cpu_bus_cache: State<CpuBusCache>,
) -> Result<RegisterSnapshot, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;

    let p_before = cpu.registers().p.to_byte();

    match field {
        RegisterField::A => cpu.registers_mut().a = single_byte(value, field)?,
        RegisterField::X => cpu.registers_mut().x = single_byte(value, field)?,
        RegisterField::Y => cpu.registers_mut().y = single_byte(value, field)?,
        RegisterField::S => cpu.registers_mut().s = single_byte(value, field)?,
        RegisterField::P => {
            cpu.registers_mut().p = StatusRegister::from_byte(single_byte(value, field)?) | StatusRegister::UNUSED;
        }
        RegisterField::Pc => {
            cpu.registers_mut().pc = value.try_into().map_err(|_| "Pc value out of range: must be 0-65535".to_string())?;
        }
    }

    let regs = *cpu.registers();
    let changed = p_before ^ regs.p.to_byte();
    *changed_flags_state.0.lock().unwrap() = changed;
    *cpu_bus_cache.0.lock().unwrap() = snapshot_cpu_bus(cpu);

    let snapshot = RegisterSnapshot {
        a: regs.a,
        x: regs.x,
        y: regs.y,
        s: regs.s,
        pc: regs.pc,
        p: regs.p.to_byte(),
        changed_flags: changed,
        cpu_stopped: cpu.is_stopped(),
        cpu_waiting: cpu.is_waiting(),
        breakpoint_hit: false,
    };

    let _ = app.emit("debugger-halted", regs.pc);
    Ok(snapshot)
}

/// Returns a register snapshot of the current CPU state without stepping.
///
/// Falls back to the live snapshot channel when the CPU is free-running
/// (i.e. `CpuState` is `None`). `changed_flags` is 0 during free-run.
#[tauri::command]
pub fn get_registers(
    cpu_state: State<CpuState>,
    changed_flags_state: State<ChangedFlagsState>,
    live_snapshot_rx: State<LiveSnapshotRx>,
) -> Result<RegisterSnapshot, String> {
    let guard = cpu_state.0.lock().unwrap();
    if let Some(cpu) = guard.as_ref() {
        let regs = cpu.registers();
        let changed_flags = *changed_flags_state.0.lock().unwrap();
        return Ok(RegisterSnapshot {
            a: regs.a,
            x: regs.x,
            y: regs.y,
            s: regs.s,
            pc: regs.pc,
            p: regs.p.to_byte(),
            changed_flags,
            cpu_stopped: cpu.is_stopped(),
            cpu_waiting: cpu.is_waiting(),
            breakpoint_hit: false,
        });
    }
    // CPU is free-running — read from the live snapshot channel.
    let live = live_snapshot_rx.0.lock().unwrap()
        .as_ref()
        .and_then(|rx| rx.borrow().clone())
        .ok_or("CPU not ready")?;
    let regs = &live.registers;
    Ok(RegisterSnapshot {
        a: regs.a,
        x: regs.x,
        y: regs.y,
        s: regs.s,
        pc: regs.pc,
        p: regs.p.to_byte(),
        changed_flags: 0,
        cpu_stopped: false,
        cpu_waiting: false,
        breakpoint_hit: false,
    })
}
