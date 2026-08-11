//! Memory panel: paged reads, writes, fills, and file loads.

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

use tauri::{AppHandle, Emitter, State};

use crate::disassembly::LiveSnapshotRx;
use crate::CpuState;

/// Paragraph-aligned address of the memory page currently displayed in the
/// memory panel. Updated by `get_memory` so the live snapshot always captures
/// the right page during free-run.
pub struct MemoryViewAddr(pub Arc<AtomicU16>);

/// Returns 256 bytes of memory starting at `addr` (address AND'ed with 0xfff0 for paragraph alignment).
///
/// Reads are performed via `Bus::peek_range` so no device side effects occur.
/// While the CPU is free-running, returns the most recently captured snapshot
/// page from `LiveSnapshotRx` so the memory panel stays live.
#[tauri::command]
pub fn get_memory(
    addr: u16,
    cpu_state: State<CpuState>,
    live_snapshot_rx: State<LiveSnapshotRx>,
    mem_view_addr: State<MemoryViewAddr>,
) -> Result<Vec<u8>, String> {
    mem_view_addr.0.store(addr & 0xfff0, Ordering::Relaxed);
    let guard = cpu_state.0.lock().unwrap();
    if let Some(cpu) = guard.as_ref() {
        let page_start = addr & 0xfff0;
        let mut buf = vec![0u8; 256];
        cpu.bus().peek_range(page_start, &mut buf).map_err(|e| e.to_string())?;
        return Ok(buf);
    }
    let live = live_snapshot_rx.0.lock().unwrap()
        .as_ref()
        .and_then(|rx| rx.borrow().clone())
        .ok_or("CPU not ready")?;
    Ok(live.memory_page)
}

/// Writes `data` bytes to memory starting at `addr`, wrapping at 0xFFFF.
///
/// Writes are performed via `Bus::write` so device side effects apply, unless `patch` is true
/// in which case `Bus::patch` is used to bypass ROM write protection.
/// Only callable while the CPU is halted; returns an error if the CPU is running.
/// Emits `"debugger-halted"` and `"memory-modified"` on success to refresh all panels
/// (e.g. disassembly, stack, and watchpoint views, any of which may depend on the
/// written addresses).
#[tauri::command]
pub fn write_memory(
    addr: u16,
    data: Vec<u8>,
    patch: bool,
    cpu_state: State<CpuState>,
    app: AppHandle,
) -> Result<(), String> {
    let pc = {
        let mut guard = cpu_state.0.lock().unwrap();
        let cpu = guard.as_mut().ok_or("CPU not ready")?;
        let bus = cpu.bus_mut();
        for (i, &byte) in data.iter().enumerate() {
            let a = addr.wrapping_add(i as u16);
            if patch {
                bus.patch(a, byte);
            } else {
                bus.write(a, byte).map_err(|e| e.to_string())?;
            }
        }
        cpu.registers().pc
    };
    app.emit("debugger-halted", pc).ok();
    app.emit("memory-modified", ()).ok();
    Ok(())
}

