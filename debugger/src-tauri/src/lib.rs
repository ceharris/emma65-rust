use std::path::Path;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use figment::{Figment, providers::{Env, Format, Toml}};
use tauri::{AppHandle, Emitter, Listener, Manager, State};
use tauri_plugin_log::{Target, TargetKind};
use tokio::sync::oneshot;

use emma65::disasm::Disassembler;
use emma65::emulator::bus::MAX_IRQ_SOURCES;
use emma65::emulator::{Config, Cpu, DeviceRegistry, EmulatorSession, InstantiationContext, InternalPipeTransport, IrqSource, LogSender, Transport, TransportReporter, TransportSlot};

/// Label of the main debugger window, as assigned by the (unlabeled) first
/// entry in `tauri.conf.json`'s `app.windows` list.
pub const MAIN_WINDOW_LABEL: &str = "main";

/// Configuration profile selection: the `--profile` CLI flag, profile
/// directory resolution, default-file seeding, and window title updates.
mod profile;

/// Debugger UI preferences (theme, exit confirmation, file dialog directory):
/// persisted state and Tauri commands.
mod preferences;

/// Dock layout persistence: the dockview panel arrangement, persisted as
/// opaque JSON.
mod layout;

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

/// Terminal panel: console byte-stream bridge.
mod terminal;

/// Trace panel: live-recorded execution trace and windowed reads.
mod trace;

/// Log panel: in-memory ring buffer of structured log records pushed live to the frontend.
mod logging;

/// Native app menu bar (File/Edit/Window/Help).
mod menu;

/// Recently-used profile list backing the File > Open Recent submenu.
mod recent;

/// Shared test-only helpers (e.g. a `HOME` env var lock) used across modules'
/// `#[cfg(test)]` blocks.
#[cfg(test)]
mod test_support;

/// IRQ used by the debugger
pub const DEBUGGER_IRQ: u32 = MAX_IRQ_SOURCES - 1;

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

/// Loads emulator config from `profile_dir/emulator.toml`, builds the
/// session with an injected pipe transport for the console, and returns the
/// session, the remote end of the pipe, and an `IrqSource` reserved for the
/// debugger UI's own IRQ toggle control.
///
/// Takes `log_sender` rather than constructing its own: the caller (`load_or_reload_session`)
/// needs to build it before this runs, since doing so requires an `AppHandle` (via
/// `spawn_log_collector`'s callback) that this function doesn't have.
async fn load_session(profile_dir: &Path, log_sender: LogSender) -> Result<(EmulatorSession, InternalPipeTransport, IrqSource), String> {
    let config_path = profile_dir.join("emulator.toml");

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

    // `log_sender` (built by the caller, see this function's doc comment) is shared by every
    // device, the CPU, and the event-logging loop spawned in `load_or_reload_session` below, so
    // every clone shares the same underlying cycle-count `Arc` (see `LogSender::set_cycles`)
    // instead of each independently-defaulted sender carrying its own always-zero counter (see
    // #372/#371).
    let context = InstantiationContext {
        clock_hz: config.clock_speed_hz,
        error_sender: None,
        console_transport: Some(transport_slot),
        log_sender: Some(log_sender.clone()),
    };

    let registry = DeviceRegistry::with_builtins();
    let mut session = config.build_with_context(&registry, context).await
        .map_err(|e| format!("Failed to build emulator session: {e}"))?;
    session.cpu.set_log_sender(log_sender.clone());

    let device_id = session.id_allocator.for_irq(DEBUGGER_IRQ)
        .map_err(|e| format!("Failed to build emulator session: {e}"))?;

    let ui_irq_source = IrqSource::from(device_id);
    Ok((session, remote, ui_irq_source))
}

/// Stops any free-running CPU (Run, Step Over, or Step Return) and waits for
/// its recovery to finish, so a session reload never races the background
/// task that would otherwise write the old CPU back into `CpuState` after the
/// new session has already been installed. A no-op if the CPU is halted.
async fn stop_active_run(app: &AppHandle) {
    let wait_rx = {
        let state = app.state::<disassembly::RunStopperState>();
        let guard = state.0.lock().unwrap();
        guard.as_ref().map(|stopper| {
            let (tx, rx) = oneshot::channel::<()>();
            app.once_any("debugger-run-stopped", move |_event| {
                let _ = tx.send(());
            });
            stopper.stop();
            rx
        })
    };
    if let Some(rx) = wait_rx {
        let _ = rx.await;
    }
}

