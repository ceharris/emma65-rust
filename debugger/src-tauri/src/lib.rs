use std::sync::atomic::AtomicU16;
use std::sync::{Arc, Mutex};

use figment::{Figment, providers::{Env, Format, Toml}};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_log::{Target, TargetKind};
use tokio::sync::oneshot;

use emma65::emulator::{Config, Cpu, Disassembler, DeviceRegistry, EmulatorSession, InstantiationContext, InternalPipeTransport, IrqSource, Transport, TransportReporter, TransportSlot};

/// Debugger UI theme selection: persisted preference and Tauri commands.
mod theme;

/// Watchpoint panel: loads/compiles `watchpoints.emw`, evaluates it on demand,
/// and supports adding/removing watchpoints with persistence back to the file.
mod watchpoints;

/// Register panel: register snapshot/edit commands and changed-flag tracking.
mod registers;

/// CPU/bus panel: IRQ/NMI controls, reset, and cached bus-signal snapshot.
mod cpu_bus;

/// Disassembly panel: run/step/stop controls, breakpoints, and disassembly listing.
mod disassembly;

/// Memory panel: paged reads, writes, fills, and file loads.
mod memory;

/// Stack panel: stack pointer and stack page snapshot.
mod stack;

/// Terminal window: console byte-stream bridge and window visibility.
mod terminal;

/// Holds the CPU once the session is ready.
pub struct CpuState(pub Mutex<Option<Cpu>>);

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

/// Loads emulator config from `~/.emma/debugger/default/emulator.toml`,
/// builds the session with an injected pipe transport for the console,
/// and returns the session, the remote end of the pipe, and an `IrqSource`
/// reserved for the debugger UI's own IRQ toggle control.
async fn load_session() -> Result<(EmulatorSession, InternalPipeTransport, IrqSource), String> {
    let config_path = theme::debugger_config_dir()?.join("emulator.toml");

    let config: Config = Figment::new()
        .merge(Toml::file(&config_path))
        .merge(Env::prefixed("EMMA65_").map(|k| k.as_str().replace('_', "-").into()))
        .extract()
        .map_err(|e| format!("Configuration error: {e}"))?;

    // No `DeviceId` exists yet at this point (the console's isn't allocated until
    // `ConsoleModule::instantiate` runs deep inside `build_with_context` below), so the
    // reporter starts unbound; `ConsoleModule::instantiate` binds it once the id is known.
    let reporter = TransportReporter::pending(None);
    let ((local, relay), remote) = InternalPipeTransport::pair(reporter.clone())
        .map_err(|e| format!("Failed to create console transport: {e}"))?;

    let transport_slot: TransportSlot = Arc::new(Mutex::new(Some((Box::new(local) as Box<dyn Transport>, relay, reporter))));
    let context = InstantiationContext {
        clock_hz: config.clock_speed_hz,
        error_sender: None,
        console_transport: Some(transport_slot),
    };

    let registry = DeviceRegistry::with_builtins();
    let mut session = config.build_with_context(&registry, context).await
        .map_err(|e| format!("Failed to build emulator session: {e}"))?;

    // Reserve an IRQ-capable device ID for the debugger UI's own IRQ toggle
    // control. Allocated after all configured devices, so it never collides
    // with a device's `IrqSource`.
    let ui_irq_source = IrqSource::from(session.id_allocator.next(true));

    Ok((session, remote, ui_irq_source))
}

/// Exits the application cleanly.
#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

/// Returns the current session status, or `None` if not yet determined.
#[tauri::command]
fn get_session_status(state: State<SessionStatusState>) -> Option<SessionStatus> {
    state.0.lock().unwrap().clone()
}

/// Resolves `name` against the symbol table; returns the associated address, or `null` if not found.
///
/// Shared by the disassembly panel (jump-to-address) and the memory panel
/// (address entry), so it isn't scoped to either panel module.
#[tauri::command]
fn resolve_symbol(name: String, cpu_state: State<CpuState>) -> Option<u16> {
    cpu_state.0.lock().unwrap().as_ref()?.bus().symbol_table().address_for(&name)
}

