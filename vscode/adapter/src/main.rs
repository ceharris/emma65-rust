use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use dap::prelude::*;
use dap::events::StoppedEventBody;
use dap::responses::{
    DisassembleResponse, ScopesResponse, SetInstructionBreakpointsResponse, SetVariableResponse, StackTraceResponse,
    ThreadsResponse, VariablesResponse,
};
use dap::types::{Capabilities, StackFrame, StoppedEventReason, Thread};

mod breakpoints;
mod disasm;
mod exec;
mod memory;
mod registers;
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
                supports_disassemble_request: Some(true),
                supports_instruction_breakpoints: Some(true),
                supports_set_variable: Some(true),
                supports_read_memory_request: Some(true),
                supports_write_memory_request: Some(true),
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
        Command::StackTrace(_) => {
            // A single synthetic frame with no `source`: this emulator has no source-level
            // debug info (see the plan's "Deferred" section), and an absent `source` plus
            // `instruction_pointer_reference` is what tells VS Code to land on its built-in
            // Disassembly View instead of a source editor.
            match state.with_cpu(|cpu| cpu.registers().pc) {
                Some(pc) => {
                    let frame = StackFrame {
                        id: exec::THREAD_ID,
                        name: "CPU".to_string(),
                        instruction_pointer_reference: Some(format!("0x{pc:04X}")),
                        ..Default::default()
                    };
                    let body = StackTraceResponse { stack_frames: vec![frame], total_frames: Some(1) };
                    let _ = server.respond(request.success(ResponseBody::StackTrace(body)));
                }
                None => {
                    let _ = server.respond(request.error("CPU not ready"));
                }
            }
        }
        Command::Scopes(_) => {
            let scopes = if state.has_cpu() { registers::scopes() } else { vec![] };
            let _ = server.respond(request.success(ResponseBody::Scopes(ScopesResponse { scopes })));
        }
        Command::Variables(args) => {
            if args.variables_reference != registers::REGISTERS_VARIABLES_REFERENCE {
                let _ = server.respond(request.error("unknown variablesReference"));
            } else {
                match state.with_cpu(registers::variables) {
                    Some(variables) => {
                        let _ = server.respond(request.success(ResponseBody::Variables(VariablesResponse { variables })));
                    }
                    None => {
                        let _ = server.respond(request.error("CPU not ready"));
                    }
                }
            }
        }
        Command::SetVariable(args) => {
            if args.variables_reference != registers::REGISTERS_VARIABLES_REFERENCE {
                let _ = server.respond(request.error("unknown variablesReference"));
            } else {
                match state.with_cpu_mut(|cpu| registers::set_variable(cpu, &args.name, &args.value)) {
                    Some(Ok(value)) => {
                        let body = SetVariableResponse { value, ..Default::default() };
                        let _ = server.respond(request.success(ResponseBody::SetVariable(body)));
                    }
                    Some(Err(message)) => {
                        let _ = server.respond(request.error(&message));
                    }
                    None => {
                        let _ = server.respond(request.error("CPU not ready"));
                    }
                }
            }
        }
        Command::Disassemble(args) => match state.with_cpu(|cpu| disasm::disassemble(cpu, args)) {
            Some(Ok(instructions)) => {
                let _ = server.respond(request.success(ResponseBody::Disassemble(DisassembleResponse { instructions })));
            }
            Some(Err(message)) => {
                let _ = server.respond(request.error(&message));
            }
            None => {
                let _ = server.respond(request.error("CPU not ready"));
            }
        },
        Command::ReadMemory(args) => match state.with_cpu(|cpu| memory::read_memory(cpu, args)) {
            Some(Ok(response)) => {
                let _ = server.respond(request.success(ResponseBody::ReadMemory(response)));
            }
            Some(Err(message)) => {
                let _ = server.respond(request.error(&message));
            }
            None => {
                let _ = server.respond(request.error("CPU not ready"));
            }
        },
        Command::WriteMemory(args) => match state.with_cpu_mut(|cpu| memory::write_memory(cpu, args)) {
            Some(Ok(response)) => {
                let _ = server.respond(request.success(ResponseBody::WriteMemory(response)));
            }
            Some(Err(message)) => {
                let _ = server.respond(request.error(&message));
            }
            None => {
                let _ = server.respond(request.error("CPU not ready"));
            }
        },
        Command::SetInstructionBreakpoints(args) => {
            match state.with_cpu_mut(|cpu| breakpoints::set_instruction_breakpoints(cpu, args)) {
                Some(Ok(breakpoints)) => {
                    let body = SetInstructionBreakpointsResponse { breakpoints };
                    let _ = server.respond(request.success(ResponseBody::SetInstructionBreakpoints(body)));
                }
                Some(Err(message)) => {
                    let _ = server.respond(request.error(&message));
                }
                None => {
                    let _ = server.respond(request.error("CPU not ready"));
                }
            }
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
