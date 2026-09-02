use std::path::Path;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use clap::Parser;
use figment::{Figment, providers::{Env, Format, Toml}};
use tauri::{AppHandle, Emitter, Listener, Manager, State};
use tauri_plugin_log::{Target, TargetKind};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{mpsc, oneshot};

use emma65::disassembler::Disassembler;
use emma65::emulator::bus::MAX_IRQ_SOURCES;
use emma65::emulator::{Config, Cpu, DeviceRegistry, DisplayFrame, DisplayFrameSlot, DisplayGeometrySlot, EmulatorSession, InstantiationContext, InternalPipeTransport, IrqSource, LedMatrixFrame, LedMatrixFrameSlot, LedMatrixGeometrySlot, LogSender, Transport, TransportReporter, TransportSlot};

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

/// Disassembly panel: run/step/stop controls and disassembly listing.
mod disassembly;

/// Breakpoint panel: tracked breakpoint set, CRUD commands, and the
/// `breakpoints-changed` broadcast shared with the Disassembly gutter;
/// persists the set to `breakpoints.json` in the active profile.
mod breakpoints;

/// Memory panel: paged reads, writes, fills, and file loads.
mod memory;

/// Assembler panel: assembles source text and patches the result into the
/// CPU's bus.
mod assembler;

/// Stack panel: stack pointer and stack page snapshot.
mod stack;

/// Symbols panel: read-only snapshot of the live symbol table (name,
/// address, source, aliases) for the sortable/filterable Symbols panel.
mod symbols;

/// Terminal panel: console byte-stream bridge.
mod terminal;

/// Display panel: composited-frame push channel bridge, dockable/detachable window lifecycle
/// (mirroring `terminal`'s dock/detach architecture), and the keyboard input bridge that
/// forwards bytes typed in the Display panel to the active `display` device's keyboard
/// sub-range (display/keyboard integration plan, unit 5).
mod display;

/// LED matrix panel: composited per-matrix-frame push channel bridge and dockable/detachable
/// window lifecycle (mirroring `display`'s architecture, minus the keyboard bridge — LED
/// matrices have no input capability).
mod led_matrix;

/// Trace panel: live-recorded execution trace and windowed reads.
mod trace;

/// Log panel: in-memory ring buffer of structured log records pushed live to the frontend.
mod logging;

/// Native app menu bar (File/Edit/Window/Help).
mod menu;

/// Recently-used profile list backing the File > Open Recent submenu.
mod recent;

