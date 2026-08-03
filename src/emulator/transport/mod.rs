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
use tokio::sync::{Notify, broadcast, watch};

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

pub(crate) fn no_op_reporter() -> Box<impl Fn(TransportError) + Send> {
    Box::new(|_error| {})
}

pub(crate) fn reporter(sender: ErrorSender, id: DeviceId) -> Box<impl Fn(TransportError) + Send> {
    Box::new(move |error| {
        let _ = sender.send(DeviceEvent::TransportError { device: id, error });
    })
}

/// Bundles the callbacks a transport needs at construction time: error
/// reporting, diagnostic drop counters (§1.5), and connect/disconnect edge
/// reporting (§5.1). Built once per transport and cloned into every
/// concurrent owner that needs to report — the CPU-thread-side `Transport`
/// (`send`'s outbound-drop counting), the outbound-pump task, and, for
/// multipoint transports, each per-client task — sharing one set of
/// counters and one `ErrorSender` via `Arc` so counts/errors converge
/// correctly regardless of which clone incremented or reported them.
#[derive(Clone)]
pub struct TransportReporter {
    /// Bound lazily for the `TransportSlot` injection path (§2.2), where the
    /// transport must be constructed before its device (and `DeviceId`)
    /// exists. Every reporting method is a silent no-op until this is set.
    device_id: Arc<OnceLock<DeviceId>>,
    error_sender: Option<ErrorSender>,
    outbound_drops: Arc<AtomicU64>,
    inbound_drops: Arc<AtomicU64>,
}

impl TransportReporter {
    /// Constructs a reporter already bound to `device_id`.
    pub(crate) fn new(device_id: DeviceId, error_sender: Option<ErrorSender>) -> Self {
        let reporter = Self::pending(error_sender);
        reporter.bind(device_id);
        reporter
    }

    /// Constructs a reporter whose `DeviceId` isn't known yet (§2.2) — for
    /// the `TransportSlot` injection path (§9.4), where the transport must
    /// be built before the device (and its `DeviceId`) exists. Every
    /// reporting call before `bind` is a silent no-op.
    pub(crate) fn pending(error_sender: Option<ErrorSender>) -> Self {
        Self {
            device_id: Arc::new(OnceLock::new()),
            error_sender,
            outbound_drops: Arc::new(AtomicU64::new(0)),
            inbound_drops: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Binds the `DeviceId` once the caller determines it (§2.2). Every
    /// existing `Clone` of this reporter — including ones already handed to
    /// background tasks that started before the ID was known — observes the
    /// bound ID from that point on, since it lives behind the same `Arc` as
    /// the counters/`ErrorSender`. Exactly-once, enforced by the underlying
    /// `OnceLock`; later calls are silently ignored.
    pub(crate) fn bind(&self, device_id: DeviceId) {
        let _ = self.device_id.set(device_id);
    }

    /// Reports a hard transport error. Currently only ever called with
    /// `TransportError::Io` (§1.8).
    pub fn report_error(&self, error: TransportError) {
        let Some(&device) = self.device_id.get() else { return };
        let Some(sender) = &self.error_sender else { return };
        let _ = sender.send(DeviceEvent::TransportError { device, error });
    }

    /// Increments the outbound drop counter (§1.5). Every transport has one.
    pub fn note_outbound_drop(&self) {
        self.outbound_drops.fetch_add(1, Ordering::Relaxed);
    }

    /// Increments the inbound ingress-drop counter (§1.5). Multipoint only —
    /// P2P transports never call this.
    pub fn note_inbound_drop(&self) {
        self.inbound_drops.fetch_add(1, Ordering::Relaxed);
    }

    /// Swaps both counters to 0 and emits `DeviceEvent::OutboundBytesDropped`/
    /// `InboundEventsDropped` for any nonzero count. Called from the existing
    /// outbound-pump/ingress tokio tasks on a `tokio::time::interval` (§1.5).
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

    /// Reports a connect edge (§5.1). `peer` is `None` for point-to-point
    /// transports and `Some(name)` per-client for multipoint ones.
    pub fn report_connected(&self, peer: Option<String>) {
        let Some(&device) = self.device_id.get() else { return };
        let Some(sender) = &self.error_sender else { return };
        let _ = sender.send(DeviceEvent::TransportConnected { device, peer });
    }

    /// Reports a disconnect edge (§5.1); see [`report_connected`](Self::report_connected)
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
        let reporter = TransportReporter::new(DeviceId(1), Some(sender));

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
        let reporter = TransportReporter::new(DeviceId(2), Some(sender));

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
        let reporter = TransportReporter::new(DeviceId(3), Some(sender));
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
        let reporter = TransportReporter::new(DeviceId(4), None);
        // Must not panic even with no sender to report through.
        reporter.report_error(TransportError::Disconnected);
        reporter.report_connected(None);
        reporter.report_disconnected(None, "gone".to_string());
        reporter.note_outbound_drop();
        reporter.report_counts();
    }
}

/// An event yielded by [`Transport::try_recv_tagged`] for transports that
/// support multiple concurrent connections.
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
/// everything else scheduled on it; see `doc/transport-relay-redesign-plan.md`
/// §1.2 for the full rationale. Shared by every `Transport` implementation
/// needing an inbound relay (`T` is `u8` for point-to-point transports,
/// `TransportEvent` for multipoint ones).
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
    /// callers like [`InternalPipeTransport`] (§4.4) that read from a raw
    /// source with no `crossbeam_channel` hop and drive their own
    /// [`push_and_park`] loop against their own `Producer<T>`.
    pub(crate) fn from_parts(consumer: Consumer<T>, handle: JoinHandle<()>) -> Self {
        Self { consumer, handle: Some(handle), stop: None }
    }

