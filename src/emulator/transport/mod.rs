//! Transport abstraction and implementations for device IO.
//!
//! ## `ChannelRelay` shutdown contract
//!
//! `Drop for ChannelRelay` sends an internal stop signal, unparks the relay
//! thread (in case it's parked retrying a push into a full ring), and joins
//! it. For a `ChannelRelay::spawn`-constructed relay, this makes dropping
//! always prompt: the relay thread doesn't need its producer side to make
//! any progress (e.g. an owning Tokio task actually getting polled) in order
//! to exit, so it's safe to drop a `ChannelRelay` from anywhere — including
//! from within an async task on a Tokio worker, which would otherwise
//! deadlock that worker against the very task it needs to drive to
//! completion. An earlier version of this design had no independent stop
//! signal and instead required every corresponding `Sender<T>` to be dropped
//! first; that contract turned out to be an easy-to-violate footgun in
//! practice (see the transport relay redesign plan's implementation log)
//! and has been replaced by this one.
//!
//! `ChannelRelay::from_parts`-constructed relays don't yet have this
//! independent signal wired to their caller-owned thread (that thread's loop
//! is defined entirely by the caller, e.g. `InternalPipeTransport`, which
//! reads off a raw fd rather than a `crossbeam_channel`). Until a caller
//! wires up an equivalent stop condition of its own, dropping one of these
//! still depends on the caller's thread exiting on its own.
pub mod internal_pipe;
pub mod pipe;
pub mod tcp_socket;
pub mod unix_socket;
pub mod pty;

pub use self::internal_pipe::InternalPipeTransport;
pub use self::pipe::PipeTransport;
pub use self::pty::PtyTransport;
pub use self::tcp_socket::TcpSocketTransport;
pub use self::unix_socket::UnixSocketTransport;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use crate::emulator::{DeviceEvent, DeviceId, ErrorSender};
use crossbeam_channel::{Receiver, Select, Sender, TrySendError, bounded};
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, broadcast, oneshot, watch};

pub(crate) const CHANNEL_CAPACITY: usize = 256;

/// Capacity of the outbound fan-out broadcast channel used by multi-client
/// transports (TCP, Unix socket). Each connected client gets its own
/// receiver subscribed to this channel.
pub(crate) const BROADCAST_CAPACITY: usize = 256;

/// Error type for transport operations.
#[derive(Debug, Error)]
pub enum TransportError {
    /// An IO error occurred on the underlying channel.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// The remote end closed the connection.
    #[error("disconnected")]
    Disconnected,
    /// The send channel is full (non-blocking send failed).
    #[error("send buffer full")]
    Full,
}

/// Bundles the callbacks a transport needs at construction time: error
/// reporting, diagnostic drop counters, and connect/disconnect edge
/// reporting. Built once per transport and cloned into every
/// concurrent owner that needs to report — the CPU-thread-side `Transport`
/// (`send`'s outbound-drop counting), the outbound-pump task, and, for
/// multipoint transports, each per-client task — sharing one set of
/// counters and one `ErrorSender` via `Arc` so counts/errors converge
/// correctly regardless of which clone incremented or reported them.
#[derive(Clone)]
pub struct TransportReporter {
    /// Bound lazily for the [`TransportSlot`](crate::emulator::TransportSlot)
    /// injection path, where the transport must be constructed before its
    /// device (and `DeviceId`) exists. Every reporting method is a silent
    /// no-op until this is set.
    device_id: Arc<OnceLock<DeviceId>>,
    error_sender: Option<ErrorSender>,
    outbound_drops: Arc<AtomicU64>,
    inbound_drops: Arc<AtomicU64>,
}