/// Help > About dialog: static app info plus a production-only build-info
/// line (git commit hash, build timestamp).
mod about;

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
///
/// Also creates a fresh composited-frame channel and geometry slot (design doc §9) for a
/// possible `display` device to consume via `InstantiationContext::display_frame_sink`/
/// `display_geometry_sink` — present regardless of whether the active profile actually
/// configures one, the same way `console_transport` is always injected whether or not a
/// console device consumes it. Returns the receiver and whatever geometry ended up in the
/// slot (`None` if no display device is configured) alongside the session.
///
/// Also injects a second, independent pipe transport for `InstantiationContext::keyboard_transport`
/// (display/keyboard integration plan), built and consumed the same way as the console one —
/// present regardless of whether the active profile's `display` device actually configures
/// `keyboard-address=`. Returns its remote end alongside the console one.
///
/// Also creates a fresh composited-per-matrix-frame channel and geometry slot (design doc §10)
/// for a possible `display/matrix` device to consume via
/// `InstantiationContext::led_matrix_frame_sink`/`led_matrix_geometry_sink`, the same way as the
/// display channel/slot above.
#[allow(clippy::type_complexity)]
async fn load_session(profile_dir: &Path, log_sender: LogSender) -> Result<(EmulatorSession, InternalPipeTransport, InternalPipeTransport, IrqSource, mpsc::Receiver<DisplayFrame>, Option<display::DisplayGeometryPayload>, mpsc::Receiver<LedMatrixFrame>, Option<led_matrix::LedMatrixGeometryPayload>), String> {
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

    // No `DeviceId` exists yet at this point either — `CharDisplayModule::instantiate` binds its
    // own reporter once its id is known, same as the console reporter above.
    let kbd_reporter = TransportReporter::pending(None);
    let ((kbd_local, kbd_relay), kbd_remote) = InternalPipeTransport::pair(kbd_reporter.clone())
        .map_err(|e| format!("Failed to create keyboard transport: {e}"))?;
    let keyboard_transport_slot: TransportSlot =
        Arc::new(Mutex::new(Some((Box::new(kbd_local) as Box<dyn Transport>, kbd_relay, kbd_reporter))));

    // Bounded to 2 (design doc §6): the device's own vsync composites at most `frame_rate_hz`
    // times per second, and the bridge task's wall-clock rate limiting (`display::run_display_bridge`)
    // is what actually protects the UI -- this capacity just needs to be small enough that
    // `try_send` starts dropping stale frames promptly if the bridge task is ever briefly
    // behind, rather than allowing a backlog to build up.
    let (display_frame_tx, display_frame_rx) = mpsc::channel::<DisplayFrame>(2);
    let display_frame_slot: DisplayFrameSlot = Arc::new(Mutex::new(Some(display_frame_tx)));
    let display_geometry_slot: DisplayGeometrySlot = Arc::new(Mutex::new(None));

    // NOT the same bounded-to-2 reasoning as `display_frame_tx` above, despite looking parallel:
    // `display_frame_tx`'s capacity-2 argument relies on the device pushing exactly one frame per
    // vsync and recompositing its *entire* grid every time, so a `try_send` dropped under a brief
    // backlog is immediately superseded by the next vsync's frame -- self-healing. LED matrix's
    // per-matrix push model (design doc §10) breaks both halves of that: a single triggering event
    // (a full-bitmask `CMD_SWAP`, or one auto-refresh tick finding several matrices dirty at once)
    // can synchronously `try_send` up to `matrices` frames back to back with no yield point between
    // them, and a dropped frame is *not* superseded by anything -- there's no periodic full-repaint,
    // so a matrix whose swap frame got dropped stays visibly stale until some unrelated later write
    // happens to touch that exact matrix again (see
    // `cmd_swap_of_all_8_matrices_delivers_every_frame_when_the_sink_is_sized_for_8` in
    // `led_matrix::mod::tests`, which documents the requirement this capacity exists to satisfy).
    // 8 is the spec's hard cap on `matrices` (`VALID_MATRIX_COUNTS` in
    // `emulator::config::led_matrix`), so it's sized to guarantee no single triggering event can
    // ever overflow this channel, regardless of how many matrices are configured.
    let (led_matrix_frame_tx, led_matrix_frame_rx) = mpsc::channel::<LedMatrixFrame>(8);
    let led_matrix_frame_slot: LedMatrixFrameSlot = Arc::new(Mutex::new(Some(led_matrix_frame_tx)));
    let led_matrix_geometry_slot: LedMatrixGeometrySlot = Arc::new(Mutex::new(None));

    // `log_sender` (built by the caller, see this function's doc comment) is shared by every
    // device, the CPU, and the event-logging loop spawned in `load_or_reload_session` below, so
    // every clone shares the same underlying cycle-count `Arc` (see `LogSender::set_cycles`)
    // instead of each independently-defaulted sender carrying its own always-zero counter (see
    // #372/#371).
    let context = InstantiationContext {
        clock_hz: config.clock_speed_hz,
        error_sender: None,
        console_transport: Some(transport_slot),
        keyboard_transport: Some(keyboard_transport_slot),
        log_sender: Some(log_sender.clone()),
        display_frame_sink: Some(display_frame_slot),
        display_geometry_sink: Some(display_geometry_slot.clone()),
        led_matrix_frame_sink: Some(led_matrix_frame_slot),
        led_matrix_geometry_sink: Some(led_matrix_geometry_slot.clone()),
    };

    let registry = DeviceRegistry::with_builtins();
    let mut session = config.build_with_context(&registry, context).await
        .map_err(|e| format!("Failed to build emulator session: {e}"))?;
    session.cpu.set_log_sender(log_sender.clone());

    let device_id = session.id_allocator.for_irq(DEBUGGER_IRQ)
        .map_err(|e| format!("Failed to build emulator session: {e}"))?;

    let ui_irq_source = IrqSource::from(device_id);
    let display_geometry = display_geometry_slot.lock().unwrap().take().map(display::DisplayGeometryPayload::from);
    let led_matrix_geometry =
        led_matrix_geometry_slot.lock().unwrap().take().map(led_matrix::LedMatrixGeometryPayload::from);
    Ok((session, remote, kbd_remote, ui_irq_source, display_frame_rx, display_geometry, led_matrix_frame_rx, led_matrix_geometry))
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
/// loads `profile_dir`'s watchpoints and breakpoints, and resets every other
/// panel's Tauri-managed state to match the new CPU.
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
    app.state::<terminal::TerminalHistory>().0.lock().unwrap().clear();
    *app.state::<display::DisplayGeometryState>().0.lock().unwrap() = None;
    *app.state::<display::KeyboardTx>().0.lock().unwrap() = None;
    *app.state::<led_matrix::LedMatrixGeometryState>().0.lock().unwrap() = None;
    app.state::<led_matrix::LedMatrixFrameCache>().0.lock().unwrap().clear();

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
        Ok((session, remote, kbd_remote, ui_irq_source, display_frame_rx, display_geometry, led_matrix_frame_rx, led_matrix_geometry)) => {
            let (remote_rx, remote_tx) = remote.into_split();
            *app.state::<terminal::TerminalTx>().0.lock().unwrap() = Some(remote_tx);

            // The rx half is intentionally dropped: `into_split()` returns two independently-
            // `try_clone()`d `File`s with no coupling requiring both to stay alive, and nothing
            // ever writes to this pipe from the emulator side — `CharDisplay`'s keyboard
            // sub-range is input-only, so its `write` handler never calls `transport.send()`.
            let (_kbd_remote_rx, kbd_remote_tx) = kbd_remote.into_split();
            *app.state::<display::KeyboardTx>().0.lock().unwrap() = Some(kbd_remote_tx);

            let frame_rate_hz = display_geometry.map(|g| g.frame_rate_hz);
            *app.state::<display::DisplayGeometryState>().0.lock().unwrap() = display_geometry;
            let display_bridge_handle = app.clone();
            tauri::async_runtime::spawn(async move {
                display::run_display_bridge(display_frame_rx, display_bridge_handle, frame_rate_hz).await;
            });

            *app.state::<led_matrix::LedMatrixGeometryState>().0.lock().unwrap() = led_matrix_geometry;
            let led_matrix_bridge_handle = app.clone();
            tauri::async_runtime::spawn(async move {
                led_matrix::run_led_matrix_bridge(led_matrix_frame_rx, led_matrix_bridge_handle).await;
            });

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

            // Independent of session readiness, same as watchpoints above:
            // load the new profile's breakpoints.json, install its enabled
            // addresses into the CPU so they actually halt execution, and
            // broadcast breakpoints-changed so the panel and gutter resync
            // without a dedicated "profile changed" event.
            let loaded_breakpoints = breakpoints::load_breakpoints_from(profile_dir);
            breakpoints::install_breakpoints(&mut cpu, &loaded_breakpoints);
            breakpoints::emit_loaded_breakpoints(app, &loaded_breakpoints, &symbol_table);
            *app.state::<breakpoints::BreakpointState>().0.lock().unwrap() = loaded_breakpoints;

            // The Symbols panel has no per-panel state of its own to reset —
            // it just re-fetches `get_symbols` on this broadcast.
            let _ = app.emit("symbols-changed", ());

            // Reset the rest of the per-panel state that assumes one
            // long-lived session, so nothing from the previous profile lingers.
            *app.state::<disassembly::SkipBreakpointPc>().0.lock().unwrap() = None;
            *app.state::<disassembly::LiveSnapshotRx>().0.lock().unwrap() = None;
            *app.state::<registers::ChangedFlagsState>().0.lock().unwrap() = 0;
            app.state::<memory::MemoryViewAddr>().0.store(0, Ordering::Relaxed);
            app.state::<memory::MemoryViewSeq>().0.store(0, Ordering::Relaxed);

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
    persist_window_geometries(app);
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

/// Captures and persists the main window's geometry, plus the
/// detached-Terminal and detached-Display windows' geometry if either is
/// currently detached (visible) — see issue #419. Called once per exit
/// request rather than on every resize/move event, so `ui.toml` isn't
/// rewritten continuously while the user drags a window around.
fn persist_window_geometries(app: &AppHandle) {
    let state = app.state::<preferences::UiConfigState>();
    if let Some(main_window) = app.get_webview_window(MAIN_WINDOW_LABEL)
        && let Err(e) = preferences::save_window_geometry(&main_window, &state, |c, g| c.main_window_geometry = Some(g))
    {
        eprintln!("Failed to save main window geometry: {e}");
    }
    if let Some(terminal_window) = app.get_webview_window(terminal::TERMINAL_DETACHED_WINDOW_LABEL)
        && terminal_window.is_visible().unwrap_or(false)
        && let Err(e) =
            preferences::save_window_geometry(&terminal_window, &state, |c, g| c.terminal_window_geometry = Some(g))
    {
        eprintln!("Failed to save terminal window geometry: {e}");
    }
    if let Some(display_window) = app.get_webview_window(display::DISPLAY_DETACHED_WINDOW_LABEL)
        && display_window.is_visible().unwrap_or(false)
        && let Err(e) =
            preferences::save_window_geometry(&display_window, &state, |c, g| c.display_window_geometry = Some(g))
    {
        eprintln!("Failed to save display window geometry: {e}");
    }
    if let Some(led_matrix_window) = app.get_webview_window(led_matrix::LED_MATRIX_DETACHED_WINDOW_LABEL)
        && led_matrix_window.is_visible().unwrap_or(false)
        && let Err(e) = preferences::save_window_geometry(
            &led_matrix_window, &state, |c, g| c.led_matrix_window_geometry = Some(g))
    {
        eprintln!("Failed to save LED matrix window geometry: {e}");
    }
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
    let config_dir = profile::config_dir().expect("Failed to resolve debugger config directory");
    let recent_profiles = recent::load_and_prune_recent(&config_dir);
    let (profile_dir, profile_name) = profile::resolve_startup_profile(cli.profile.as_deref(), &recent_profiles)
        .expect("Failed to prepare profile directory");
    // `--restore-layout` (issue #398) skips loading the persisted arrangement
    // entirely rather than deleting `layout.json` up front: `DockLayoutData::default()`
    // is exactly what a brand-new profile starts with, and `DockLayout.tsx`'s own
    // "nothing persisted" fallback (`restoreLayout`) already rebuilds the default
    // arrangement and re-persists it, which is what actually overwrites the stale file.
    let dock_layout =
        if cli.restore_layout { layout::DockLayoutData::default() } else { layout::load_dock_layout_from(&config_dir) };
    let terminal_was_detached = dock_layout.terminal_detached;
    let display_was_detached = dock_layout.display_detached;
    let led_matrix_was_detached = dock_layout.led_matrix_detached;

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
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
        .manage(terminal::TerminalHistory::default())
        .manage(terminal::TerminalTx(Mutex::new(None)))
        .manage(terminal::TerminalTargetWindow(Mutex::new(MAIN_WINDOW_LABEL.to_string())))
        .manage(terminal::TerminalScaleFactorOverride(cli.scale_factor))
        .manage(display::KeyboardTx(Mutex::new(None)))
        .manage(display::DisplayTargetWindow(Mutex::new(MAIN_WINDOW_LABEL.to_string())))
        .manage(display::DisplayGeometryState::default())
        .manage(led_matrix::LedMatrixTargetWindow(Mutex::new(MAIN_WINDOW_LABEL.to_string())))
        .manage(led_matrix::LedMatrixGeometryState::default())
        .manage(led_matrix::LedMatrixFrameCache::default())
        .manage(CpuState(Mutex::new(None)))
        .manage(cpu_bus::UiIrqSourceState(Mutex::new(None)))
        .manage(disassembly::DisassemblerState(Mutex::new(None)))
        .manage(registers::ChangedFlagsState(Mutex::new(0)))
        .manage(disassembly::RunStopperState(Mutex::new(None)))
        .manage(disassembly::SkipBreakpointPc(Mutex::new(None)))
        .manage(breakpoints::BreakpointState(Mutex::new(std::collections::BTreeMap::new())))
        .manage(disassembly::LiveSnapshotRx(Mutex::new(None)))
        .manage(memory::MemoryViewAddr(Arc::new(AtomicU16::new(0))))
        .manage(memory::MemoryViewSeq(AtomicU64::new(0)))
        .manage(cpu_bus::CpuBusCache(Mutex::new(cpu_bus::CpuBusSnapshot {
            irq_active: false,
            nmi_pending: false,
            cycles: 0,
            effective_speed: cpu_bus::EFFECTIVE_SPEED_UNKNOWN.to_string(),
            cpu_stopped: false,
            cpu_waiting: false,
        })))
        .manage(preferences::UiConfigState(Mutex::new(preferences::load_ui_config_from(&config_dir))))
        .manage(layout::LayoutState(Mutex::new(dock_layout)))
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
            } else if event.id() == menu::RELOAD_PROFILE_ID {
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = profile::reload_profile(app_handle).await;
                });
            } else if event.id() == menu::CLEAR_RECENT_ID {
                recent::emit_open_clear_recent_dialog(app);
            } else if event.id() == menu::RESTORE_LAYOUT_ID {
                layout::emit_open_restore_layout_dialog(app);
            } else if event.id() == menu::ABOUT_ID {
                about::emit_open_about_dialog(app);
            } else if event.id() == menu::GITHUB_ID {
                let _ = app.opener().open_url(menu::GITHUB_REPO_URL, None::<&str>);
            } else if let Some(path) = event.id().as_ref().strip_prefix(menu::OPEN_RECENT_ID_PREFIX) {
                let app_handle = app.clone();
                let path = std::path::PathBuf::from(path);
                tauri::async_runtime::spawn(async move {
                    recent::open_recent_profile(app_handle, path).await;
                });
            } else if event.id() == menu::TOGGLE_TERMINAL_ID {
                let detached = app.state::<layout::LayoutState>().0.lock().unwrap().terminal_detached;
                if detached {
                    terminal::reattach_terminal(app);
                } else if let Err(e) = terminal::begin_terminal_detach(app) {
                    eprintln!("Failed to detach terminal: {e}");
                } else {
                    let _ = app.emit_to(MAIN_WINDOW_LABEL, "terminal-detach-requested", ());
                }
            } else if event.id() == menu::TOGGLE_DISPLAY_ID {
                let detached = app.state::<layout::LayoutState>().0.lock().unwrap().display_detached;
                if detached {
                    display::reattach_display(app);
                } else if let Err(e) = display::begin_display_detach(app) {
                    eprintln!("Failed to detach display: {e}");
                } else {
                    let _ = app.emit_to(MAIN_WINDOW_LABEL, "display-detach-requested", ());
                }
            } else if event.id() == menu::TOGGLE_LED_MATRIX_ID {
                let detached = app.state::<layout::LayoutState>().0.lock().unwrap().led_matrix_detached;
                if detached {
                    led_matrix::reattach_led_matrix(app);
                } else if let Err(e) = led_matrix::begin_led_matrix_detach(app) {
                    eprintln!("Failed to detach LED matrix: {e}");
                } else {
                    let _ = app.emit_to(MAIN_WINDOW_LABEL, "led-matrix-detach-requested", ());
                }
            } else if let Some(panel_id) = event.id().as_ref().strip_prefix(menu::VIEW_PANEL_ID_PREFIX) {
                // Terminal, Display, and LED Matrix are all special-cased: while any is detached
                // to its own window, that window (not a dock panel) is the thing to reveal —
                // asking the dock to add a panel that duplicates it would fight the
                // single-source-of-truth detach/reattach design in
                // `terminal.rs`/`display.rs`/`led_matrix.rs`. Every other panel id, and these
                // three while docked, goes through the generic dockview-driven `reveal-panel`
                // handler in `DockLayout.tsx`, which adds the panel back (using its last dock
                // position, or its default position) if it isn't present, or just activates its
                // tab if it is.
                let detached_window_label = {
                    let layout_state = app.state::<layout::LayoutState>();
                    let detached = layout_state.0.lock().unwrap();
                    match panel_id {
                        "terminal" if detached.terminal_detached => Some(terminal::TERMINAL_DETACHED_WINDOW_LABEL),
                        "display" if detached.display_detached => Some(display::DISPLAY_DETACHED_WINDOW_LABEL),
                        "led-matrix" if detached.led_matrix_detached =>
                            Some(led_matrix::LED_MATRIX_DETACHED_WINDOW_LABEL),
                        _ => None,
                    }
                };
                if let Some(label) = detached_window_label {
                    if let Some(window) = app.get_webview_window(label) {
                        let _ = window.set_focus();
                    }
                } else {
                    let _ = app.emit_to(MAIN_WINDOW_LABEL, "reveal-panel", panel_id.to_string());
                }
            } else if matches!(
                event.id().as_ref(),
                menu::RUN_CPU_ID
                    | menu::STOP_CPU_ID
                    | menu::STEP_INTO_ID
                    | menu::STEP_OVER_ID
                    | menu::STEP_RETURN_ID
                    | menu::TOGGLE_AUTO_STEP_ID
            ) {
                // Bring the floating panel back if it's been dismissed, then
                // dispatch the action itself — both routed to
                // `RunControlsContext.tsx`, which owns the actual
                // run/step/auto-step handlers (issue #395). A native
                // accelerator, this menu click, and the panel's own button
                // all end up calling the exact same code.
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "reveal-panel", "run-controls");
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "run-menu-action", event.id().as_ref().to_string());
            } else if matches!(
                event.id().as_ref(),
                menu::LOAD_MEMORY_ID | menu::SAVE_MEMORY_ID | menu::EDIT_MEMORY_ID | menu::FILL_MEMORY_ID
            ) {
                // Same pattern as the Run menu above (issue #411): bring the
                // Memory panel back if it's been dismissed, then dispatch the
                // action itself to `MemoryPanel.tsx`, which owns the actual
                // dialog-opening logic that used to live behind its own
                // header buttons.
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "reveal-panel", "memory");
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "memory-menu-action", event.id().as_ref().to_string());
            } else if matches!(
                event.id().as_ref(),
                menu::NEW_ASSEMBLER_ID
                    | menu::OPEN_ASSEMBLER_ID
                    | menu::SAVE_ASSEMBLER_ID
                    | menu::SAVE_AS_ASSEMBLER_ID
                    | menu::ASSEMBLE_LOAD_ID
            ) {
                // Same pattern as the Memory menu above (issue #474, debugger
                // integration Unit 4): bring the Assembler panel back if it's
                // been dismissed, then dispatch the action itself to
                // `AssemblerPanel.tsx`, which owns the actual file-dialog/
                // dirty-tracking/assemble logic.
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "reveal-panel", "assembler");
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "assembler-menu-action", event.id().as_ref().to_string());
            } else if matches!(event.id().as_ref(), menu::CUT_ID | menu::COPY_ID | menu::PASTE_ID) {
                // No panel to reveal here (issue #435) — `EditMenuContext.tsx`
                // acts against whatever is currently focused/selected in the
                // main window, wherever that is, rather than a single owning
                // panel like the Run/Memory menus dispatch to.
                let _ = app.emit_to(MAIN_WINDOW_LABEL, "edit-menu-action", event.id().as_ref().to_string());
            }
        })
        .invoke_handler(tauri::generate_handler![
            quit,
            confirm_exit,
            profile::create_profile,
            profile::list_templates,
            profile::open_profile,
            profile::reload_profile,
            get_session_status,
            terminal::write_terminal,
            terminal::get_terminal_history,
            terminal::detach_terminal,
            terminal::attach_terminal,
            terminal::get_terminal_scale_factor_override,
            display::write_keyboard,
            display::detach_display,
            display::attach_display,
            display::get_display_geometry,
            led_matrix::detach_led_matrix,
            led_matrix::attach_led_matrix,
            led_matrix::get_led_matrix_geometry,
            led_matrix::get_led_matrix_frames,
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
            assembler::assemble_preview,
            assembler::assemble_and_load,
            assembler::read_source_file,
            assembler::write_source_file,
            stack::get_stack,
            symbols::get_symbols,
            breakpoints::toggle_breakpoint,
            breakpoints::set_breakpoint,
            breakpoints::remove_breakpoint,
            breakpoints::disable_breakpoint,
            breakpoints::enable_breakpoint,
            breakpoints::get_breakpoints,
            cpu_bus::get_cpu_bus_state,
            resolve_symbol,
            memory::get_symbols_for_range,
            preferences::get_theme,
            preferences::set_theme,
            preferences::get_last_file_dialog_dir,
            preferences::get_terminal_preferences,
            preferences::set_terminal_preferences,
            preferences::set_last_file_dialog_dir,
            preferences::get_symbols_column_widths,
            preferences::set_symbols_column_widths,
            layout::get_dock_layout,
            layout::set_dock_layout,
            layout::restore_dock_layout,
            watchpoints::get_watchpoints,
            watchpoints::add_watchpoint,
            watchpoints::remove_watchpoint,
            watchpoints::edit_watchpoint,
            watchpoints::toggle_watchpoint,
            recent::clear_recent_profiles,
            menu::set_run_controls_enabled,
            menu::set_memory_menu_enabled,
            menu::set_assembler_menu_enabled,
            menu::set_edit_menu_enabled,
            menu::set_profile_menu_enabled,
            menu::set_recent_menu_enabled,
            about::get_about_info,
        ])
        .setup(move |app| {
            let (
                app_menu,
                window_menu_state,
                recent_menu_state,
                run_menu_state,
                memory_menu_state,
                assembler_menu_state,
                edit_menu_state,
                profile_menu_state,
            ) = menu::build_menu(app)?;
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

            // The main window starts hidden (`visible: false` in
            // tauri.conf.json) specifically so its saved geometry (issue
            // #419) can be applied before it's ever shown, avoiding a
            // visible jump from the configured default size/position to the
            // restored one.
            if let Some(main_window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let geometry = app.state::<preferences::UiConfigState>().0.lock().unwrap().main_window_geometry;
                if let Some(geometry) = geometry {
                    preferences::apply_window_geometry(&main_window, &geometry);
                }
                let _ = main_window.show();
            }

            app.manage(window_menu_state);
            app.manage(recent_menu_state);
            app.manage(run_menu_state);
            app.manage(memory_menu_state);
            app.manage(assembler_menu_state);
            app.manage(edit_menu_state);
            app.manage(profile_menu_state);

            // Detached-Terminal window: strip its menu and install the
            // close-hides-and-reattaches lifecycle once, regardless of
            // whether it's ever actually detached this run (see
            // `install_detached_window`'s doc comment). If the persisted
            // layout says Terminal was left detached last time the app
            // exited, reopen it now rather than silently re-docking it
            // (issue #385's risk #3) — `DockLayout.tsx`'s own restore logic
            // independently consults the same flag to skip re-adding the
            // dock panel, so this only needs to handle the window side.
            terminal::install_detached_window(app.handle());
            terminal::restore_detached_window_if_needed(app.handle(), terminal_was_detached);
            display::install_detached_window(app.handle());
            display::restore_detached_window_if_needed(app.handle(), display_was_detached);
            led_matrix::install_detached_window(app.handle());
            led_matrix::restore_detached_window_if_needed(app.handle(), led_matrix_was_detached);

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

            // Starts loading the session immediately, without waiting for the
            // terminal panel to mount: unlike the old standalone Terminal
            // window, the panel lives inside the main window's dockview
            // instance, which doesn't render until the session itself is
            // ready — waiting here would deadlock. Console output produced
            // before any panel mounts is retained regardless (see
            // `terminal::TerminalHistory`), so nothing is lost.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                load_or_reload_session(&handle, &profile_dir).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