/// Loads (or reloads) the emulator session from `profile_dir`: builds a fresh
/// `EmulatorSession`, replaces the console transport and its terminal bridge,
/// and resets every panel's Tauri-managed state to match the new CPU.
///
/// Safe to call more than once — this generalizes what `setup()`'s async
/// block used to do only at startup, so a later profile switch (New Profile,
/// Open Profile, Open Recent) can call it again against a different profile
/// directory without restarting the app. Stops any in-progress free-run
/// first, then drops the previous session — which drops its console
/// transport in turn, unwinding the previous terminal bridge task via EOF —
/// before building the new one.
///
/// Updates `ProfileDirState` and the main window's title to match `profile_dir`
/// regardless of whether the session itself loads successfully. UI
/// preferences are not profile-scoped, so they're left untouched. Emits
/// `session-status`, and on success `debugger-halted` with the freshly reset
/// PC.
pub(crate) async fn load_or_reload_session(app: &AppHandle, profile_dir: &Path) {
    stop_active_run(app).await;

    // Drop the previous session (if any) before building the new one, so its
    // console transport unwinds (see InternalPipeTransport::drop) rather than
    // leaking alongside the new session's.
    *app.state::<CpuState>().0.lock().unwrap() = None;
    *app.state::<terminal::TerminalTx>().0.lock().unwrap() = None;

    *app.state::<profile::ProfileDirState>().0.lock().unwrap() = profile_dir.to_path_buf();

    let profile_name = profile_dir.file_name().and_then(|n| n.to_str()).unwrap_or("default").to_string();
    profile::set_main_window_title(app, &profile_name);
    recent::record_recent_profile(app, profile_dir);

    // One `LogSender`/collector pair per session load, backed by a background thread that
    // forwards every `LogRecord` into the Log window's ring buffer + `log-record` event (see
    // `logging::push_record`). Cloned into `load_session` (shared with the CPU and every
    // device) and into the event-logging loop below, so every clone shares the same underlying
    // cycle-count `Arc` (see `LogSender::set_cycles`), same reasoning as #372/#371.
    let app_for_log = app.clone();
    let (log_sender, _log_collector_handle) =
        emma65::emulator::spawn_log_collector(logging::LOG_CHANNEL_CAPACITY, move |record| {
            logging::push_record(&app_for_log, record);
        });

    match load_session(profile_dir, log_sender.clone()).await {
        Ok((session, remote, ui_irq_source)) => {
            let (remote_rx, remote_tx) = remote.into_split();
            *app.state::<terminal::TerminalTx>().0.lock().unwrap() = Some(remote_tx);

            // Structured device/transport events now flow into the Log window via the
            // `log_sender`/collector built above, in addition to the `tauri-plugin-log` sink any
            // unconfigured `LogSender` would otherwise fall back through. Shares `log_sender`
            // with the CPU and every device (see `load_session`) so cycle counts on these events
            // aren't stuck at zero.
            tauri::async_runtime::spawn(emma65::emulator::log_device_events(
                session.error_receiver,
                log_sender,
            ));

            let mut cpu = session.cpu;
            let variant = cpu.variant();

            if let Err(e) = cpu.reset() {
                emit_status(app, SessionStatus {
                    message: format!("CPU reset failed: {e}"),
                    ok: false,
                });
                return;
            }

            let initial_pc = cpu.registers().pc;
            let disasm = Disassembler::new(variant);
            *app.state::<disassembly::DisassemblerState>().0.lock().unwrap() = Some(disasm);

            // Independent of session readiness: a bad watchpoints.emw is
            // reported inside the watchpoint panel, not via emit_status,
            // so it never blocks or fails the rest of the debugger.
            let symbol_table = cpu.bus().symbol_table().clone();
            let watch_data = match watchpoints::load_watchpoints_from(profile_dir, &symbol_table) {
                Ok((evaluator, enabled)) => watchpoints::WatchData { evaluator, compile_error: None, enabled },
                Err(message) => {
                    eprintln!("watchpoints.emw: {message}");
                    watchpoints::WatchData {
                        evaluator: emma65::watch::WatchEvaluator::new(),
                        compile_error: Some(message),
                        enabled: Vec::new(),
                    }
                }
            };
            // Install the loaded, enabled watchpoints into the CPU's own
            // evaluator too, so they actually halt execution in step()/run() —
            // not just show up in the panel's display snapshot.
            if let Err(e) = watchpoints::sync_cpu_evaluator(&mut cpu, &watch_data.evaluator, &watch_data.enabled) {
                eprintln!("Failed to install watchpoints for execution: {e}");
            }
            *app.state::<watchpoints::WatchState>().0.lock().unwrap() = watch_data;

            // Reset the rest of the per-panel state that assumes one
            // long-lived session, so nothing from the previous profile lingers.
            *app.state::<disassembly::BreakpointState>().0.lock().unwrap() = std::collections::BTreeMap::new();
            *app.state::<disassembly::SkipBreakpointPc>().0.lock().unwrap() = None;
            *app.state::<disassembly::LiveSnapshotRx>().0.lock().unwrap() = None;
            *app.state::<registers::ChangedFlagsState>().0.lock().unwrap() = 0;
            app.state::<memory::MemoryViewAddr>().0.store(0, Ordering::Relaxed);

            *app.state::<cpu_bus::CpuBusCache>().0.lock().unwrap() = cpu_bus::snapshot_cpu_bus(&cpu);
            *app.state::<cpu_bus::UiIrqSourceState>().0.lock().unwrap() = Some(ui_irq_source);
            *app.state::<CpuState>().0.lock().unwrap() = Some(cpu);

            emit_status(app, SessionStatus {
                message: "Emulator session ready".to_string(),
                ok: true,
            });

            let bridge_handle = app.clone();
            tauri::async_runtime::spawn(async move {
                terminal::run_terminal_bridge(remote_rx, bridge_handle).await;
            });

            // Emit the initial halted state so the frontend can render the
            // disassembly view immediately.
            let _ = app.emit("debugger-halted", initial_pc);
        }
        Err(message) => {
            emit_status(app, SessionStatus { message, ok: false });
        }
    }
}

