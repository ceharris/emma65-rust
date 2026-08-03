//! Transport that connects a device to the stdin/stdout of a child process.
//!
//! Spawns a command and bridges the device's byte stream to the child's stdin
//! (device → child) and the child's stdout (child → device), following the
//! same relay shape as [`PtyTransport`](super::PtyTransport) (transport relay
//! redesign plan, §4.3): a Tokio task reads `stdout` and pushes bytes into a
//! plain `crossbeam_channel`, and a [`ChannelRelay<u8>`](ChannelRelay) relays
//! those into an `rtrb` ring for the caller to drain. Outbound bytes go the
//! other way: the transport pushes into an `rtrb::Producer<u8>` (never
//! blocking; overflow is counted via `TransportReporter`, not surfaced as an
//! error, per the module's shutdown/error-reporting contract), and the same
//! Tokio task drains the matching `Consumer<u8>` and writes to the child's
//! `stdin`. The child's stderr is inherited from the emulator process. When
//! the child exits for any reason, the supplied `on_exit` callback is called
//! with a describing [`io::Error`] so the event can be surfaced as an
//! emulator-level error.

use std::io;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Sender, bounded};
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{Notify, oneshot};

use super::{CHANNEL_CAPACITY, ChannelRelay, Transport, TransportError, TransportReporter};

/// Transport that connects a device to the stdin/stdout of a child process.
///
/// Created via [`PipeTransport::spawn`]. The child's stderr is inherited from
/// the emulator process. Any exit of the child process — normal or otherwise —
/// triggers the `on_exit` callback supplied at construction.
pub struct PipeTransport {
    /// Outbound ring producer; see the module doc for the outbound path.
    outbound: Producer<u8>,
    outbound_notify: Arc<Notify>,
    /// One-shot signal sent to the Tokio task to request shutdown.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Reflects whether the child process is still alive, shared with the
    /// Tokio task. Unlike `PtyTransport`, `send()` gates on this: once the
    /// child has exited, its stdin pipe genuinely has no reader left, so
    /// skipping the write avoids a doomed syscall on every subsequent call.
    connected: Arc<AtomicBool>,
    /// Clone of the reporter supplied to `spawn`, for `send()`'s own
    /// `note_outbound_drop` call.
    reporter: TransportReporter,
}

impl PipeTransport {
    /// Spawns `command[0]` with `command[1..]` as arguments, connecting its
    /// stdin/stdout to this transport. `on_exit` is called exactly once when
    /// the child process exits or its IO fails. Uses the crate's default
    /// channel capacity for both the inbound relay's ring and the outbound
    /// ring.
    pub async fn spawn<F>(
        command: &[String],
        reporter: TransportReporter,
        on_exit: F,
    ) -> io::Result<(Self, ChannelRelay<u8>)>
    where
        F: FnOnce(io::Error) + Send + 'static,
    {
        Self::spawn_with_capacity(command, reporter, on_exit, CHANNEL_CAPACITY).await
    }

    /// Same as [`spawn`](Self::spawn), with the inbound/outbound ring
    /// capacity parameterized. `pub(crate)` and used only by tests, to force
    /// a deterministic ring overflow (capacity 1) rather than relying on
    /// timing.
    pub(crate) async fn spawn_with_capacity<F>(
        command: &[String],
        reporter: TransportReporter,
        on_exit: F,
        capacity: usize,
    ) -> io::Result<(Self, ChannelRelay<u8>)>
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

        let (in_tx, in_rx) = bounded::<u8>(capacity);
        let relay = ChannelRelay::spawn(in_rx, capacity);

        let (outbound_producer, outbound_consumer) = RingBuffer::new(capacity);
        let outbound_notify = Arc::new(Notify::new());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let connected = Arc::new(AtomicBool::new(true));

        tokio::spawn(run_pipe_task(
            stdin,
            stdout,
            child,
            on_exit,
            in_tx,
            outbound_consumer,
            Arc::clone(&outbound_notify),
            shutdown_rx,
            Arc::clone(&connected),
            reporter.clone(),
        ));

