//! Memory panel: paged reads, writes, fills, and file loads.

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};

use tauri::{AppHandle, Emitter, State};

use crate::disassembly::LiveSnapshotRx;
use crate::CpuState;

/// Paragraph-aligned address of the memory page currently displayed in the
/// memory panel. Updated by `get_memory` so the live snapshot always captures
/// the right page during free-run.
pub struct MemoryViewAddr(pub Arc<AtomicU16>);

/// Guards `MemoryViewAddr` against being overwritten by a stale `get_memory`
/// call that happens to finish executing after a more recently issued one.
///
/// Tauri dispatches command invocations to a thread pool with no ordering
/// guarantee between separate calls, so two `get_memory` invocations issued
/// in one order (e.g. navigating to 0xFF00, then to 0xC000) can run their
/// command bodies in the other order. Without this guard, whichever call's
/// `mem_view_addr.store()` executes last would win — even if it was issued
/// first and the frontend has already moved on and shown the newer page
/// (issue #453). The frontend passes its own monotonically increasing
/// per-fetch request ID as `seq`; only the highest `seq` seen so far is
/// allowed to update `MemoryViewAddr`.
pub struct MemoryViewSeq(pub AtomicU64);

/// Returns 256 bytes of memory starting at `addr` (address AND'ed with 0xfff0 for paragraph alignment).
///
/// Reads are performed via `Bus::peek_range` so no device side effects occur.
/// While the CPU is free-running, returns the most recently captured snapshot
/// page from `LiveSnapshotRx` so the memory panel stays live.
///
/// `seq` is the frontend's monotonically increasing request ID for this fetch;
/// see [`MemoryViewSeq`] for why `MemoryViewAddr` is only updated when it's the
/// highest `seq` seen so far.
#[tauri::command]
pub fn get_memory(
    addr: u16,
    seq: u64,
    cpu_state: State<CpuState>,
    live_snapshot_rx: State<LiveSnapshotRx>,
    mem_view_addr: State<MemoryViewAddr>,
    mem_view_seq: State<MemoryViewSeq>,
) -> Result<Vec<u8>, String> {
    let mut last_seq = mem_view_seq.0.load(Ordering::Relaxed);
    while seq > last_seq {
        match mem_view_seq.0.compare_exchange_weak(last_seq, seq, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => {
                mem_view_addr.0.store(addr & 0xfff0, Ordering::Relaxed);
                break;
            }
            Err(actual) => last_seq = actual,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CpuState;
    use emma65::emulator::{run_from as exec_run_from, AddressRange, Bus, ClockSpeed, Cpu, CpuBuilder, CpuVariant};
    use std::sync::Mutex;
    use tauri::Manager;
    use tauri::test::{mock_builder, mock_context, noop_assets};

    fn make_cpu() -> Cpu {
        let mut bus = Bus::config()
            .ram_with_fill(AddressRange::new(0x0000, 0xFFFF), 0)
            .unwrap()
            .build();
        bus.write(0x0010, 0xAA).unwrap();
        bus.write(0xC010, 0xBB).unwrap();
        let mut cpu = CpuBuilder::new(CpuVariant::Wdc65C02)
            .clock_speed(ClockSpeed::mhz(1.8432))
            .bus(bus)
            .build()
            .unwrap();
        cpu.reset().unwrap();
        cpu
    }

    /// Reproduces issue #453 (navigate to a page while stopped, then Run): the page
    /// `get_memory` reports while free-running must always match the address it was
    /// asked for, both immediately after `run_cpu` starts and on every subsequent
    /// `debugger-running-tick` refresh — never a stale, previously-viewed page.
    #[tokio::test]
    async fn get_memory_matches_requested_addr_throughout_free_run() {
        let app = mock_builder()
            .manage(CpuState(Mutex::new(None)))
            .manage(LiveSnapshotRx(Mutex::new(None)))
            .manage(MemoryViewAddr(Arc::new(AtomicU16::new(0))))
            .manage(MemoryViewSeq(AtomicU64::new(0)))
            .build(mock_context(noop_assets()))
            .expect("failed to build mock app");

        *app.state::<CpuState>().0.lock().unwrap() = Some(make_cpu());

        // Stopped: user navigates to 0xC000 (direct-peek path).
        let bytes = get_memory(
            0xC000,
            1,
            app.state::<CpuState>(),
            app.state::<LiveSnapshotRx>(),
            app.state::<MemoryViewAddr>(),
            app.state::<MemoryViewSeq>(),
        ).unwrap();
        assert_eq!(bytes[0x10], 0xBB, "stopped read of 0xC000 should see its marker byte");

        // Run: mirrors run_cpu's core logic (take the CPU, start the run loop,
        // subscribe the live snapshot channel).
        let cpu = app.state::<CpuState>().0.lock().unwrap().take().unwrap();
        let mem_view_addr = Arc::clone(&app.state::<MemoryViewAddr>().0);
        let handle = exec_run_from(cpu, None, Arc::clone(&mem_view_addr), true);
        *app.state::<LiveSnapshotRx>().0.lock().unwrap() = Some(handle.subscribe_live());

        // A handful of debugger-running-tick refreshes (100ms apart) re-requesting
        // the same page; every one must still see 0xC000's content, not 0x0000's.
        for i in 0..5u64 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let bytes = get_memory(
                0xC000,
                2 + i,
                app.state::<CpuState>(),
                app.state::<LiveSnapshotRx>(),
                app.state::<MemoryViewAddr>(),
                app.state::<MemoryViewSeq>(),
            ).unwrap();
            assert_eq!(bytes[0x10], 0xBB, "running read of 0xC000 should see its marker byte, not 0x0000's");
        }

        handle.take_cpu().await;
    }

    /// Reproduces the true root cause behind issue #453: two `get_memory` calls
    /// issued in one order (an earlier navigation to 0xFF00, then a later one to
    /// 0xC000) can have their command bodies actually *execute* in the opposite
    /// order, since Tauri dispatches commands to a thread pool with no ordering
    /// guarantee. Without the `seq` guard, the stale 0xFF00 call finishing last
    /// would overwrite `MemoryViewAddr` back to 0xFF00 even though the frontend
    /// has already moved on to 0xC000. Simulates that inversion directly: call
    /// the newer request (seq 2, addr 0xC000) before the older one (seq 1, addr
    /// 0xFF00), exactly as they'd execute out of order.
    #[test]
    fn get_memory_ignores_late_arriving_stale_seq() {
        let app = mock_builder()
            .manage(CpuState(Mutex::new(None)))
            .manage(LiveSnapshotRx(Mutex::new(None)))
            .manage(MemoryViewAddr(Arc::new(AtomicU16::new(0))))
            .manage(MemoryViewSeq(AtomicU64::new(0)))
            .build(mock_context(noop_assets()))
            .expect("failed to build mock app");

        *app.state::<CpuState>().0.lock().unwrap() = Some(make_cpu());

        // The newer request (issued second by the frontend) executes first.
        get_memory(
            0xC000,
            2,
            app.state::<CpuState>(),
            app.state::<LiveSnapshotRx>(),
            app.state::<MemoryViewAddr>(),
            app.state::<MemoryViewSeq>(),
        ).unwrap();
        assert_eq!(app.state::<MemoryViewAddr>().0.load(Ordering::Relaxed), 0xC000);

        // The older, stale request (issued first, but slower) executes second.
        get_memory(
            0xFF00,
            1,
            app.state::<CpuState>(),
            app.state::<LiveSnapshotRx>(),
            app.state::<MemoryViewAddr>(),
            app.state::<MemoryViewSeq>(),
        ).unwrap();

        // MemoryViewAddr must still reflect the newer request, not the stale one.
        assert_eq!(app.state::<MemoryViewAddr>().0.load(Ordering::Relaxed), 0xC000);
    }
}
