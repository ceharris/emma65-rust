use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;

use figment::{Figment, providers::{Format, Toml, Env}};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::unix::AsyncFd;
use tokio::sync::oneshot;

use emma65::emulator::{
    Config, Cpu, DeviceRegistry, Disassembler, EmulatorSession, InstantiationContext,
    PipeTransport, Registers, TransportSlot,
};

const TERMINAL_WINDOW_LABEL: &str = "terminal";

/// Holds the tx end of the remote pipe so `write_terminal` can send bytes to the console.
pub struct TerminalTx(pub Mutex<File>);

/// One-shot sender signalling that the terminal window is ready to receive output.
pub struct TerminalReadyTx(pub Mutex<Option<oneshot::Sender<()>>>);

/// Holds the CPU and disassembler once the session is ready.
pub struct CpuState(pub Mutex<Option<Cpu>>);

/// Holds the disassembler once the session is ready.
pub struct DisassemblerState(pub Mutex<Option<Disassembler>>);

/// Registers from the previous step; used to compute which flags changed.
pub struct PrevRegisters(pub Mutex<Option<Registers>>);

/// Payload emitted to the frontend on the `session-status` event.
#[derive(Clone, serde::Serialize)]
pub struct SessionStatus {
    /// Human-readable status message.
    pub message: String,
    /// True if the session was constructed successfully.
    pub ok: bool,
}

/// Holds the last emitted session status so late-connecting frontends can retrieve it.
pub struct SessionStatusState(pub Mutex<Option<SessionStatus>>);

/// A single disassembled line returned to the frontend.
#[derive(Clone, serde::Serialize)]
pub struct DisassembledRow {
    /// Instruction address.
    pub addr: u16,
    /// Raw bytes as hex strings, e.g. ["4C", "00", "06"].
    pub bytes: Vec<String>,
    /// Mnemonic string, e.g. "JMP".
    pub mnemonic: String,
    /// Formatted operand text, e.g. "$0600".
    pub operand: String,
    /// False for invalid opcodes under the active variant.
    pub is_valid: bool,
}

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
    /// Bitmask of flags that changed since the previous step (0 on first call).
    pub changed_flags: u8,
}

/// Loads emulator config from `~/.emma/debugger/default/emulator.toml`,
/// builds the session with an injected pipe transport for the console,
/// and returns the session along with the remote end of the pipe.
async fn load_session() -> Result<(EmulatorSession, PipeTransport), String> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable is not set".to_string())?;
    let config_path = std::path::Path::new(&home).join(".emma/debugger/default/emulator.toml");

    let config: Config = Figment::new()
        .merge(Toml::file(&config_path))
        .merge(Env::prefixed("EMMA65_").map(|k| k.as_str().replace('_', "-").into()))
        .extract()
        .map_err(|e| format!("Configuration error: {e}"))?;

    let (local, remote) = PipeTransport::pair()
        .map_err(|e| format!("Failed to create console transport: {e}"))?;

    let transport_slot: TransportSlot = std::sync::Arc::new(Mutex::new(Some(Box::new(local))));
    let context = InstantiationContext {
        clock_hz: config.clock_speed_hz,
        error_sender: None,
        console_transport: Some(transport_slot),
    };

    let registry = DeviceRegistry::with_builtins();
    let session = config.build_with_context(&registry, context).await
        .map_err(|e| format!("Failed to build emulator session: {e}"))?;

    Ok((session, remote))
}

/// Tokio task that reads bytes from the remote pipe rx and emits `terminal-output` events.
async fn run_terminal_bridge(rx: File, app: AppHandle) {
    let async_rx = match AsyncFd::new(rx) {
        Ok(fd) => fd,
        Err(e) => { eprintln!("terminal bridge: AsyncFd::new failed: {e}"); return; }
    };
    let mut buf = [0u8; 256];
    loop {
        let mut guard = match async_rx.readable().await {
            Ok(g) => g,
            Err(_) => break,
        };
        match guard.try_io(|fd| fd.get_ref().read(&mut buf)) {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                let bytes: Vec<u8> = buf[..n].to_vec();
                let _ = app.emit_to(TERMINAL_WINDOW_LABEL, "terminal-output", bytes);
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Ok(Err(_)) => break,
            Err(_would_block) => continue,
        }
    }
}

