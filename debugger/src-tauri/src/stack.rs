//! Stack panel: stack pointer and stack page snapshot.

use tauri::State;

use crate::disassembly::LiveSnapshotRx;
use crate::CpuState;

/// Stack snapshot returned to the frontend.
///
/// Covers the full stack page so the frontend can render any window within it.
#[derive(Clone, serde::Serialize)]
pub struct StackSnapshot {
    /// Current stack pointer (0x00–0xFF, page 1 offset).
    pub s: u8,
    /// All 256 bytes of the stack page (0x0100–0x01FF).
    pub page: Vec<u8>,
}

/// Returns the current stack pointer and the full stack page (0x0100–0x01FF).
///
/// Falls back to the live snapshot channel when the CPU is free-running.
/// Reads are performed via `Bus::peek_range` so no device side effects occur.
#[tauri::command]
pub fn get_stack(
    cpu_state: State<CpuState>,
    live_snapshot_rx: State<LiveSnapshotRx>,
) -> Result<StackSnapshot, String> {
    let guard = cpu_state.0.lock().unwrap();
    if let Some(cpu) = guard.as_ref() {
        let s = cpu.registers().s;
        let mut page = vec![0u8; 256];
        cpu.bus().peek_range(0x0100, &mut page).map_err(|e| e.to_string())?;
        return Ok(StackSnapshot { s, page });
    }
    // CPU is free-running — read from the live snapshot channel.
    let live = live_snapshot_rx.0.lock().unwrap()
        .as_ref()
        .and_then(|rx| rx.borrow().clone())
        .ok_or("CPU not ready")?;
    Ok(StackSnapshot { s: live.registers.s, page: live.stack_page })
}
