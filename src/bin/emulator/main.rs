mod config;

use crate::config::AppConfig;
use emma65::emulator::{DeviceEvent, StepResult};

#[tokio::main]
async fn main() {
    let config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    let registry = emma65::emulator::DeviceRegistry::with_builtins();
    let session = match config.emulator.build(&registry).await {
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
    let run_handle = emma65::emulator::run(cpu);
    let (cpu_done_tx, mut cpu_done_rx) = tokio::sync::oneshot::channel::<StepResult>();
    tokio::spawn(async move {
       let _ = cpu_done_tx.send(run_handle.wait().await);
    });

    let mut events_open = true;
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
                None => events_open = false,      // all senders dropped
            },

            result = &mut cpu_done_rx => {
                match result.unwrap_or(StepResult::Stopped) {
                    StepResult::Error(e) => {
                        eprintln!("CPU error: {e}");
                        std::process::exit(1);
                    }
                    _ => break,
                }
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }

}