/// Tauri command: called by the terminal window once its event listener is registered.
#[tauri::command]
fn terminal_ready(state: State<TerminalReadyTx>) {
    if let Some(tx) = state.0.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

/// Tauri command: send bytes typed in the terminal to the emulated console.
#[tauri::command]
fn write_terminal(bytes: Vec<u8>, state: State<TerminalTx>) -> Result<(), String> {
    let mut tx = state.0.lock().unwrap();
    tx.write_all(&bytes).map_err(|e| e.to_string())
}

/// Returns the current session status, or `None` if not yet determined.
#[tauri::command]
fn get_session_status(state: State<SessionStatusState>) -> Option<SessionStatus> {
    state.0.lock().unwrap().clone()
}

/// Executes a single CPU instruction and returns the updated register state.
///
/// Emits `debugger-halted` with the new PC after the step completes.
#[tauri::command]
fn step_into(
    app: AppHandle,
    cpu_state: State<CpuState>,
    prev_regs: State<PrevRegisters>,
) -> Result<RegisterSnapshot, String> {
    let mut guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_mut().ok_or("CPU not ready")?;

    let regs_before = *cpu.registers();
    cpu.step();
    let regs_after = *cpu.registers();

    let changed_flags = regs_before.p.to_byte() ^ regs_after.p.to_byte();
    *prev_regs.0.lock().unwrap() = Some(regs_after);

    let snapshot = RegisterSnapshot {
        a: regs_after.a,
        x: regs_after.x,
        y: regs_after.y,
        s: regs_after.s,
        pc: regs_after.pc,
        p: regs_after.p.to_byte(),
        changed_flags,
    };

    let _ = app.emit("debugger-halted", regs_after.pc);
    Ok(snapshot)
}

/// Returns a register snapshot of the current CPU state without stepping.
#[tauri::command]
fn get_registers(
    cpu_state: State<CpuState>,
    prev_regs: State<PrevRegisters>,
) -> Result<RegisterSnapshot, String> {
    let guard = cpu_state.0.lock().unwrap();
    let cpu = guard.as_ref().ok_or("CPU not ready")?;
    let regs = cpu.registers();
    let prev = prev_regs.0.lock().unwrap();
    let changed_flags = prev.map(|p| p.p.to_byte() ^ regs.p.to_byte()).unwrap_or(0);
    Ok(RegisterSnapshot {
        a: regs.a,
        x: regs.x,
        y: regs.y,
        s: regs.s,
        pc: regs.pc,
        p: regs.p.to_byte(),
        changed_flags,
    })
}

/// Returns disassembled instructions starting at `addr`, up to `count` rows.
#[tauri::command]
fn get_disassembly(
    addr: u16,
    count: usize,
    cpu_state: State<CpuState>,
    disasm_state: State<DisassemblerState>,
) -> Result<Vec<DisassembledRow>, String> {
    let cpu_guard = cpu_state.0.lock().unwrap();
    let cpu = cpu_guard.as_ref().ok_or("CPU not ready")?;
    let disasm_guard = disasm_state.0.lock().unwrap();
    let disasm = disasm_guard.as_ref().ok_or("Disassembler not ready")?;

    let lines = disasm.disassemble_range(cpu.bus(), addr, 0, count);
    let rows = lines.into_iter().map(|line| DisassembledRow {
        addr: line.addr,
        bytes: line.raw_bytes.iter().map(|b| format!("{b:02X}")).collect(),
        mnemonic: line.mnemonic.to_string(),
        operand: line.operand_text,
        is_valid: line.is_valid,
    }).collect();

    Ok(rows)
}

fn emit_status(app: &AppHandle, status: SessionStatus) {
    app.state::<SessionStatusState>().0.lock().unwrap().replace(status.clone());
    let _ = app.emit("session-status", status);
}

fn show_terminal_window(app: &AppHandle) -> Result<(), String> {
    app.get_webview_window(TERMINAL_WINDOW_LABEL)
        .ok_or_else(|| "terminal window not found".to_string())?
        .show()
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (ready_tx, ready_rx) = oneshot::channel::<()>();

    tauri::Builder::default()
        .manage(SessionStatusState(Mutex::new(None)))
        .manage(TerminalReadyTx(Mutex::new(Some(ready_tx))))
        .manage(CpuState(Mutex::new(None)))
        .manage(DisassemblerState(Mutex::new(None)))
        .manage(PrevRegisters(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            get_session_status,
            write_terminal,
            terminal_ready,
            step_into,
            get_registers,
            get_disassembly,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match load_session().await {
                    Ok((session, remote)) => {
                        let (remote_rx, remote_tx) = remote.into_split();

                        handle.manage(TerminalTx(Mutex::new(remote_tx)));

                        let mut cpu = session.cpu;
                        let variant = cpu.variant();

                        if let Err(e) = cpu.reset() {
                            emit_status(&handle, SessionStatus {
                                message: format!("CPU reset failed: {e}"),
                                ok: false,
                            });
                            return;
                        }

                        let initial_pc = cpu.registers().pc;
                        let disasm = Disassembler::new(variant);
                        *handle.state::<DisassemblerState>().0.lock().unwrap() = Some(disasm);
                        *handle.state::<CpuState>().0.lock().unwrap() = Some(cpu);

                        emit_status(&handle, SessionStatus {
                            message: "Emulator session ready".to_string(),
                            ok: true,
                        });

                        if let Err(e) = show_terminal_window(&handle) {
                            eprintln!("Failed to show terminal window: {e}");
                            return;
                        }

                        let _ = ready_rx.await;

                        let bridge_handle = handle.clone();
                        tauri::async_runtime::spawn(async move {
                            run_terminal_bridge(remote_rx, bridge_handle).await;
                        });

                        // Emit the initial halted state so the frontend can render the
                        // disassembly view immediately on first load.
                        let _ = handle.emit("debugger-halted", initial_pc);
                    }
                    Err(message) => {
                        emit_status(&handle, SessionStatus { message, ok: false });
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
