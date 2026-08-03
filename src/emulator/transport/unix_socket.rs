//! Transport that listens for incoming Unix domain socket connections.
//!
//! A Tokio task owns the `UnixListener` and accepts connections in a loop,
//! spawning a per-client task for each one so multiple clients can be
//! connected concurrently — the "multipoint" transport shape (transport
//! relay redesign plan §4.2), contrasted with point-to-point transports like
//! [`PtyTransport`](super::PtyTransport) (§4.1). Inbound bytes from every
//! client are tagged with their originating connection and relayed through a
//! single [`ChannelRelay<TransportEvent>`](ChannelRelay), the same mechanism
//! point-to-point transports use for `ChannelRelay<u8>`. Outbound bytes are
//! pushed into an `rtrb::Producer<u8>` by `send()` (never blocking; overflow
//! is counted via [`TransportReporter`], not surfaced as an error) and
//! fanned out to every connected client via a `broadcast::channel`.
//!
//! `pump_outbound`, `run_client_task`, and `ClientSession` here are
//! deliberately *not* the shared versions of the same names in
//! `super` — those are still used as-is by [`TcpSocketTransport`]
//! (super::TcpSocketTransport), not yet converted to this redesign
//! (checklist item 5). Duplicating them locally keeps this rewrite isolated
//! and compiling on its own; look for an opportunity to reunify once both
//! transports are converted.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crossbeam_channel::{Sender, TrySendError, bounded};
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{Notify, broadcast, oneshot, watch};

use super::{BROADCAST_CAPACITY, CHANNEL_CAPACITY, ChannelRelay, TagAllocator, Transport, TransportEvent, TransportReporter};

/// Transport that listens for incoming Unix-domain socket connections.
pub struct UnixSocketTransport {
    /// Outbound ring producer; see the module doc for the outbound path.
    outbound: Producer<u8>,
    outbound_notify: Arc<Notify>,
    /// One-shot signal sent to the Tokio task to request shutdown.
    shutdown_tx: Option<oneshot::Sender<()>>,
    /// Number of currently connected clients. `send()` is a no-op while this
    /// is zero; `is_connected()` reports whether it's nonzero.
    client_count: Arc<AtomicUsize>,
    /// Filesystem path of the listening socket.
    path: PathBuf,
    /// Clone of the reporter supplied to `listen`, for `send()`'s own
    /// `note_outbound_drop` call.
    reporter: TransportReporter,
}

impl UnixSocketTransport {
    /// Binds a Unix-domain socket at `path` and starts listening for
    /// connections, using the crate's default channel/ring capacity for
    /// both the inbound relay and the outbound ring.
    pub async fn listen(path: impl Into<PathBuf>, reporter: TransportReporter) -> std::io::Result<(Self, ChannelRelay<TransportEvent>)> {
        Self::listen_with_capacity(path, reporter, CHANNEL_CAPACITY).await
    }

    /// Same as [`listen`](Self::listen), with the inbound/outbound ring
    /// capacity parameterized. `pub(crate)` and used only by the drop-count
    /// tests below, to force deterministic ring/channel overflow (capacity
    /// 1) rather than relying on timing.
    pub(crate) async fn listen_with_capacity(
        path: impl Into<PathBuf>,
        reporter: TransportReporter,
        capacity: usize,
    ) -> std::io::Result<(Self, ChannelRelay<TransportEvent>)> {
        let path = path.into();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;

        let (in_tx, in_rx) = bounded::<TransportEvent>(capacity);
        let relay = ChannelRelay::spawn(in_rx, capacity);

        let (outbound_producer, outbound_consumer) = RingBuffer::new(capacity);
        let outbound_notify = Arc::new(Notify::new());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let client_count = Arc::new(AtomicUsize::new(0));

        let (shutdown_watch_tx, shutdown_watch_rx) = watch::channel(false);
        tokio::spawn(propagate_shutdown(shutdown_rx, shutdown_watch_tx));

        tokio::spawn(run_unix_task(
            listener,
            in_tx,
            outbound_consumer,
            Arc::clone(&outbound_notify),
            shutdown_watch_rx,
            Arc::clone(&client_count),
            reporter.clone(),
        ));

        let transport = Self {
            outbound: outbound_producer,
            outbound_notify,
            shutdown_tx: Some(shutdown_tx),
            client_count,
            path,
            reporter,
        };
        Ok((transport, relay))
    }

    /// Returns the filesystem path of the listening socket.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Transport for UnixSocketTransport {
    /// Superseded by the [`ChannelRelay<TransportEvent>`](ChannelRelay)
    /// returned alongside this transport from `listen`/`listen_with_capacity`
    /// — inbound events flow through that relay now, not through this
    /// method. Retained only because `Transport::try_recv` isn't removed
    /// from the trait until every device migrates to draining a relay
    /// directly (transport relay redesign plan §10, checklist item 12).
    /// Until then, a device still calling this method on a
    /// `UnixSocketTransport` will not receive inbound data.
    fn try_recv(&mut self) -> Option<u8> {
        None
    }

