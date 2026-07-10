mod config;
mod tty;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use crate::config::{AppConfig, apply_default_if_unconfigured};
use emma65::emulator::{DeviceEvent, InstantiationContext, PipeTransport, StepResult, Transport};

const DEFAULT_ROM: &[u8] = include_bytes!("default.bin");

#[tokio::main]
async fn main() -> ExitCode {
    env_logger::init();
    let mut config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    let default_rom_file = apply_default_if_unconfigured(&mut config, DEFAULT_ROM);
    let registry = emma65::emulator::DeviceRegistry::with_builtins();

    // The default (no-config) layout has no `transport=` attribute on its console device;
    // attach it directly to this process's stdin/stdout instead.
    let session = if default_rom_file.is_some() {
        let transport = PipeTransport::stdio().unwrap_or_else(|e| {
            eprintln!("error: failed to attach console to stdin/stdout: {e}");
            std::process::exit(1);
        });
        let context = InstantiationContext {
            clock_hz: config.emulator.clock_speed_hz,
            error_sender: None,
            console_transport: Some(Arc::new(Mutex::new(Some(Box::new(transport) as Box<dyn Transport>)))),
        };
        config.emulator.build_with_context(&registry, context).await
    } else {
        config.emulator.build(&registry).await
    };
    let session = match session {
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

    // Only enter raw mode once startup has fully succeeded, and only when the console is
    // attached to this terminal, so no error exit above ever needs to restore it first.
    let _raw_mode_guard = if default_rom_file.is_some() {
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
                Some(DeviceEvent::TransportDisconnected { device, reason}) =>
                    eprintln!("device {} disconnected: {}", device.0, reason),
                Some(DeviceEvent::TransportConnected { device }) =>
                    println!("device {} connected", device.0),
                Some(DeviceEvent::DeviceInfo { device, message}) =>
                    eprintln!("device {}: {}", device.0, message),
                Some(DeviceEvent::RejectedWrite { device, address }) =>
                    eprintln!("device rejected write {}: at address {}", device.0, address),
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

    print!("\r\n");     // canonical newline to delineate emulator output from user's shell prompt

    // Falling off the end here (rather than calling std::process::exit) lets `_raw_mode_guard`
    // drop normally, restoring the terminal before the process actually exits.
    exit_code
}