fn emit_status(app: &AppHandle, status: SessionStatus) {
    app.state::<SessionStatusState>().0.lock().unwrap().replace(status.clone());
    let _ = app.emit("session-status", status);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (ready_tx, ready_rx) = oneshot::channel::<()>();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::LogDir { file_name: None }),
                    Target::new(TargetKind::Webview),
                ])
                .build(),
        )
        .manage(SessionStatusState(Mutex::new(None)))
        .manage(terminal::TerminalReadyTx(Mutex::new(Some(ready_tx))))
        // TerminalTx is registered after setup; commands are only called after the
        // terminal window is open, so it will always be present by then.
        .manage(CpuState(Mutex::new(None)))
        .manage(cpu_bus::UiIrqSourceState(Mutex::new(None)))
        .manage(disassembly::DisassemblerState(Mutex::new(None)))
        .manage(registers::ChangedFlagsState(Mutex::new(0)))
        .manage(disassembly::RunStopperState(Mutex::new(None)))
        .manage(disassembly::SkipBreakpointPc(Mutex::new(None)))
        .manage(disassembly::BreakpointState(Mutex::new(std::collections::BTreeMap::new())))
        .manage(disassembly::LiveSnapshotRx(Mutex::new(None)))
        .manage(memory::MemoryViewAddr(Arc::new(AtomicU16::new(0))))
        .manage(cpu_bus::CpuBusCache(Mutex::new(cpu_bus::CpuBusSnapshot {
            irq_active: false,
            nmi_pending: false,
            cycles: 0,
            effective_speed: cpu_bus::EFFECTIVE_SPEED_UNKNOWN.to_string(),
            cpu_stopped: false,
            cpu_waiting: false,
        })))
        .manage(theme::UiConfigState(Mutex::new(theme::load_ui_config())))
        .manage(watchpoints::WatchState(Mutex::new(watchpoints::WatchData {
            evaluator: emma65::watch::WatchEvaluator::new(),
            compile_error: None,
        })))
        .invoke_handler(tauri::generate_handler![
            quit,
            terminal::toggle_terminal_visibility,
            get_session_status,
            terminal::write_terminal,
            terminal::terminal_ready,
            disassembly::run_cpu,
            disassembly::stop_cpu,
            disassembly::step_into,
            disassembly::step_over,
            disassembly::step_return,
            cpu_bus::reset_cpu,
            registers::set_register,
            cpu_bus::trigger_nmi,
            cpu_bus::assert_irq,
            cpu_bus::release_irq,
            registers::get_registers,
            disassembly::get_disassembly,
            memory::get_memory,
            memory::write_memory,
            memory::load_memory,
            memory::fill_memory,
            stack::get_stack,
            disassembly::toggle_breakpoint,
            disassembly::set_breakpoint,
            disassembly::remove_breakpoint,
            disassembly::disable_breakpoint,
            disassembly::enable_breakpoint,
            disassembly::get_breakpoints,
            cpu_bus::get_cpu_bus_state,
            resolve_symbol,
            memory::get_symbols_for_range,
            theme::get_theme,
            theme::set_theme,
            watchpoints::get_watchpoints,
            watchpoints::add_watchpoint,
            watchpoints::remove_watchpoint,
        ])
        .setup(|app| {
            if let Some(terminal_window) = app.get_webview_window(terminal::TERMINAL_WINDOW_LABEL) {
                let window_for_events = terminal_window.clone();
                terminal_window.on_window_event(move |event| match event {
                    // The terminal window is a persistent, toggle-able auxiliary
                    // window (see `toggle_terminal_visibility`), not a closable
                    // one — closing it via native window chrome would otherwise
                    // destroy it, after which the toggle command can never find
                    // it again. Hide it instead so it can still be brought back.
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let _ = window_for_events.hide();
                    }
                    // Workaround for a Wayland/GTK bug (tauri-apps/tauri#11856,
                    // tauri-apps/tao#1046): a window's title bar buttons stop
                    // responding to clicks every time it transitions from hidden
                    // to shown. Toggling `resizable` off and back on forces GTK
                    // to recompute the decoration hit-test region. Since the
                    // terminal window can be hidden/shown repeatedly (via the
                    // toggle above, or this same close-to-hide behavior), apply
                    // this on every focus, not just once at startup.
                    #[cfg(target_os = "linux")]
                    tauri::WindowEvent::Focused(true) => {
                        let _ = window_for_events.set_resizable(false);
                        let _ = window_for_events.set_resizable(true);
                    }
                    _ => {}
                });
            }

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match load_session().await {
                    Ok((session, remote, ui_irq_source)) => {
                        let (remote_rx, remote_tx) = remote.into_split();

                        // Register the tx side so write_terminal can use it.
                        handle.manage(terminal::TerminalTx(Mutex::new(remote_tx)));

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
                        *handle.state::<disassembly::DisassemblerState>().0.lock().unwrap() = Some(disasm);

                        // Independent of session readiness: a bad watchpoints.emw is
                        // reported inside the watchpoint panel, not via emit_status,
                        // so it never blocks or fails the rest of the debugger.
                        let symbol_table = cpu.bus().symbol_table().clone();
                        let watch_data = match watchpoints::load_watchpoints(&symbol_table) {
                            Ok(evaluator) => watchpoints::WatchData { evaluator, compile_error: None },
                            Err(message) => {
                                eprintln!("watchpoints.emw: {message}");
                                watchpoints::WatchData {
                                    evaluator: emma65::watch::WatchEvaluator::new(),
                                    compile_error: Some(message),
                                }
                            }
                        };
                        *handle.state::<watchpoints::WatchState>().0.lock().unwrap() = watch_data;

                        *handle.state::<cpu_bus::CpuBusCache>().0.lock().unwrap() = cpu_bus::snapshot_cpu_bus(&cpu);
                        *handle.state::<cpu_bus::UiIrqSourceState>().0.lock().unwrap() = Some(ui_irq_source);
                        *handle.state::<CpuState>().0.lock().unwrap() = Some(cpu);

                        emit_status(&handle, SessionStatus {
                            message: "Emulator session ready".to_string(),
                            ok: true,
                        });

                        // Briefly show the terminal window (created hidden at startup) so
                        // its webview realizes and runs on webkit2gtk; hidden windows never
                        // fire their JS there, so terminal_ready would otherwise never arrive.
                        if let Err(e) = terminal::show_terminal_window(&handle) {
                            eprintln!("Failed to show terminal window: {e}");
                            return;
                        }

                        // Wait for the terminal window to signal it is ready.
                        let _ = ready_rx.await;

                        // Hide it again so the window stays hidden at launch as intended;
                        // the user reveals it with Ctrl+Shift+` (see `toggle_terminal_visibility`).
                        let _ = terminal::hide_terminal_window(&handle);

                        // Start the terminal bridge.
                        let bridge_handle = handle.clone();
                        tauri::async_runtime::spawn(async move {
                            terminal::run_terminal_bridge(remote_rx, bridge_handle).await;
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