/// Requests that the application exit: if the "Don't ask again" preference
/// is already set, exits immediately; otherwise focuses the main window and
/// opens the exit confirmation dialog there.
///
/// Shared by every exit trigger — File > Exit, Ctrl+Q, and the main window's
/// close control — so all three funnel through the same confirm-or-skip
/// decision (issue #349). All three are main-window-only (issue #351), but
/// the focus call is harmless if that ever changes.
fn request_exit(app: &AppHandle) {
    let skip = app.state::<preferences::UiConfigState>().0.lock().unwrap().skip_exit_confirmation;
    if skip {
        app.exit(0);
        return;
    }
    if let Some(main_window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = main_window.set_focus();
    }
    let _ = app.emit("open-exit-confirm-dialog", ());
}

/// Exits the application cleanly. Invoked directly by Ctrl+Q (handled
/// locally in `App.tsx`, main-window-only — see issue #351); routes through
/// `request_exit` like every other exit trigger so the confirmation dialog
/// (or the "Don't ask again" skip) applies uniformly.
#[tauri::command]
fn quit(app: AppHandle) {
    request_exit(&app);
}

/// Commits the exit confirmation dialog: persists the "Don't ask again"
/// checkbox state and exits. Canceling the dialog never calls this — it just
/// closes the dialog locally, leaving the preference and the process alone.
#[tauri::command]
fn confirm_exit(skip_confirmation: bool, state: State<preferences::UiConfigState>, app: AppHandle) {
    if let Err(e) = preferences::set_skip_exit_confirmation(skip_confirmation, &state) {
        eprintln!("Failed to save exit confirmation preference: {e}");
    }
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
    let cli = profile::CliArgs::parse();
    let profile_name = cli.profile;
    let profile_dir = profile::ensure_profile_dir(&profile_name).expect("Failed to prepare profile directory");
    let config_dir = profile::config_dir().expect("Failed to resolve debugger config directory");
    let recent_profiles = recent::load_recent_from(&config_dir);

    let (ready_tx, ready_rx) = oneshot::channel::<()>();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
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
        .manage(terminal::TerminalTx(Mutex::new(None)))
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
        .manage(preferences::UiConfigState(Mutex::new(preferences::load_ui_config_from(&config_dir))))
        .manage(layout::LayoutState(Mutex::new(layout::load_dock_layout_from(&config_dir))))
        .manage(profile::ProfileDirState(Mutex::new(profile_dir.clone())))
        .manage(recent::RecentProfilesState(Mutex::new(recent_profiles)))
        .manage(watchpoints::WatchState(Mutex::new(watchpoints::WatchData {
            evaluator: emma65::watch::WatchEvaluator::new(),
            compile_error: None,
            enabled: Vec::new(),
        })))
        .manage(trace::TraceState(Mutex::new(trace::TraceData::new())))
        .manage(logging::LogState(Mutex::new(std::collections::VecDeque::new())))
        .on_menu_event(|app, event| {
            let state = app.state::<menu::WindowMenuState>();
            if event.id() == state.exit_item.id() {
                request_exit(app);
            } else if event.id() == menu::NEW_PROFILE_ID {
                profile::emit_open_new_profile_dialog(app);
            } else if event.id() == menu::OPEN_PROFILE_ID {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = profile::open_profile(app_handle).await;
                });
            } else if event.id() == menu::CLEAR_RECENT_ID {
                recent::emit_open_clear_recent_dialog(app);
            } else if let Some(path) = event.id().as_ref().strip_prefix(menu::OPEN_RECENT_ID_PREFIX) {
                let app_handle = app.clone();
                let path = std::path::PathBuf::from(path);
                tauri::async_runtime::spawn(async move {
                    recent::open_recent_profile(app_handle, path).await;
                });
            } else if event.id() == menu::TOGGLE_TERMINAL_ID {
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "reveal-panel", "terminal");
            } else if event.id() == menu::TOGGLE_TRACE_ID {
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "reveal-panel", "trace");
            } else if event.id() == menu::TOGGLE_LOG_ID {
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "reveal-panel", "log");
            }
        })
        .invoke_handler(tauri::generate_handler![
            quit,
            confirm_exit,
            profile::create_profile,
            profile::open_profile,
            get_session_status,
            terminal::write_terminal,
            terminal::terminal_ready,
            trace::record_trace,
            trace::stop_trace,
            trace::get_trace_window,
            trace::get_trace_status,
            logging::get_log_records,
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
            memory::save_memory,
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
            preferences::get_theme,
            preferences::set_theme,
            preferences::get_last_file_dialog_dir,
            preferences::set_last_file_dialog_dir,
            layout::get_dock_layout,
            layout::set_dock_layout,
            watchpoints::get_watchpoints,
            watchpoints::add_watchpoint,
            watchpoints::remove_watchpoint,
            watchpoints::edit_watchpoint,
            watchpoints::toggle_watchpoint,
            recent::clear_recent_profiles,
        ])
        .setup(move |app| {
            let (app_menu, window_menu_state, recent_menu_state) = menu::build_menu(app)?;
            app.set_menu(app_menu)?;

            // GTK's default `gtk-menu-bar-accel` binds F10 to focus/open the menu
            // bar, intercepting it before it ever reaches the webview — stealing
            // it from the disassembly panel's Step Over shortcut (also F10). This
            // is a global GtkSettings property (not per-window/per-menu), so
            // disabling it here covers every window's menu bar at once.
            #[cfg(target_os = "linux")]
            if let Some(settings) = gtk::Settings::default() {
                use gtk::glib::object::ObjectExt;
                settings.set_property("gtk-menu-bar-accel", None::<&str>);
            }

            profile::set_main_window_title(app, &profile_name);

            app.manage(window_menu_state);
            app.manage(recent_menu_state);

            // Exit explicitly rather than relying on Tauri's default "exit when all
            // windows are closed" behavior, so the close control honors the exit
            // confirmation dialog (issue #349) the same as File > Exit and Ctrl+Q.
            // Always prevent the default close: the window must stay open unless/until
            // `request_exit` (or the dialog it opens) decides to actually exit the
            // process.
            if let Some(main_window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let app_for_close = app.handle().clone();
                main_window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        request_exit(&app_for_close);
                    }
                });
            }

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Wait for the terminal panel to signal it is ready (mounted inside
                // the main window's single webview, so no separate-window realize
                // step is needed here as there was before #384).
                let _ = ready_rx.await;

                load_or_reload_session(&handle, &profile_dir).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
