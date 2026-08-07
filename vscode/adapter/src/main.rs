use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use dap::prelude::*;
use dap::events::StoppedEventBody;
use dap::responses::ThreadsResponse;
use dap::types::{Capabilities, StoppedEventReason, Thread};

mod exec;
mod session;

fn main() {
    let stdin = std::io::stdin();
    // `Stdout` (not `.lock()`) so the write half is `Send`: background threads
    // (`exec::continue_cpu` and friends) hold a clone of `server.output` and send
    // `stopped` events once their run/step completes.
    let mut server = Server::new(BufReader::new(stdin.lock()), BufWriter::new(std::io::stdout()));
    let runtime = Arc::new(
        tokio::runtime::Runtime::new().expect("emma65-vscode-adapter: failed to start async runtime"),
    );

    let state = exec::ExecState::default();

    loop {
        let request = match server.poll_request() {
            Ok(Some(request)) => request,
            Ok(None) => break,
            Err(e) => {
                eprintln!("emma65-vscode-adapter: {e}");
                break;
            }
        };

        if !handle_request(&mut server, &runtime, &state, request) {
            break;
        }
    }
}

/// Dispatches a single DAP request, returning `false` once the session should end
/// (`disconnect`/`terminate`).
fn handle_request<W: Write + Send + 'static>(
    server: &mut Server<impl Read, W>,
    runtime: &Arc<tokio::runtime::Runtime>,
    state: &exec::ExecState,
    request: Request,
) -> bool {
    match &request.command {
        Command::Initialize(_) => {
            let capabilities = Capabilities {
                supports_configuration_done_request: Some(true),
                supports_restart_request: Some(true),
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
                    state.set_cpu(new_cpu);
                    let _ = server.respond(request.ack().expect("launch is ack-able"));
                }
                Err(message) => {
                    let _ = server.respond(request.error(&message));
                }
            }
        }
        Command::ConfigurationDone => {
            let _ = server.respond(request.ack().expect("configurationDone is ack-able"));
            if state.has_cpu() {
                let _ = server.send_event(Event::Stopped(StoppedEventBody {
                    reason: StoppedEventReason::Entry,
                    description: None,
                    thread_id: Some(exec::THREAD_ID),
                    preserve_focus_hint: None,
                    text: None,
                    all_threads_stopped: Some(true),
                    hit_breakpoint_ids: None,
                }));
            }
        }
        Command::Threads => {
            let threads = vec![Thread { id: exec::THREAD_ID, name: "CPU".to_string() }];
            let _ = server.respond(request.success(ResponseBody::Threads(ThreadsResponse { threads })));
        }
        Command::Continue(_) => {
            match exec::continue_cpu(state, runtime, server.output.clone()) {
                Ok(body) => {
                    let _ = server.respond(request.success(ResponseBody::Continue(body)));
                }
                Err(message) => {
                    let _ = server.respond(request.error(&message));
                }
            }
        }
        Command::Pause(_) => match exec::pause(state) {
            Ok(()) => {
                let _ = server.respond(request.ack().expect("pause is ack-able"));
            }
            Err(message) => {
                let _ = server.respond(request.error(&message));
            }
        },
        Command::Next(_) => match exec::step_over(state, server.output.clone()) {
            Ok(()) => {
                let _ = server.respond(request.ack().expect("next is ack-able"));
            }
            Err(message) => {
                let _ = server.respond(request.error(&message));
            }
        },
        Command::StepIn(_) => match exec::step_into(state) {
            Ok(reason) => {
                let _ = server.respond(request.ack().expect("stepIn is ack-able"));
                exec::send_stopped(&server.output, reason);
            }
            Err(message) => {
                let _ = server.respond(request.error(&message));
            }
        },
        Command::StepOut(_) => match exec::step_return(state, server.output.clone()) {
            Ok(()) => {
                let _ = server.respond(request.ack().expect("stepOut is ack-able"));
            }
            Err(message) => {
                let _ = server.respond(request.error(&message));
            }
        },
        Command::Restart(_) => match exec::restart(state) {
            Ok(reason) => {
                // Not `request.ack()`: dap 0.4.1-alpha1's ack() mislabels Restart's
                // response body as `ResponseBody::Next` instead of `Restart`, which
                // would serialize the response's `command` field as "next".
                let _ = server.respond(request.success(ResponseBody::Restart));
                if let Some(reason) = reason {
                    exec::send_stopped(&server.output, reason);
                }
            }
            Err(message) => {
                let _ = server.respond(request.error(&message));
            }
        },
        Command::Disconnect(_) => {
            let _ = server.respond(request.ack().expect("disconnect is ack-able"));
            state.clear_cpu();
            return false;
        }
        Command::Terminate(_) => {
            let _ = server.respond(request.ack().expect("terminate is ack-able"));
            let _ = server.send_event(Event::Terminated(None));
            state.clear_cpu();
            return false;
        }
        _ => {
            let _ = server.respond(request.error("unsupported request"));
        }
    }
    true
}
