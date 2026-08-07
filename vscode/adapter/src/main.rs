use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use dap::prelude::*;
use dap::events::StoppedEventBody;
use dap::types::{Capabilities, StoppedEventReason};
use emma65::emulator::Cpu;

mod session;

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut server = Server::new(BufReader::new(stdin.lock()), BufWriter::new(stdout.lock()));
    let runtime = tokio::runtime::Runtime::new().expect("emma65-vscode-adapter: failed to start async runtime");

    let mut cpu: Option<Cpu> = None;

    loop {
        let request = match server.poll_request() {
            Ok(Some(request)) => request,
            Ok(None) => break,
            Err(e) => {
                eprintln!("emma65-vscode-adapter: {e}");
                break;
            }
        };

        if !handle_request(&mut server, &runtime, &mut cpu, request) {
            break;
        }
    }
}

/// Dispatches a single DAP request, returning `false` once the session should end
/// (`disconnect`/`terminate`).
fn handle_request(
    server: &mut Server<impl Read, impl Write>,
    runtime: &tokio::runtime::Runtime,
    cpu: &mut Option<Cpu>,
    request: Request,
) -> bool {
    match &request.command {
        Command::Initialize(_) => {
            let capabilities = Capabilities {
                supports_configuration_done_request: Some(true),
                ..Default::default()
            };
            let _ = server.respond(request.success(ResponseBody::Initialize(capabilities)));
            let _ = server.send_event(Event::Initialized);
        }
        Command::Launch(args) => {
            let config_path = args
                .additional_data
                .as_ref()
                .and_then(|data| data.get("configPath"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from);

            match runtime.block_on(session::build_session(config_path.as_deref())) {
                Ok(new_cpu) => {
                    *cpu = Some(new_cpu);
                    let _ = server.respond(request.ack().expect("launch is ack-able"));
                }
                Err(message) => {
                    let _ = server.respond(request.error(&message));
                }
            }
        }
        Command::ConfigurationDone => {
            let _ = server.respond(request.ack().expect("configurationDone is ack-able"));
            if cpu.is_some() {
                let _ = server.send_event(Event::Stopped(StoppedEventBody {
                    reason: StoppedEventReason::Entry,
                    description: None,
                    thread_id: Some(1),
                    preserve_focus_hint: None,
                    text: None,
                    all_threads_stopped: Some(true),
                    hit_breakpoint_ids: None,
                }));
            }
        }
        Command::Disconnect(_) => {
            let _ = server.respond(request.ack().expect("disconnect is ack-able"));
            *cpu = None;
            return false;
        }
        Command::Terminate(_) => {
            let _ = server.respond(request.ack().expect("terminate is ack-able"));
            let _ = server.send_event(Event::Terminated(None));
            *cpu = None;
            return false;
        }
        _ => {
            let _ = server.respond(request.error("unsupported request"));
        }
    }
    true
}