impl TransportReporter {
    /// Constructs a reporter whose `DeviceId` isn't known yet — for the
    /// [`TransportSlot`](crate::emulator::TransportSlot) injection path,
    /// where the transport must be built before the device (and its
    /// `DeviceId`) exists. Every reporting call before `bind` is a silent
    /// no-op.
    ///
    /// `pub` (rather than `pub(crate)`) so the two call sites that build an
    /// `InternalPipeTransport` directly for that injection path —
    /// `src/bin/emulator/main.rs` and `debugger/src-tauri/src/lib.rs`, both
    /// outside this crate — can construct one too.
    pub fn pending(error_sender: Option<ErrorSender>) -> Self {
        Self {
            device_id: Arc::new(OnceLock::new()),
            error_sender,
            outbound_drops: Arc::new(AtomicU64::new(0)),
            inbound_drops: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Binds the `DeviceId` once the caller determines it. Every
    /// existing `Clone` of this reporter — including ones already handed to
    /// background tasks that started before the ID was known — observes the
    /// bound ID from that point on, since it lives behind the same `Arc` as
    /// the counters/`ErrorSender`. Exactly-once, enforced by the underlying
    /// `OnceLock`; later calls are silently ignored.
    pub(crate) fn bind(&self, device_id: DeviceId) {
        let _ = self.device_id.set(device_id);
    }

    /// Reports a hard transport error. Currently only ever called with
    /// `TransportError::Io`.
    pub fn report_error(&self, error: TransportError) {
        let Some(&device) = self.device_id.get() else { return };
        let Some(sender) = &self.error_sender else { return };
        let _ = sender.send(DeviceEvent::TransportError { device, error });
    }

    /// Increments the outbound drop counter. Every transport has one.
    pub fn note_outbound_drop(&self) {
        self.outbound_drops.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments the inbound ingress-drop counter. Multipoint only —
    /// P2P transports never call this.
    pub fn note_inbound_drop(&self) {
        self.inbound_drops.fetch_add(1, Ordering::Relaxed);
    }

    /// Swaps both counters to 0 and emits `DeviceEvent::OutboundBytesDropped`/
    /// `InboundEventsDropped` for any nonzero count. Called from the existing
    /// outbound-pump/ingress tokio tasks on a `tokio::time::interval`.
    pub fn report_counts(&self) {
        let Some(&device) = self.device_id.get() else { return };
        let Some(sender) = &self.error_sender else { return };

        let outbound = self.outbound_drops.swap(0, Ordering::Relaxed);
        if outbound > 0 {
            let _ = sender.send(DeviceEvent::OutboundBytesDropped { device, count: outbound });
        }

        let inbound = self.inbound_drops.swap(0, Ordering::Relaxed);
        if inbound > 0 {
            let _ = sender.send(DeviceEvent::InboundEventsDropped { device, count: inbound });
        }
    }

    /// Reports a connect edge. `peer` is `None` for point-to-point
    /// transports and `Some(name)` per-client for multipoint ones.
    pub fn report_connected(&self, peer: Option<String>) {
        let Some(&device) = self.device_id.get() else { return };
        let Some(sender) = &self.error_sender else { return };
        let _ = sender.send(DeviceEvent::TransportConnected { device, peer });
    }

    /// Reports a disconnect edge; see [`report_connected`](Self::report_connected)
    /// for `peer`.
    pub fn report_disconnected(&self, peer: Option<String>, reason: String) {
        let Some(&device) = self.device_id.get() else { return };
        let Some(sender) = &self.error_sender else { return };
        let _ = sender.send(DeviceEvent::TransportDisconnected { device, peer, reason });
    }
}

#[cfg(test)]
mod transport_reporter_tests {
    use super::*;
    use crate::emulator::device_event_channel;

    #[test]
    fn pending_reporter_is_no_op_until_bound() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));

        reporter.report_error(TransportError::Disconnected);
        reporter.report_connected(None);
        reporter.note_outbound_drop();
        reporter.report_counts();

        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn bind_enables_reporting_on_existing_clones() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        let clone = reporter.clone();

        clone.bind(DeviceId(7));
        reporter.report_connected(Some("peer".to_string()));

        match receiver.try_recv() {
            Ok(DeviceEvent::TransportConnected { device, peer }) => {
                assert_eq!(device, DeviceId(7));
                assert_eq!(peer, Some("peer".to_string()));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn report_error_sends_transport_error() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind(DeviceId(1));

        reporter.report_error(TransportError::Disconnected);

        match receiver.try_recv() {
            Ok(DeviceEvent::TransportError { device, error: TransportError::Disconnected }) => {
                assert_eq!(device, DeviceId(1));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn report_counts_emits_only_nonzero_counters_and_resets_them() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind(DeviceId(2));

        reporter.note_outbound_drop();
        reporter.note_outbound_drop();
        reporter.report_counts();

        match receiver.try_recv() {
            Ok(DeviceEvent::OutboundBytesDropped { device, count }) => {
                assert_eq!(device, DeviceId(2));
                assert_eq!(count, 2);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        // Inbound counter was 0 the whole time, so no InboundEventsDropped fires.
        assert!(receiver.try_recv().is_err());

        // Counters were reset by the swap above; nothing new to report.
        reporter.report_counts();
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn clones_share_counters_and_error_sender() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind(DeviceId(3));
        let clone = reporter.clone();

        clone.note_inbound_drop();
        reporter.report_counts();

        match receiver.try_recv() {
            Ok(DeviceEvent::InboundEventsDropped { device, count }) => {
                assert_eq!(device, DeviceId(3));
                assert_eq!(count, 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn no_error_sender_is_a_silent_no_op() {
        let reporter = TransportReporter::pending(None);
        reporter.bind(DeviceId(4));
        // Must not panic even with no sender to report through.
        reporter.report_error(TransportError::Disconnected);
        reporter.report_connected(None);
        reporter.report_disconnected(None, "gone".to_string());
        reporter.note_outbound_drop();
        reporter.report_counts();
    }
}

/// An event drained from a [`ChannelRelay<TransportEvent>`](ChannelRelay) for
/// transports that support multiple concurrent connections.
///
/// `Connected`/`Disconnected` bound the lifetime of a given `tag` explicitly,
/// so callers that demultiplex by tag (e.g. `ProtocolManager`) don't have to
/// infer connection/disconnection from data alone — which is unreliable once
/// the tag (a truncated, wrapping view of the connection counter) is reused
/// by an unrelated later connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportEvent {
    /// A new connection tagged `tag` has been established.
    Connected(u8),
    /// A byte tagged with the connection it arrived on.
    Data(u8, u8),
    /// The connection tagged `tag` has closed. No further `Data` events
    /// carrying this tag will be produced unless the tag is later reused
    /// by a new `Connected` event.
    Disconnected(u8),
}

/// Allocates small tags used to identify live connections, guaranteeing no
/// two *live* connections ever share a tag. Freed tags become available for
/// reuse by later connections — unlike deriving the tag from a monotonic
/// counter, this makes collisions impossible (as long as at most 256
/// connections are open at once), not just statistically unlikely.
pub(crate) struct TagAllocator {
    in_use: Mutex<[bool; 256]>,
}

impl TagAllocator {
    pub(crate) fn new() -> Self {
        Self { in_use: Mutex::new([false; 256]) }
    }

    /// Allocates and returns the lowest-numbered unused tag, or `None` if
    /// all 256 tags are currently in use (256 concurrent connections).
    pub(crate) fn allocate(&self) -> Option<u8> {
        let mut in_use = self.in_use.lock().unwrap();
        let tag = (0..=u8::MAX).find(|&t| !in_use[t as usize])?;
        in_use[tag as usize] = true;
        Some(tag)
    }

    /// Releases `tag` so a future connection can reuse it.
    pub(crate) fn release(&self, tag: u8) {
        self.in_use.lock().unwrap()[tag as usize] = false;
    }
}

/// A dedicated relay thread paired with the consumer side of an `rtrb` ring
/// buffer. The relay thread blocks on an inbound source and pushes each item
/// into the ring, parking on a full ring until [`ChannelRelay::drain_into`]
/// frees space and unparks it. Using a plain OS thread (rather than an async
/// task) for the parkable producer side avoids stalling a Tokio worker and
/// everything else scheduled on it — `thread::park`/`unpark` block only the
/// dedicated relay thread, never a Tokio executor thread. Shared by every
/// `Transport` implementation needing an inbound relay (`T` is `u8` for
/// point-to-point transports, `TransportEvent` for multipoint ones).
pub struct ChannelRelay<T> {
    /// Consumer side of the ring the relay thread pushes into.
    consumer: Consumer<T>,
    /// The relay thread's handle. Taken on drop so it can be unparked and
    /// joined exactly once.
    handle: Option<JoinHandle<()>>,
    /// Independent shutdown signal for `Drop`, so a [`spawn`](Self::spawn)ed
    /// relay thread can be told to stop without depending on its producer
    /// side making any progress — see the module documentation. `None` for
    /// [`from_parts`](Self::from_parts)-constructed relays, whose thread
    /// doesn't know about this signal (yet).
    stop: Option<Sender<()>>,
}

impl<T: Send + 'static> ChannelRelay<T> {
    /// Spawns a relay thread that waits on either `rx` or its own internal
    /// stop signal, pushing each item received from `rx` into a new ring of
    /// `capacity` via the crate-internal `push_and_park` retry helper.
    /// Exits when `rx` disconnects *or* when `Drop` signals it to stop — see the
    /// [module documentation](crate::emulator::transport) for why both
    /// exist.
    ///
    /// `pub` (rather than `pub(crate)`, as every in-crate `Transport`
    /// implementation's own use of this constructor would otherwise
    /// require) so that tests exercising a device end-to-end can hand-feed
    /// a relay from a plain `crossbeam_channel` they control, independent
    /// of any real transport's timing.
    pub fn spawn(rx: Receiver<T>, capacity: usize) -> Self {
        let (mut producer, consumer) = RingBuffer::new(capacity);
        let (stop_tx, stop_rx) = bounded::<()>(1);
        let handle = thread::spawn(move || {
            let mut sel = Select::new();
            let data_idx = sel.recv(&rx);
            let stop_idx = sel.recv(&stop_rx);
            loop {
                let oper = sel.select();
                match oper.index() {
                    i if i == data_idx => match oper.recv(&rx) {
                        Ok(item) => {
                            if !push_and_park(&mut producer, item, &stop_rx) {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                    i if i == stop_idx => {
                        let _ = oper.recv(&stop_rx);
                        break;
                    }
                    _ => unreachable!(),
                }
            }
        });
        Self { consumer, handle: Some(handle), stop: Some(stop_tx) }
    }

    /// Wraps an already-running relay thread's `Consumer<T>`/`JoinHandle`
    /// directly, without spawning anything or requiring a `Receiver<T>`. For
    /// callers like [`InternalPipeTransport`] that read from a raw
    /// source with no `crossbeam_channel` hop and drive their own
    /// [`push_and_park`] loop against their own `Producer<T>`.
    pub(crate) fn from_parts(consumer: Consumer<T>, handle: JoinHandle<()>) -> Self {
        Self { consumer, handle: Some(handle), stop: None }
    }

    /// Pops one item from the ring, if available.
    fn pop(&mut self) -> Option<T> {
        self.consumer.pop().ok()
    }

    /// Returns `true` if the ring currently has nothing to drain.
    ///
    /// Lets a caller skip work that's only needed when there's actually
    /// something to process — see `ProtocolManager::has_pending`'s doc
    /// comment for why this matters for `Via6522`/`Mc6840`.
    pub(crate) fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    /// Unparks the relay thread. Harmless if it isn't currently parked.
    fn unpark(&self) {
        if let Some(handle) = &self.handle {
            handle.thread().unpark();
        }
    }

    /// Drains all currently available items, then unparks the relay thread
    /// if anything was actually drained. Call once per `tick()`.
    ///
    /// The unpark is conditional on having popped at least one item: `park`/
    /// `unpark` share a single per-thread token regardless of *why* a thread
    /// parked, and this same relay thread also blocks in a
    /// `crossbeam_channel::Select` wait (for the next inbound item) whenever
    /// it isn't retrying a full-ring push in `push_and_park`. An
    /// unconditional `unpark()` here would spuriously kick the relay thread
    /// out of that otherwise-idle `Select` wait on every call — at the
    /// unthrottled-clock tick rate, that's a busy-loop with no actual data
    /// flowing. Only popping something means the ring may have just gained
    /// free space, which is the only condition `push_and_park` needs to be
    /// woken for.
    pub fn drain_into(&mut self, mut f: impl FnMut(T)) {
        let mut drained_any = false;
        while let Some(item) = self.pop() {
            drained_any = true;
            f(item);
        }
        if drained_any {
            self.unpark();
        }
    }
}

impl<T> Drop for ChannelRelay<T> {
    /// Signals stop (if this relay has that signal wired up, i.e. it was
    /// `spawn`ed rather than built via `from_parts`), unparks the relay
    /// thread (in case it's parked retrying a push into a full ring), and
    /// joins it. See the [module documentation](crate::emulator::transport)
    /// for why both are needed and what this means for `from_parts`.
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

/// The relay half of whichever `(Transport, Relay)` pair a [`TransportSpec`](crate::emulator::TransportSpec)
/// produces: `ChannelRelay<u8>` for point-to-point transports (Pty, Pipe,
/// InternalPipe) or `ChannelRelay<TransportEvent>` for multipoint ones (Tcp,
/// Unix socket). A device that only cares about the byte stream — not which
/// client tag it arrived on — can drain either kind uniformly via
/// [`drain_bytes_into`](Self::drain_bytes_into) without matching on the
/// variant itself.
pub enum TransportRelay {
    /// A point-to-point transport's relay.
    Byte(ChannelRelay<u8>),
    /// A multipoint transport's relay.
    Tagged(ChannelRelay<TransportEvent>),
}

impl TransportRelay {
    /// Drains all currently available bytes, regardless of relay kind. For
    /// [`Tagged`](Self::Tagged), only `TransportEvent::Data` events yield a
    /// byte; `Connected`/`Disconnected` events are discarded here, since a
    /// device that doesn't distinguish clients has no use for them (peer
    /// connect/disconnect edges are reported separately via
    /// [`TransportReporter::report_connected`]/
    /// [`TransportReporter::report_disconnected`]).
    pub fn drain_bytes_into(&mut self, mut f: impl FnMut(u8)) {
        match self {
            TransportRelay::Byte(relay) => relay.drain_into(f),
            TransportRelay::Tagged(relay) => relay.drain_into(|event| {
                if let TransportEvent::Data(_, byte) = event {
                    f(byte);
                }
            }),
        }
    }
}

/// Pushes `item` into `producer`, parking the current thread on
/// `PushError::Full` and retrying once unparked, until either the push
/// succeeds (`true`) or `stop` signals shutdown while parked, in which case
/// `item` is dropped and this returns `false`. Shared by
/// [`ChannelRelay::spawn`]'s thread body and `InternalPipeTransport`'s custom
/// relay thread, so the retry loop exists in exactly one place.
pub(crate) fn push_and_park<T>(producer: &mut Producer<T>, mut item: T, stop: &Receiver<()>) -> bool {
    loop {
        match producer.push(item) {
            Ok(()) => return true,
            Err(PushError::Full(returned)) => {
                item = returned;
                thread::park();
                if stop.try_recv().is_ok() {
                    return false;
                }
            }
        }
    }
}

pub trait Transport: Send {
    /// Never blocks and never returns an error. Outbound ring overflow is
    /// diagnostic-only (counted, not reported as an error); hard I/O errors
    /// are reported asynchronously via the `TransportReporter` supplied at
    /// construction, not through this call's return value.
    fn send(&mut self, byte: u8);

    fn is_connected(&self) -> bool;

    fn shutdown(&mut self);
}

// --- Shared machinery for multi-client, ring-based transports (TCP, Unix
// socket). Unified here once both transports were converted to the
// `ChannelRelay`/`TransportReporter` shapes — `UnixSocketTransport` briefly
// held a local duplicate of these while `TcpSocketTransport` still used an
// older, `ChannelBridge`-based implementation, before both were unified here.

/// Drains the outbound ring on notification, fanning bytes out to every
/// connected client, and reports outbound/inbound drop counts on a
/// 1-second interval.
pub(crate) async fn pump_outbound(
    mut outbound: Consumer<u8>,
    fanout_tx: broadcast::Sender<u8>,
    outbound_notify: Arc<Notify>,
    mut shutdown_rx: watch::Receiver<bool>,
    reporter: TransportReporter,
) {
    let mut report_interval = tokio::time::interval(Duration::from_secs(1));
    report_interval.tick().await; // first tick fires immediately; skip it

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => break,
            _ = outbound_notify.notified() => {
                while let Ok(byte) = outbound.pop() {
                    let _ = fanout_tx.send(byte);
                }
            }
            _ = report_interval.tick() => {
                reporter.report_counts();
            }
        }
    }
}

/// Per-connection context needed to run a client session: identity, the
/// channels bridging it to the sync side and to the other clients' fan-out,
/// the shutdown signal, the shared bookkeeping (`client_count`,
/// `tag_allocator`) it must update on exit, and the reporter for ingress
/// drop-counting.
pub(crate) struct ClientSession {
    pub(crate) conn_tag: u8,
    /// Human-readable client identity for `DeviceEvent::TransportConnected`/
    /// `TransportDisconnected` — `peer_addr()` for TCP, `peer_cred()`
    /// (falling back to `conn_tag`) for Unix sockets. Captured by the caller
    /// at accept time, before the stream is split.
    pub(crate) peer: String,
    pub(crate) in_tx: Sender<TransportEvent>,
    pub(crate) fanout_rx: broadcast::Receiver<u8>,
    pub(crate) shutdown_rx: watch::Receiver<bool>,
    pub(crate) client_count: Arc<AtomicUsize>,
    pub(crate) tag_allocator: Arc<TagAllocator>,
    pub(crate) reporter: TransportReporter,
}

/// Handles one connected client for the lifetime of its session: reads bytes
/// tagged with `session.conn_tag` into `session.in_tx` (via `try_send`,
/// counting a `Full` inbound ring as a drop via `TransportReporter` rather
/// than blocking), and writes bytes fanned out via `session.fanout_rx` to
/// the client. Generic over any split-able async stream, so it's shared
/// between `TcpSocketTransport` and `UnixSocketTransport`.
pub(crate) async fn run_client_task<R, W>(mut reader: R, mut writer: W,
    session: ClientSession)
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let ClientSession {
        conn_tag,
        peer,
        in_tx,
        mut fanout_rx,
        mut shutdown_rx,
        client_count,
        tag_allocator,
        reporter,
    } = session;

    reporter.report_connected(Some(peer.clone()));

    if in_tx.try_send(TransportEvent::Connected(conn_tag)).is_err() {
        reporter.report_disconnected(Some(peer), "inbound channel unavailable".to_string());
        client_count.fetch_sub(1, Ordering::Release);
        tag_allocator.release(conn_tag);
        return;
    }

    let mut buf = [0u8; 1];
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                // Skip the terminal Disconnected send on whole-bus shutdown:
                // ProtocolManager::poll_transport only reacts to Disconnected
                // by releasing a slot, and neither slots nor the tag
                // allocator outlive the bus, so a skipped event here has no
                // observable effect. DeviceEvent::TransportDisconnected is a
                // separate, UI-facing signal and still fires below.
                reporter.report_disconnected(Some(peer), "shutdown".to_string());
                drop(in_tx);
                client_count.fetch_sub(1, Ordering::Release);
                tag_allocator.release(conn_tag);
                return;
            }

            result = reader.read(&mut buf) => {
                match result {
                    Ok(1) => {
                        match in_tx.try_send(TransportEvent::Data(conn_tag, buf[0])) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => reporter.note_inbound_drop(),
                            Err(TrySendError::Disconnected(_)) => break,
                        }
                    }
                    _ => break,
                }
            }

            byte = fanout_rx.recv() => {
                match byte {
                    Ok(byte) => {
                        if writer.write_all(&[byte]).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    let _ = in_tx.try_send(TransportEvent::Disconnected(conn_tag));
    reporter.report_disconnected(Some(peer), "connection closed".to_string());
    client_count.fetch_sub(1, Ordering::Release);
    tag_allocator.release(conn_tag);
}

/// Abstracts a multipoint listener's `accept()` over `TcpListener`/
/// `UnixListener` so [`run_listener_task`] can own their otherwise-identical
/// accept loops in one place (transport relay redesign plan, PR #227 review).
/// `PeerInfo` carries whatever raw, listener-specific data is needed to name
/// the client (a `SocketAddr` for TCP, a `UCred` for Unix), captured at
/// accept time — before the stream is split into halves for
/// [`run_client_task`] — and turned into the final peer string by
/// [`format_peer`](Self::format_peer) once the connection's `conn_tag` is
/// known, since Unix's fallback identifier needs it.
pub(crate) trait ClientListener {
    type Reader: AsyncRead + Unpin + Send + 'static;
    type Writer: AsyncWrite + Unpin + Send + 'static;
    type PeerInfo;

    /// Explicit `+ Send` on the returned future (rather than plain `async
    /// fn`) is required so `tokio::spawn(run_listener_task(...))` type-checks
    /// from inside the generic [`ListenerCore::spawn`], not just from a
    /// concrete, non-generic call site.
    fn accept(&self) -> impl std::future::Future<Output = std::io::Result<(Self::Reader, Self::Writer, Self::PeerInfo)>> + Send;

    fn format_peer(info: Self::PeerInfo, conn_tag: u8) -> String;
}

/// Tokio task: owns a multipoint listener's accept loop, spawning a
/// [`run_client_task`] per connection, and the outbound [`pump_outbound`]
/// task that fans sent bytes out to every connected client. Shared by
/// `TcpSocketTransport` and `UnixSocketTransport` via [`ClientListener`] —
/// their accept loops were otherwise byte-for-byte identical except for the
/// listener/stream types and how a client's peer identity is captured.
pub(crate) async fn run_listener_task<L: ClientListener>(
    listener: L,
    in_tx: Sender<TransportEvent>,
    outbound: Consumer<u8>,
    outbound_notify: Arc<Notify>,
    mut shutdown_rx: watch::Receiver<bool>,
    client_count: Arc<AtomicUsize>,
    reporter: TransportReporter,
) {
    let (fanout_tx, _) = broadcast::channel::<u8>(BROADCAST_CAPACITY);
    tokio::spawn(pump_outbound(outbound, fanout_tx.clone(), outbound_notify, shutdown_rx.clone(), reporter.clone()));

    let tag_allocator = Arc::new(TagAllocator::new());

    loop {
        let (reader, writer, peer_info) = tokio::select! {
            _ = shutdown_rx.changed() => break,
            result = listener.accept() => match result {
                Ok(parts) => parts,
                Err(_) => continue,
            },
        };

        let conn_tag = match tag_allocator.allocate() {
            Some(tag) => tag,
            None => continue,
        };
        let peer = L::format_peer(peer_info, conn_tag);

        client_count.fetch_add(1, Ordering::Release);

        tokio::spawn(run_client_task(
            reader,
            writer,
            ClientSession {
                conn_tag,
                peer,
                in_tx: in_tx.clone(),
                fanout_rx: fanout_tx.subscribe(),
                shutdown_rx: shutdown_rx.clone(),
                client_count: Arc::clone(&client_count),
                tag_allocator: Arc::clone(&tag_allocator),
                reporter: reporter.clone(),
            },
        ));
    }
}

/// Sends `true` on `shutdown_tx` once `shutdown_rx` fires, translating a
/// transport's one-shot `shutdown()` signal into the `watch` channel that
/// [`run_listener_task`] and [`pump_outbound`] select on. A `watch` channel
/// (rather than the oneshot directly) is needed because both tasks, plus
/// every per-client [`run_client_task`], must observe the same shutdown
/// signal.
async fn propagate_shutdown(shutdown_rx: oneshot::Receiver<()>, shutdown_tx: watch::Sender<bool>) {
    let _ = shutdown_rx.await;
    let _ = shutdown_tx.send(true);
}

/// Owns the state and background Tokio tasks shared by every multipoint,
/// ring-based listener transport (`TcpSocketTransport`, `UnixSocketTransport`)
/// — construction (relay, outbound ring, shutdown plumbing, `run_listener_task`),
/// and the `send`/`is_connected`/`shutdown` logic that used to be duplicated
/// verbatim across both `Transport` impls, before being extracted here.
/// Each transport wraps this plus whatever
/// listener-specific identity it needs to expose (a `PathBuf` for Unix, a
/// `SocketAddr` for TCP).
pub(crate) struct ListenerCore {
    /// Outbound ring producer; see the module doc for the outbound path.
    outbound: Producer<u8>,
    outbound_notify: Arc<Notify>,
    /// One-shot signal sent to the Tokio task to request shutdown.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Number of currently connected clients. `send()` is a no-op while this
    /// is zero; `is_connected()` reports whether it's nonzero.
    client_count: Arc<AtomicUsize>,
    /// Clone of the reporter supplied to `spawn`, for `send()`'s own
    /// `note_outbound_drop` call.
    reporter: TransportReporter,
}

impl ListenerCore {
    /// Wires up the inbound relay, outbound ring, and shutdown plumbing for
    /// `listener`, and spawns the [`run_listener_task`] Tokio task that
    /// drives its accept loop. Returns the resulting core plus the inbound
    /// relay the caller hands back alongside its transport.
    pub(crate) fn spawn<L: ClientListener + Send + 'static>(
        listener: L,
        reporter: TransportReporter,
        capacity: usize,
    ) -> (Self, ChannelRelay<TransportEvent>) {
        let (in_tx, in_rx) = bounded::<TransportEvent>(capacity);
        let relay = ChannelRelay::spawn(in_rx, capacity);

        let (outbound_producer, outbound_consumer) = RingBuffer::new(capacity);
        let outbound_notify = Arc::new(Notify::new());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let client_count = Arc::new(AtomicUsize::new(0));

        let (shutdown_watch_tx, shutdown_watch_rx) = watch::channel(false);
        tokio::spawn(propagate_shutdown(shutdown_rx, shutdown_watch_tx));

        tokio::spawn(run_listener_task(
            listener,
            in_tx,
            outbound_consumer,
            Arc::clone(&outbound_notify),
            shutdown_watch_rx,
            Arc::clone(&client_count),
            reporter.clone(),
        ));

        let core = Self {
            outbound: outbound_producer,
            outbound_notify,
            shutdown_tx: Some(shutdown_tx),
            client_count,
            reporter,
        };
        (core, relay)
    }

    /// Pushes a byte into the outbound ring for fan-out to every connected
    /// client. A no-op while no client is connected — matching the previous
    /// early-out behavior — and this idle-no-client case is deliberately
    /// *not* counted as a drop (unlike a genuine ring overflow while clients
    /// are connected), since it's ordinary steady state for an unwatched
    /// multipoint device, not a diagnostic signal. Never blocks and never
    /// errors: ring overflow while connected is counted via
    /// `TransportReporter`, not surfaced here.
    pub(crate) fn send(&mut self, byte: u8) {
        if self.client_count.load(Ordering::Acquire) == 0 {
            return;
        }
        match self.outbound.push(byte) {
            Ok(()) => self.outbound_notify.notify_one(),
            Err(PushError::Full(_)) => self.reporter.note_outbound_drop(),
        }
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.client_count.load(Ordering::Acquire) > 0
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod channel_relay_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn round_trip_preserves_order() {
        let (tx, rx) = bounded::<u8>(16);
        let mut relay = ChannelRelay::spawn(rx, 16);

        for i in 0..10u8 {
            tx.send(i).unwrap();
        }
        std::thread::sleep(Duration::from_millis(50));

        let mut got = Vec::new();
        relay.drain_into(|item| got.push(item));
        assert_eq!(got, (0..10u8).collect::<Vec<_>>());
    }

    #[test]
    fn backpressure_parks_relay_thread_until_drained() {
        let (tx, rx) = bounded::<u8>(16);
        let mut relay = ChannelRelay::spawn(rx, 2);

        // Fill the ring (capacity 2) and give the relay thread time to push both.
        tx.send(0).unwrap();
        tx.send(1).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        // The relay thread should now be parked trying to push a 3rd item —
        // sending it must not panic or lose the item.
        tx.send(2).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let mut got = Vec::new();
        relay.drain_into(|item| got.push(item));
        assert_eq!(got, vec![0, 1]);

        // drain_into's unpark should let the relay thread push the 3rd item now.
        std::thread::sleep(Duration::from_millis(50));
        let mut got = Vec::new();
        relay.drain_into(|item| got.push(item));
        assert_eq!(got, vec![2]);
    }

    #[test]
    fn drop_returns_promptly_after_senders_close() {
        let (tx, rx) = bounded::<u8>(4);
        let relay = ChannelRelay::spawn(rx, 4);
        drop(tx);

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(relay);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("ChannelRelay::drop hung after Sender was dropped");
    }

    #[test]
    fn drop_returns_promptly_even_with_sender_still_alive() {
        let (tx, rx) = bounded::<u8>(4);
        let relay = ChannelRelay::spawn(rx, 4);
        // Deliberately keep `tx` alive past the relay's drop — this is
        // exactly the scenario that used to require callers to carefully
        // order drops (or, worse, deadlock a Tokio worker joining a thread
        // that could only exit once an async task got scheduled to drop its
        // sender). The internal stop signal means the relay thread no
        // longer needs the sender to close at all.
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(relay);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("ChannelRelay::drop hung with a live Sender");

        drop(tx);
    }
}
