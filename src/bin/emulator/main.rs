mod config;
mod tty;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use crate::config::{AppConfig, apply_default_if_unconfigured, apply_named_profile};
use emma65::emulator::cpu::StepResult;
use emma65::emulator::{InstantiationContext, InternalPipeTransport, Transport, TransportReporter, log_device_event};

#[tokio::main]
async fn main() -> ExitCode {
    // The console, when attached to this process's own stdio, puts the terminal into raw mode
    // (see tty::enter_raw_mode), which disables OPOST — so a bare '\n' no longer returns the
    // cursor to column 0. env_logger's default suffix is just '\n'; force '\r\n' so log lines
    // (e.g. TRANSPORT connect/disconnect events, which arrive after raw mode is entered) still
    // render as proper new lines. Harmless when raw mode isn't in effect: the terminal treats a
    // leading '\r' before '\n' as a no-op cursor return.
    let mut log_format = env_logger::fmt::ConfigurableFormat::default();
    log_format.suffix("\r\n");
    env_logger::Builder::from_default_env().format(move |buf, record| log_format.format(buf, record)).init();
    let loaded = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    let mut config = loaded.app;
    // Hold the tempdir reference until after build so its files aren't deleted too early.
    let _profile_dir = match loaded.profile {
        Some(id) => Some(apply_named_profile(&mut config, &id).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        })),
        None => apply_default_if_unconfigured(&mut config),
    };
    let registry = emma65::emulator::DeviceRegistry::with_builtins();

    // Always offer stdin/stdout to the console via the context. If the console has no
    // `transport=` attribute it will take this transport; if it does have one it will ignore it.
    // Checking whether the slot was consumed after build tells us whether to enter raw mode.
    //
    // No `DeviceId` exists yet at this point (the console's isn't allocated until
    // `ConsoleModule::instantiate` runs deep inside `build_with_context` below), so the
    // reporter starts unbound; `ConsoleModule::instantiate` binds it once the id is known.
    let reporter = TransportReporter::pending(None);
    let (transport, relay) = InternalPipeTransport::stdio(reporter.clone()).unwrap_or_else(|e| {
        eprintln!("error: failed to attach console to stdin/stdout: {e}");
        std::process::exit(1);
    });
    let console_transport_slot = Arc::new(Mutex::new(Some((Box::new(transport) as Box<dyn Transport>, relay, reporter))));

    // Always one concrete `LogSender`, cloned into `context`, the CPU, and the event-logging
    // loop below, so every clone shares the same underlying cycle-count `Arc` (see
    // `LogSender::set_cycles`) — without a single shared instance, each of those independently
    // defaulted senders would carry its own always-zero counter, even on the `log`-crate
    // fallback path used when no `--log-file` is configured.
    let (log_sender, log_writer_handle) = match config.log_file.as_ref() {
        Some(path) => {
            let file = std::fs::File::create(path).unwrap_or_else(|e| {
                eprintln!("error: failed to open log file {}: {e}", path.display());
                std::process::exit(1);
            });
            let (sender, handle) = emma65::emulator::spawn_log_writer(file, 4096);
            (sender, Some(handle))
        }
        None => (emma65::emulator::LogSender::default(), None),
    };

    let context = InstantiationContext {
        clock_hz: config.emulator.clock_speed_hz,
        error_sender: None,
        console_transport: Some(Arc::clone(&console_transport_slot)),
        log_sender: Some(log_sender.clone()),
    };
    let session = match config.emulator.build_with_context(&registry, context).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("startup error: {e}");
            std::process::exit(1);
        }
    };

    let (mut cpu, mut error_receiver) = (session.cpu, session.error_receiver);
    let log_sender_for_events = log_sender.clone();
    cpu.set_log_sender(log_sender);
    if let Err(e) = cpu.reset() {
        eprintln!("reset error: {e}");
        std::process::exit(1);
    }

    let trace_writer_handle = match &config.trace_file {
        Some(path) => {
            let file = std::fs::File::create(path).unwrap_or_else(|e| {
                eprintln!("error: failed to open trace file {}: {e}", path.display());
                std::process::exit(1);
            });
            let writer = emma65::emulator::BinaryTraceWriter::new(file, cpu.variant());
            let (callback, handle, _dropped) = emma65::emulator::spawn_trace_writer(
                writer,
                4096,
                emma65::emulator::OverflowPolicy::DropOnFull,
            );
            cpu.set_trace_callback(Some(Box::new(callback)));
            Some(handle)
        }
        None => None,
    };

    // Enter raw mode only if the console took the stdio transport — and only after startup has
    // fully succeeded, so no error exit above ever needs to restore the terminal first.
    let stdio_in_use = console_transport_slot.lock().is_ok_and(|slot| slot.is_none());
    let _raw_mode_guard = if stdio_in_use {
        tty::enter_raw_mode()
    } else {
        None
    };

    let run_handle = emma65::emulator::run(cpu);
    let (cpu_done_tx, mut cpu_done_rx) = tokio::sync::oneshot::channel::<StepResult>();
    tokio::spawn(async move {
       let _ = cpu_done_tx.send(run_handle.wait().await);
    });

    let mut events_open = true;
    let mut exit_code = ExitCode::SUCCESS;
    loop {
        tokio::select! {
            event = error_receiver.recv(), if events_open => {
                match &event {
                    Some(event) => log_device_event(&log_sender_for_events, event),
                    None => events_open = false,      // all senders dropped
                }
            },

            result = &mut cpu_done_rx => {
                if let StepResult::Error(e) = result.unwrap_or(StepResult::Stopped) {
                    eprintln!("CPU error: {e}");
                    exit_code = ExitCode::FAILURE;
                }
                break;
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    if let Some(handle) = trace_writer_handle {
        let _ = handle.join();
    }
    if let Some(handle) = log_writer_handle {
        let _ = handle.join();
    }

    print!("\r\n");     // canonical newline to delineate emulator output from user's shell prompt

    // Falling off the end here (rather than calling std::process::exit) lets `_raw_mode_guard`
    // drop normally, restoring the terminal before the process actually exits.
    exit_code
}