        let transport = Self {
            outbound: outbound_producer,
            outbound_notify,
            shutdown_tx: Some(shutdown_tx),
            connected,
            reporter,
        };
        transport.reporter.report_connected(None);
        Ok((transport, relay))
    }
}

impl Transport for PipeTransport {
    /// Superseded by the [`ChannelRelay<u8>`](ChannelRelay) returned
    /// alongside this transport from `spawn`/`spawn_with_capacity` — inbound
    /// bytes flow through that relay now, not through this method. Retained
    /// only because `Transport::try_recv` isn't removed from the trait until
    /// every device migrates to draining a relay directly (transport relay
    /// redesign plan §10, checklist item 12). Until then, a device still
    /// calling this method on a `PipeTransport` will not receive child output.
    fn try_recv(&mut self) -> Option<u8> {
        None
    }

    /// Sends a byte to the child's stdin.
    ///
    /// Gates on the child still being alive: once it has exited, its stdin
    /// pipe has no reader left, so the write is skipped and counted as a
    /// drop via `TransportReporter` instead of attempting a doomed syscall.
    /// Never blocks and never errors: outbound ring overflow is also counted
    /// via `TransportReporter`, not surfaced here.
    fn send(&mut self, byte: u8) {
        if !self.connected.load(Ordering::Acquire) {
            self.reporter.note_outbound_drop();
            return;
        }
        match self.outbound.push(byte) {
            Ok(()) => self.outbound_notify.notify_one(),
            Err(PushError::Full(_)) => self.reporter.note_outbound_drop(),
        }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Tokio task: bridges child process stdin/stdout to the sync side.
///
/// Reads bytes from `stdout` and pushes them into `in_tx` (consumed by the
/// `ChannelRelay<u8>` returned from `spawn`); drains `outbound` (the
/// transport's `send()` target) and writes those bytes to `stdin`; reports
/// outbound drop counts on a 1-second interval. `connected` tracks the
/// child's alive/exited state for `is_connected()` and `send()`'s gate,
/// independent of the relay. On any exit — IO error, child process
/// termination, or shutdown signal — calls `on_exit` with a describing
/// error and reports the disconnect edge via `reporter` (§5.1); the matching
/// connect edge is reported once at `spawn`, since a spawned child is
/// considered connected from the start.
#[allow(clippy::too_many_arguments)]
async fn run_pipe_task<F>(
    mut stdin: tokio::process::ChildStdin,
    mut stdout: tokio::process::ChildStdout,
    mut child: tokio::process::Child,
    on_exit: F,
    in_tx: Sender<u8>,
    mut outbound: Consumer<u8>,
    outbound_notify: Arc<Notify>,
    mut shutdown_rx: oneshot::Receiver<()>,
    connected: Arc<AtomicBool>,
    reporter: TransportReporter,
) where
    F: FnOnce(io::Error) + Send + 'static,
{
    let mut report_interval = tokio::time::interval(std::time::Duration::from_secs(1));
    report_interval.tick().await; // first tick fires immediately; skip it

    let exit_error = loop {
        let mut buf = [0u8; 1];
        tokio::select! {
            _ = &mut shutdown_rx => {
                break io::Error::new(io::ErrorKind::Interrupted, "transport shut down");
            }

            result = stdout.read(&mut buf) => match result {
                Ok(1) => {
                    if in_tx.send(buf[0]).is_err() {
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

            _ = drain_outbound(&mut stdin, &mut outbound, &outbound_notify, &reporter) => {}

            _ = report_interval.tick() => {
                reporter.report_counts();
            }
        }
    };

    connected.store(false, Ordering::Release);
    reporter.report_disconnected(None, exit_error.to_string());
    drop(in_tx);
    on_exit(exit_error);
}

async fn drain_outbound(
    stdin: &mut tokio::process::ChildStdin,
    outbound: &mut Consumer<u8>,
    notify: &Notify,
    reporter: &TransportReporter,
) {
    notify.notified().await;
    while let Ok(byte) = outbound.pop() {
        if let Err(e) = stdin.write_all(&[byte]).await {
            reporter.report_error(TransportError::Io(e));
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::{DeviceEvent, DeviceId, device_event_channel};
    use std::sync::Mutex;

    /// Shuts `transport` down (stopping the background Tokio task) and drops
    /// it plus `relay`. `ChannelRelay::drop`'s internal stop signal means
    /// this is safe and prompt even here, inside an `async fn` test on a
    /// Tokio worker (see the shutdown contract in the module doc).
    fn close(mut transport: PipeTransport, relay: ChannelRelay<u8>) {
        transport.shutdown();
        drop((transport, relay));
    }

    #[tokio::test]
    async fn spawn_cat_and_echo_byte() {
        let received_exit = Arc::new(Mutex::new(None::<String>));
        let received_exit_clone = Arc::clone(&received_exit);

        let (mut transport, mut relay) = PipeTransport::spawn(
            &["cat".to_string()],
            TransportReporter::pending(None),
            move |e| *received_exit_clone.lock().unwrap() = Some(e.to_string()),
        ).await.unwrap();

        transport.send(0x42);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let mut got = Vec::new();
        relay.drain_into(|b| got.push(b));
        assert_eq!(got, vec![0x42]);

        close(transport, relay);
    }

    #[tokio::test]
    async fn exit_calls_on_exit_callback() {
        let received_exit = Arc::new(Mutex::new(false));
        let received_exit_clone = Arc::clone(&received_exit);

        // `true` exits immediately with status 0
        let (_transport, _relay) = PipeTransport::spawn(
            &["true".to_string()],
            TransportReporter::pending(None),
            move |_| *received_exit_clone.lock().unwrap() = true,
        ).await.unwrap();

        // Give the task time to detect child exit and call on_exit
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(*received_exit.lock().unwrap(), "on_exit should have been called");
    }

    #[tokio::test]
    async fn exit_marks_disconnected() {
        let (transport, relay) = PipeTransport::spawn(
            &["true".to_string()],
            TransportReporter::pending(None),
            |_| {},
        ).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!transport.is_connected());

        close(transport, relay);
    }

    #[tokio::test]
    async fn shutdown_marks_disconnected() {
        let (mut transport, relay) = PipeTransport::spawn(
            &["cat".to_string()],
            TransportReporter::pending(None),
            |_| {},
        ).await.unwrap();

        transport.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());

        close(transport, relay);
    }

    #[tokio::test]
    async fn send_after_exit_does_not_write_and_is_not_a_panic() {
        let (mut transport, relay) = PipeTransport::spawn(
            &["true".to_string()],
            TransportReporter::pending(None),
            |_| {},
        ).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!transport.is_connected());

        // Must not panic or block now that the child has exited.
        transport.send(0xFF);

        close(transport, relay);
    }

    #[tokio::test]
    async fn outbound_overflow_increments_drop_counter_and_is_reported() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::new(DeviceId(99), Some(sender));
        let (mut transport, relay) = PipeTransport::spawn_with_capacity(
            &["cat".to_string()],
            reporter.clone(),
            |_| {},
            1,
        ).await.unwrap();
        assert!(matches!(receiver.try_recv(), Ok(DeviceEvent::TransportConnected { .. })));

        // Capacity 1, two sends back-to-back with no `.await` in between —
        // the spawned Tokio task can't be scheduled to drain in between, so
        // the second send is guaranteed to see the ring still full.
        transport.send(0x01);
        transport.send(0x02);

        reporter.report_counts();

        match receiver.try_recv() {
            Ok(DeviceEvent::OutboundBytesDropped { device, count }) => {
                assert_eq!(device, DeviceId(99));
                assert!(count >= 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        close(transport, relay);
    }
}
