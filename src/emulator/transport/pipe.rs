//! Transport that connects a device to the stdin/stdout of a child process.
//!
//! Spawns a command and bridges the device's byte stream to the child's stdin
//! (device → child) and the child's stdout (child → device). The child's
//! stderr is inherited from the emulator process. When the child exits for any
//! reason, the supplied `on_exit` callback is called with a describing
//! [`io::Error`] so the event can be surfaced as an emulator-level error.

use std::io;
use std::process::Stdio;

use crossbeam_channel::{Receiver, Sender};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::oneshot;

use super::{ChannelBridge, Transport, TransportError, TransportEvent};

/// Transport that connects a device to the stdin/stdout of a child process.
///
/// Created via [`PipeTransport::spawn`]. The child's stderr is inherited from
/// the emulator process. Any exit of the child process — normal or otherwise —
/// triggers the `on_exit` callback supplied at construction.
///
/// `Connected(0)` and `Disconnected(0)` events are emitted by the background
/// task and flow through the bridge unchanged; `is_connected` tracks the latest
/// state seen via `try_recv_tagged` or `try_recv`.
pub struct PipeTransport {
    bridge: ChannelBridge<TransportEvent>,
    connected: bool,
}

impl PipeTransport {
    /// Spawns `command[0]` with `command[1..]` as arguments, connecting its
    /// stdin/stdout to this transport. `on_exit` is called exactly once when
    /// the child process exits or its IO fails.
    pub async fn spawn<F>(command: &[String], on_exit: F) -> io::Result<Self>
    where
        F: FnOnce(io::Error) + Send + 'static,
    {
        if command.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "command must not be empty"));
        }
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child.stdin.take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "child stdin unavailable"))?;
        let stdout = child.stdout.take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "child stdout unavailable"))?;

        let (bridge, in_tx, out_rx, shutdown_rx) = ChannelBridge::<TransportEvent>::new();

        tokio::spawn(run_pipe_task(stdin, stdout, child, in_tx, out_rx, shutdown_rx, on_exit));

        Ok(Self { bridge, connected: true })
    }

}

impl Transport for PipeTransport {
    fn try_recv(&mut self) -> Option<u8> {
        loop {
            match self.bridge.try_recv()? {
                TransportEvent::Data(_, byte) => return Some(byte),
                TransportEvent::Disconnected(_) => {
                    self.connected = false;
                    return None;
                }
                TransportEvent::Connected(_) => continue,
            }
        }
    }

    fn send(&mut self, byte: u8) -> Result<(), TransportError> {
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        self.bridge.send(byte)
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn try_recv_tagged(&mut self) -> Option<TransportEvent> {
        let event = self.bridge.try_recv()?;
        if matches!(event, TransportEvent::Disconnected(_)) {
            self.connected = false;
        }
        Some(event)
    }

    fn shutdown(&mut self) {
        self.bridge.shutdown();
        self.connected = false;
    }
}

/// Tokio task: bridges child process stdin/stdout to the sync `ChannelBridge`.
///
/// Sends `Connected(0)` immediately, then relays bytes between the child and
/// the bridge. On any exit — IO error, child process termination, or shutdown
/// signal — calls `on_exit` with a describing error and sends `Disconnected(0)`.
async fn run_pipe_task<F>(
    mut stdin: tokio::process::ChildStdin,
    mut stdout: tokio::process::ChildStdout,
    mut child: tokio::process::Child,
    in_tx: Sender<TransportEvent>,
    out_rx: Receiver<u8>,
    mut shutdown_rx: oneshot::Receiver<()>,
    on_exit: F,
) where
    F: FnOnce(io::Error) + Send + 'static,
{
    if in_tx.send(TransportEvent::Connected(0)).is_err() {
        return;
    }

    let exit_error = loop {
        let mut buf = [0u8; 1];
        tokio::select! {
            _ = &mut shutdown_rx => {
                break io::Error::new(io::ErrorKind::Interrupted, "transport shut down");
            }

            result = stdout.read(&mut buf) => match result {
                Ok(1) => {
                    if in_tx.send(TransportEvent::Data(0, buf[0])).is_err() {
                        break io::Error::new(io::ErrorKind::BrokenPipe, "device channel closed");
                    }
                }
                Ok(_) => {
                    // stdout closed; wait for the process to fully exit
                    let status = child.wait().await;
                    break match status {
                        Ok(s) if s.success() => {
                            io::Error::new(io::ErrorKind::UnexpectedEof, "child process exited")
                        }
                        Ok(s) => {
                            io::Error::other(format!("child process exited with {s}"))
                        }
                        Err(e) => e,
                    };
                }
                Err(e) => break e,
            },

            _ = drain_outbound(&mut stdin, &out_rx) => {}
        }
    };

    on_exit(exit_error);
    let _ = in_tx.send(TransportEvent::Disconnected(0));
}

async fn drain_outbound(stdin: &mut tokio::process::ChildStdin, out_rx: &Receiver<u8>) {
    while let Ok(byte) = out_rx.try_recv() {
        if stdin.write_all(&[byte]).await.is_err() {
            return;
        }
    }
    tokio::task::yield_now().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn spawn_cat_and_echo_byte() {
        let received_exit = Arc::new(Mutex::new(None::<String>));
        let received_exit_clone = Arc::clone(&received_exit);

        let mut transport = PipeTransport::spawn(
            &["cat".to_string()],
            move |e| *received_exit_clone.lock().unwrap() = Some(e.to_string()),
        ).await.unwrap();

        // Allow the Tokio task to send the Connected event
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        transport.send(0x42).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv(), Some(0x42));
    }

    #[tokio::test]
    async fn try_recv_tagged_emits_connected_before_data() {
        let mut transport = PipeTransport::spawn(
            &["cat".to_string()],
            |_| {},
        ).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert_eq!(transport.try_recv_tagged(), Some(TransportEvent::Connected(0)));

        transport.send(0x7A).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv_tagged(), Some(TransportEvent::Data(0, 0x7A)));
    }

    #[tokio::test]
    async fn exit_calls_on_exit_callback() {
        let received_exit = Arc::new(Mutex::new(false));
        let received_exit_clone = Arc::clone(&received_exit);

        // `true` exits immediately with status 0
        let _transport = PipeTransport::spawn(
            &["true".to_string()],
            move |_| *received_exit_clone.lock().unwrap() = true,
        ).await.unwrap();

        // Give the task time to detect child exit and call on_exit
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(*received_exit.lock().unwrap(), "on_exit should have been called");
    }

    #[tokio::test]
    async fn shutdown_marks_disconnected() {
        let mut transport = PipeTransport::spawn(
            &["cat".to_string()],
            |_| {},
        ).await.unwrap();

        transport.shutdown();
        assert!(!transport.is_connected());
    }
}