    /// Pushes a byte into the outbound ring for fan-out to every connected
    /// client. A no-op while no client is connected — matching the previous
    /// early-out behavior — and this idle-no-client case is deliberately
    /// *not* counted as a drop (unlike a genuine ring overflow while clients
    /// are connected), since it's ordinary steady state for an unwatched
    /// multipoint device, not a diagnostic signal. Never blocks and never
    /// errors: ring overflow while connected is counted via
    /// `TransportReporter`, not surfaced here.
    fn send(&mut self, byte: u8) {
        if self.client_count.load(Ordering::Acquire) == 0 {
            return;
        }
        match self.outbound.push(byte) {
            Ok(()) => self.outbound_notify.notify_one(),
            Err(PushError::Full(_)) => self.reporter.note_outbound_drop(),
        }
    }

    fn is_connected(&self) -> bool {
        self.client_count.load(Ordering::Acquire) > 0
    }

    fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

async fn propagate_shutdown(shutdown_rx: oneshot::Receiver<()>, shutdown_tx: watch::Sender<bool>) {
    let _ = shutdown_rx.await;
    let _ = shutdown_tx.send(true);
}

/// Tokio task: owns the `UnixListener`'s accept loop, spawning a
/// [`run_client_task`] per connection, and the outbound
/// [`pump_outbound`] task that fans sent bytes out to every connected
/// client.
async fn run_unix_task(
    listener: UnixListener,
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
        let stream = tokio::select! {
            _ = shutdown_rx.changed() => break,
            result = listener.accept() => match result {
                Ok((stream, _)) => stream,
                Err(_) => continue,
            },
        };

        let conn_tag = match tag_allocator.allocate() {
            Some(tag) => tag,
            None => continue,
        };

        client_count.fetch_add(1, Ordering::Release);

        let (reader, writer) = stream.into_split();
        tokio::spawn(run_client_task(
            reader,
            writer,
            ClientSession {
                conn_tag,
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

/// Drains the outbound ring on notification, fanning bytes out to every
/// connected client, and reports outbound/inbound drop counts on a
/// 1-second interval.
async fn pump_outbound(
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
struct ClientSession {
    conn_tag: u8,
    in_tx: Sender<TransportEvent>,
    fanout_rx: broadcast::Receiver<u8>,
    shutdown_rx: watch::Receiver<bool>,
    client_count: Arc<AtomicUsize>,
    tag_allocator: Arc<TagAllocator>,
    reporter: TransportReporter,
}

/// Handles one connected client for the lifetime of its session: reads bytes
/// tagged with `session.conn_tag` into `session.in_tx` (via `try_send`,
/// counting a `Full` inbound ring as a drop rather than blocking), and
/// writes bytes fanned out via `session.fanout_rx` to the client.
async fn run_client_task<R, W>(mut reader: R, mut writer: W, session: ClientSession)
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
                // observable effect (transport relay redesign plan §4.2).
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
mod tests {
    use super::*;
    use crate::emulator::{DeviceEvent, DeviceId, device_event_channel};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    fn tmp_socket_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/emma65_test_{}.sock", name))
    }

    /// Shuts `transport` down (stopping the background Tokio tasks, not just
    /// the relay) and drops it plus `relay`. Must be called before a test
    /// ends if any client connection is still expected to be alive —
    /// dropping `relay` first would stop its relay thread and disconnect
    /// `in_tx`, causing client tasks to exit early.
    fn close(mut transport: UnixSocketTransport, relay: ChannelRelay<TransportEvent>) {
        transport.shutdown();
        drop((transport, relay));
    }

    fn only_data(events: Vec<TransportEvent>) -> Vec<(u8, u8)> {
        events.into_iter().filter_map(|e| match e { TransportEvent::Data(tag, byte) => Some((tag, byte)), _ => None }).collect()
    }

    async fn make_transport(name: &str) -> (UnixSocketTransport, ChannelRelay<TransportEvent>) {
        let path = tmp_socket_path(name);
        UnixSocketTransport::listen(path, TransportReporter::pending(None)).await.unwrap()
    }

    #[tokio::test]
    async fn listen_accept_send_recv() {
        let (mut transport, mut relay) = make_transport("unix_listen_send_recv").await;
        let path = transport.path().to_path_buf();

        let mut client = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        client.write_all(&[0xAB]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut got = Vec::new();
        relay.drain_into(|event| got.push(event));
        assert_eq!(only_data(got), vec![(0, 0xAB)]);

        transport.send(0xCD);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut buf = [0u8; 1];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf[0], 0xCD);

        close(transport, relay);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn reconnection() {
        let (transport, mut relay) = make_transport("unix_reconnection").await;
        let path = transport.path().to_path_buf();

        let mut c1 = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        c1.write_all(&[0x01]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut got = Vec::new();
        relay.drain_into(|event| got.push(event));
        assert_eq!(only_data(got).into_iter().map(|(_, b)| b).collect::<Vec<_>>(), vec![0x01]);
        drop(c1);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let mut c2 = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        c2.write_all(&[0x02]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut got2 = Vec::new();
        relay.drain_into(|event| got2.push(event));
        assert_eq!(only_data(got2).into_iter().map(|(_, b)| b).collect::<Vec<_>>(), vec![0x02]);

        close(transport, relay);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn send_while_no_client() {
        let (mut transport, relay) = make_transport("unix_no_client").await;
        let path = transport.path().to_path_buf();

        assert!(!transport.is_connected());
        transport.send(0xFF);

        close(transport, relay);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn send_while_no_client_does_not_count_as_a_drop() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::new(DeviceId(101), Some(sender));
        let (mut transport, relay) = UnixSocketTransport::listen(tmp_socket_path("unix_no_client_no_drop"), reporter.clone()).await.unwrap();
        let path = transport.path().to_path_buf();

        assert!(!transport.is_connected());
        for _ in 0..10 {
            transport.send(0xFF);
        }

        reporter.report_counts();
        assert!(receiver.try_recv().is_err(), "sending with no client connected must not count as an outbound drop");

        close(transport, relay);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn is_connected_reflects_client_state() {
        let (transport, relay) = make_transport("unix_is_connected").await;
        let path = transport.path().to_path_buf();

        assert!(!transport.is_connected());

        let client = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(transport.is_connected());

        drop(client);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());

        close(transport, relay);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn shutdown() {
        let (mut transport, relay) = make_transport("unix_shutdown").await;
        let path = transport.path().to_path_buf();

        let _client = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        transport.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());

        close(transport, relay);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn concurrent_clients_are_tagged_and_counted() {
        let (mut transport, mut relay) = make_transport("unix_concurrent").await;
        let path = transport.path().to_path_buf();

        let mut c1 = UnixStream::connect(&path).await.unwrap();
        let mut c2 = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(transport.is_connected());

        c1.write_all(&[0x11]).await.unwrap();
        c2.write_all(&[0x22]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let mut events = Vec::new();
        relay.drain_into(|event| events.push(event));

        let connected_tags: Vec<u8> = events.iter()
            .filter_map(|e| match e { TransportEvent::Connected(tag) => Some(*tag), _ => None })
            .collect();
        let data = only_data(events.clone());

        assert_eq!(connected_tags.len(), 2, "expected a Connected event for each client");
        // Different clients must be tagged with different connection IDs.
        assert_ne!(connected_tags[0], connected_tags[1]);

        assert_eq!(data.len(), 2);
        assert_ne!(data[0].0, data[1].0);

        // Fan-out: a single send() reaches both clients.
        transport.send(0xEE);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut b1 = [0u8; 1];
        let mut b2 = [0u8; 1];
        c1.read_exact(&mut b1).await.unwrap();
        c2.read_exact(&mut b2).await.unwrap();
        assert_eq!(b1[0], 0xEE);
        assert_eq!(b2[0], 0xEE);

        drop(c1);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // c2 still connected, so is_connected() should remain true.
        assert!(transport.is_connected());

        // The dropped client's Disconnected event should now be available.
        let mut saw_disconnect = false;
        let mut more_events = Vec::new();
        relay.drain_into(|event| more_events.push(event));
        for event in more_events {
            if let TransportEvent::Disconnected(tag) = event {
                assert!(connected_tags.contains(&tag));
                saw_disconnect = true;
            }
        }
        assert!(saw_disconnect, "expected a Disconnected event after dropping c1");

        drop(c2);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());

        close(transport, relay);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn outbound_overflow_increments_drop_counter_and_is_reported() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::new(DeviceId(99), Some(sender));
        let (mut transport, relay) = UnixSocketTransport::listen_with_capacity(tmp_socket_path("unix_outbound_overflow"), reporter.clone(), 1).await.unwrap();
        let path = transport.path().to_path_buf();

        let _client = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(transport.is_connected());

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
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn inbound_overflow_increments_drop_counter_and_is_reported() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::new(DeviceId(100), Some(sender));
        let (transport, relay) = UnixSocketTransport::listen_with_capacity(tmp_socket_path("unix_inbound_overflow"), reporter.clone(), 1).await.unwrap();
        let path = transport.path().to_path_buf();

        let mut client = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Never drain the relay: with channel/ring capacity 1, a burst large
        // enough is guaranteed to overflow before it's exhausted.
        let burst = [0xAAu8; 64];
        client.write_all(&burst).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        reporter.report_counts();

        match receiver.try_recv() {
            Ok(DeviceEvent::InboundEventsDropped { device, count }) => {
                assert_eq!(device, DeviceId(100));
                assert!(count >= 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        close(transport, relay);
        let _ = std::fs::remove_file(&path);
    }
}
