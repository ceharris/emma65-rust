mod config;
mod tty;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use crate::config::{AppConfig, apply_default_if_unconfigured};
use emma65::emulator::cpu::StepResult;
use emma65::emulator::{DeviceEvent, InstantiationContext, InternalPipeTransport, Transport, TransportReporter};

#[tokio::main]
async fn main() -> ExitCode {
    env_logger::init();
    let mut config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    // Hold the tempdir reference until after build so the default config's files aren't deleted too early.
    let _default_config_dir = apply_default_if_unconfigured(&mut config);
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
    let context = InstantiationContext {
        clock_hz: config.emulator.clock_speed_hz,
        error_sender: None,
        console_transport: Some(Arc::clone(&console_transport_slot)),
    };
    let session = match config.emulator.build_with_context(&registry, context).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("startup error: {e}");
            std::process::exit(1);
        }
    };

    let (mut cpu, mut error_receiver) = (session.cpu, session.error_receiver);
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
            event = error_receiver.recv(), if events_open => match event {
                Some(DeviceEvent::TransportError { device, error}) =>
                    eprintln!("device {}: transport error: {}", device.0, error),
                Some(DeviceEvent::TransportDisconnected { device, peer, reason}) =>
                    match peer {
                        Some(peer) => eprintln!("device {} disconnected: {} ({})", device.0, reason, peer),
                        None => eprintln!("device {} disconnected: {}", device.0, reason),
                    },
                Some(DeviceEvent::TransportConnected { device, peer }) =>
                    match peer {
                        Some(peer) => println!("device {} connected: {}", device.0, peer),
                        None => println!("device {} connected", device.0),
                    },
                Some(DeviceEvent::DeviceInfo { device, message}) =>
                    eprintln!("device {}: {}", device.0, message),
                Some(DeviceEvent::RejectedWrite { device, address }) =>
                    eprintln!("device rejected write {}: at address {}", device.0, address),
                Some(DeviceEvent::OutboundBytesDropped { device, count }) =>
                    eprintln!("device {}: {} outbound bytes dropped", device.0, count),
                Some(DeviceEvent::InboundEventsDropped { device, count }) =>
                    eprintln!("device {}: {} inbound events dropped", device.0, count),
                None => events_open = false,      // all senders dropped
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

    print!("\r\n");     // canonical newline to delineate emulator output from user's shell prompt

    // Falling off the end here (rather than calling std::process::exit) lets `_raw_mode_guard`
    // drop normally, restoring the terminal before the process actually exits.
    exit_code
}