/// Loads a file into emulator memory, bypassing ROM write protection.
///
/// Reads the file at `path`, parses it according to `format` (`"image"`, `"intel_hex"`,
/// or `"motorola_srec"`), and writes the result to the bus via `Bus::patch`.
/// `bias` is the load address (meaningful only for the binary image format).
/// If `symbol_path` is given, loads VICE labels from that file into the bus symbol table.
#[tauri::command]
pub async fn load_memory(
    path: String,
    format: String,
    bias: u16,
    symbol_path: Option<String>,
    cpu_state: State<'_, CpuState>,
    app: AppHandle,
) -> Result<(), String> {
    use emma65::emulator::bus::BusLoadTarget;
    use emma65::emulator::bus::symbol::load_vice_labels;
    use emma65::emulator::config::loader::{LoadFormat, load_target};

    let load_format = match format.as_str() {
        "image"         => LoadFormat::Image,
        "intel_hex"     => LoadFormat::IntelHex,
        "motorola_srec" => LoadFormat::MotorolaSrec,
        _               => return Err(format!("Unknown format: {format}")),
    };

    let data = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;

    let symbols = match symbol_path.as_deref().filter(|p| !p.trim().is_empty()) {
        Some(sp) => Some(load_vice_labels(sp).await.map_err(|e| e.to_string())?),
        None => None,
    };

    let pc = {
        let mut guard = cpu_state.0.lock().unwrap();
        let cpu = guard.as_mut().ok_or("CPU not ready")?;
        let pc = cpu.registers().pc;
        let bus = cpu.bus_mut();
        let mut target = BusLoadTarget::new(bus, bias as usize);
        load_target(&data, load_format, &mut target).map_err(|e| e.to_string())?;
        if let Some(table) = &symbols {
            let bus_table = bus.symbol_table_mut();
            bus_table.clear();
            bus_table.insert_from(table);
        }
        pc
    };

    app.emit("debugger-halted", pc).ok();
    app.emit("memory-modified", ()).ok();
    Ok(())
}

/// Reads the inclusive address range [`start`, `end`] via `Bus::peek_range` and writes it
/// verbatim to the file at `path`.
///
/// Reads have no side effects, but the CPU must still be halted (`cpu_state` populated)
/// because `peek_range` needs direct bus access, which is unavailable while the CPU is
/// free-running in its background thread.
#[tauri::command]
pub async fn save_memory(
    start: u16,
    end: u16,
    path: String,
    cpu_state: State<'_, CpuState>,
) -> Result<(), String> {
    let len = end.checked_sub(start).ok_or("End address must be >= start address")? as usize + 1;
    let mut buf = vec![0u8; len];
    {
        let guard = cpu_state.0.lock().unwrap();
        let cpu = guard.as_ref().ok_or("CPU not ready")?;
        cpu.bus().peek_range(start, &mut buf).map_err(|e| e.to_string())?;
    }
    tokio::fs::write(&path, &buf).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Fills every address in the inclusive range [`start`, `end`] with `value`.
///
/// Uses `Bus::patch` when `patch` is true (bypasses ROM write protection),
/// otherwise `Bus::write`. Emits `"debugger-halted"` and `"memory-modified"`
/// on success to refresh all panels.
#[tauri::command]
pub fn fill_memory(
    start: u16,
    end: u16,
    value: u8,
    patch: bool,
    cpu_state: State<CpuState>,
    app: AppHandle,
) -> Result<(), String> {
    let pc = {
        let mut guard = cpu_state.0.lock().unwrap();
        let cpu = guard.as_mut().ok_or("CPU not ready")?;
        let pc = cpu.registers().pc;
        let bus = cpu.bus_mut();
        let mut addr = start;
        loop {
            if patch {
                bus.patch(addr, value);
            } else {
                bus.write(addr, value).map_err(|e| e.to_string())?;
            }
            if addr == end {
                break;
            }
            addr = addr.wrapping_add(1);
        }
        pc
    };
    app.emit("debugger-halted", pc).ok();
    app.emit("memory-modified", ()).ok();
    Ok(())
}

/// Returns symbol names for each address in `[start, start + count)`, as a list of name lists.
///
/// Index `i` corresponds to `start.wrapping_add(i)`; an empty inner list means no symbols at that address.
/// Returns all-empty lists when the CPU is not ready.
#[tauri::command]
pub fn get_symbols_for_range(start: u16, count: usize, cpu_state: State<CpuState>) -> Vec<Vec<String>> {
    let guard = cpu_state.0.lock().unwrap();
    let Some(cpu) = guard.as_ref() else {
        return vec![vec![]; count];
    };
    let symbol_table = cpu.bus().symbol_table();
    (0..count)
        .map(|i| {
            let addr = start.wrapping_add(i as u16);
            symbol_table.names_for(addr).map(|s| s.to_string()).collect()
        })
        .collect()
}