    /// Pops one item from the ring, if available.
    fn pop(&mut self) -> Option<T> {
        self.consumer.pop().ok()
    }

    /// Unparks the relay thread. Harmless if it isn't currently parked.
    fn unpark(&self) {
        if let Some(handle) = &self.handle {
            handle.thread().unpark();
        }
    }

    /// Drains all currently available items, then unparks the relay thread.
    /// Call once per `tick()`.
    pub fn drain_into(&mut self, mut f: impl FnMut(T)) {
        while let Some(item) = self.pop() {
            f(item);
        }
        self.unpark();
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
    /// `TransportReporter`, §5.1).
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
/// relay thread (§4.4), so the retry loop exists in exactly one place.
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
    fn try_recv(&mut self) -> Option<u8>;

    /// Never blocks and never returns an error. Outbound ring overflow is
    /// diagnostic-only (§1.5, counted not reported); hard I/O errors are
    /// reported asynchronously via the `TransportReporter` supplied at
    /// construction (§1.8), not through this call's return value.
    fn send(&mut self, byte: u8);

    fn is_connected(&self) -> bool;

    /// Returns the next event from the channel.
    /// For a transport type that accepts multiple client connections, the sequence of events for
    /// any given client tag starts with a [`Connected`](TransportEvent::Connected) event,
    /// followed by zero or more [`Data`](TransportEvent::Data) events, followed by a
    /// [`Disconnected`](TransportEvent::Disconnected) event.
    ///
    fn try_recv_tagged(&mut self) -> Option<TransportEvent> {
        self.try_recv().map(|b| TransportEvent::Data(0, b))
    }

    fn shutdown(&mut self);
}

// --- Shared machinery for multi-client, ring-based transports (TCP, Unix
// socket). Unified here once both transports were converted to the
// `ChannelRelay`/`TransportReporter` shapes (transport relay redesign plan
// §4.2) — `UnixSocketTransport` (item 4) briefly held a local duplicate of
// these while `TcpSocketTransport` (item 5) still used the older
// `ChannelBridge`-based versions; see the plan's implementation log for why.

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
        in_tx,
        mut fanout_rx,
        mut shutdown_rx,
        client_count,
        tag_allocator,
        reporter,
    } = session;

    if in_tx.try_send(TransportEvent::Connected(conn_tag)).is_err() {
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
                // observable effect.
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
    client_count.fetch_sub(1, Ordering::Release);
    tag_allocator.release(conn_tag);
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
