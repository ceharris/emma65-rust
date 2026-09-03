//! Transport that connects a device to the stdin/stdout of a child process.
//!
//! Spawns a command and bridges the device's byte stream to the child's stdin
//! (device → child) and the child's stdout (child → device), following the
//! same relay shape as [`PtyTransport`](super::PtyTransport): a Tokio task
//! reads `stdout` and pushes bytes into a
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
//!
//! [`Transport::send_bytes`] is overridden here to push a whole buffer into
//! the outbound ring atomically (via `rtrb`'s `push_entire_slice`): either
//! every byte fits or none do, so a caller relying on fixed-size framing
//! (e.g. a bulk per-frame payload) never sees a partial write that would
//! desync it. `drain_outbound` reads the ring in the same bulk fashion (via
//! `read_chunk`), so a drain writes to `stdin` in one or two `write_all`
//! calls (the ring can wrap) instead of one call per byte.

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
    /// capacity parameterized. `pub(crate)`: used by tests to force a
    /// deterministic ring overflow (capacity 1) rather than relying on
    /// timing, and by
    /// [`TransportSpec::to_transport_with_reporter_and_capacity`](crate::emulator::TransportSpec::to_transport_with_reporter_and_capacity)
    /// so a device module that knows its own bulk payload size (e.g. a
    /// per-vsync frame) can size the ring to fit it exactly.
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

    /// Pushes `bytes` into the outbound ring as a single atomic chunk: if
    /// the whole buffer doesn't fit, none of it is written and the entire
    /// buffer is counted as dropped — never a partial push. See the module
    /// documentation for why partial writes aren't acceptable here.
    fn send_bytes(&mut self, bytes: &[u8]) -> bool {
        if !self.connected.load(Ordering::Acquire) {
            self.reporter.note_outbound_drop_n(bytes.len() as u64);
            return false;
        }
        if bytes.is_empty() {
            return true;
        }
        match self.outbound.push_entire_slice(bytes) {
            Ok(()) => {
                self.outbound_notify.notify_one();
                true
            }
            Err(_) => {
                self.reporter.note_outbound_drop_n(bytes.len() as u64);
                false
            }
        }
    }

    /// `Producer::slots()` never overestimates free space (it's refreshed from the
    /// consumer's atomic position on every call), and this transport has exactly one
    /// producer -- this `send_bytes`/`send` caller -- so free space can only grow
    /// between this check and a subsequent `send_bytes` call, never shrink. A caller
    /// that checks here first is therefore guaranteed the follow-up `send_bytes` call
    /// will succeed, without ever having to attempt (and have reported as dropped) a
    /// send it already knows won't fit.
    fn has_outbound_capacity(&self, len: usize) -> bool {
        self.connected.load(Ordering::Acquire) && self.outbound.slots() >= len
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
/// error and reports the disconnect edge via `reporter`; the matching
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

/// Drains everything currently available in `outbound` and writes it to
/// `stdin` in one or two `write_all` calls (the ring can wrap, so a single
/// drain may span two contiguous slices) rather than one call per byte.
/// Only the portion actually written successfully is committed back to the
/// ring as consumed, so a `write_all` failure partway through leaves the
/// unwritten remainder in place for the next drain to retry — matching the
/// old per-byte loop's behavior on error.
async fn drain_outbound(
    stdin: &mut tokio::process::ChildStdin,
    outbound: &mut Consumer<u8>,
    notify: &Notify,
    reporter: &TransportReporter,
) {
    notify.notified().await;
    let available = outbound.slots();
    if available == 0 {
        return;
    }
    let chunk = match outbound.read_chunk(available) {
        Ok(chunk) => chunk,
        Err(_) => return,
    };

    let (first, second) = chunk.as_slices();
    let first_len = first.len();

    if let Err(e) = stdin.write_all(first).await {
        reporter.report_error(TransportError::Io(e));
        chunk.commit(0);
        return;
    }

    if !second.is_empty()
        && let Err(e) = stdin.write_all(second).await
    {
        reporter.report_error(TransportError::Io(e));
        chunk.commit(first_len);
        return;
    }

    chunk.commit_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::{DeviceEvent, device_event_channel};
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
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind("test-device-99");
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
                assert_eq!(device, "test-device-99");
                assert!(count >= 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        close(transport, relay);
    }

    #[tokio::test]
    async fn send_bytes_round_trips_a_bulk_buffer() {
        let (mut transport, mut relay) = PipeTransport::spawn(
            &["cat".to_string()],
            TransportReporter::pending(None),
            |_| {},
        ).await.unwrap();

        let payload: Vec<u8> = (0..200u16).map(|n| (n % 256) as u8).collect();
        assert!(transport.send_bytes(&payload));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let mut got = Vec::new();
        relay.drain_into(|b| got.push(b));
        assert_eq!(got, payload);

        close(transport, relay);
    }

    #[tokio::test]
    async fn send_bytes_empty_buffer_is_a_no_op() {
        let (mut transport, relay) = PipeTransport::spawn(
            &["cat".to_string()],
            TransportReporter::pending(None),
            |_| {},
        ).await.unwrap();

        assert!(transport.send_bytes(&[]));

        close(transport, relay);
    }

    #[tokio::test]
    async fn send_bytes_drops_entire_buffer_on_overflow_not_partially() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind("test-device-100");
        let (mut transport, mut relay) = PipeTransport::spawn_with_capacity(
            &["cat".to_string()],
            reporter.clone(),
            |_| {},
            4,
        ).await.unwrap();
        assert!(matches!(receiver.try_recv(), Ok(DeviceEvent::TransportConnected { .. })));

        // Ring capacity is 4; a 5-byte buffer can never fit, so the whole
        // buffer must be dropped — not the 4 bytes that would fit.
        let oversized = [1u8, 2, 3, 4, 5];
        assert!(!transport.send_bytes(&oversized));

        reporter.report_counts();
        match receiver.try_recv() {
            Ok(DeviceEvent::OutboundBytesDropped { device, count }) => {
                assert_eq!(device, "test-device-100");
                assert_eq!(count, oversized.len() as u64);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // Nothing from the dropped buffer should have reached the child.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut got = Vec::new();
        relay.drain_into(|b| got.push(b));
        assert!(got.is_empty(), "expected no bytes to arrive, got {got:?}");

        close(transport, relay);
    }

    #[tokio::test]
    async fn has_outbound_capacity_reflects_ring_free_space_without_reporting_a_drop() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind("test-device-101");
        let (transport, relay) = PipeTransport::spawn_with_capacity(
            &["cat".to_string()],
            reporter.clone(),
            |_| {},
            4,
        ).await.unwrap();
        assert!(matches!(receiver.try_recv(), Ok(DeviceEvent::TransportConnected { .. })));

        assert!(transport.has_outbound_capacity(4), "a buffer that exactly fits the ring must report capacity");
        assert!(!transport.has_outbound_capacity(5), "a buffer larger than the ring can never fit");

        // Checking capacity must never itself count as (or report) a drop, unlike an actual
        // failed `send_bytes` call (issue #587) -- there is nothing here for `report_counts` to
        // find.
        reporter.report_counts();
        assert!(receiver.try_recv().is_err());

        close(transport, relay);
    }

    #[tokio::test]
    async fn has_outbound_capacity_is_false_once_disconnected() {
        let (mut transport, relay) = PipeTransport::spawn(
            &["cat".to_string()],
            TransportReporter::pending(None),
            |_| {},
        ).await.unwrap();

        transport.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert!(
            !transport.has_outbound_capacity(1),
            "a disconnected transport can never accept a send, regardless of ring free space"
        );

        close(transport, relay);
    }
}
