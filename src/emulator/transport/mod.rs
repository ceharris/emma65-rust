//! Transport abstraction and implementations for device IO.
pub mod pipe;
pub mod tcp_socket;
pub mod unix_socket;
pub mod pty;

pub use self::pipe::PipeTransport;
pub use self::pty::PtyTransport;
pub use self::tcp_socket::TcpTransport;
pub use self::unix_socket::UnixSocketTransport;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, oneshot, watch};

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
use std::sync::Mutex;

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

/// Shared sync-side state for channel-based transports.
///
/// Each of these transports spawns a Tokio task that owns the async IO handle(s)
/// and communicates with the CPU-thread sync side via a pair of bounded `crossbeam`
/// channels. `ChannelBridge` encapsulates those channels and the shutdown signal,
/// providing the common `Transport` method implementations.
///
/// `R` is the inbound item type: plain `u8` for single-connection transports
/// (pipe, PTY), or `(u64, u8)` — a connection ID paired with a byte — for
/// multi-client transports (TCP, Unix socket) so inbound bursts from
/// different clients can be demultiplexed downstream.
pub(crate) struct ChannelBridge<R = u8> {
    pub(crate) rx: Receiver<R>,
    pub(crate) tx: Sender<u8>,
    /// One-shot signal sent to the Tokio task to request shutdown.
    pub(crate) shutdown_tx: Option<oneshot::Sender<()>>,
}

impl<R: Send + 'static> ChannelBridge<R> {
    /// Creates a new bridge and returns `(bridge, task_rx, task_tx, task_shutdown_rx)`.
    ///
    /// The caller spawns a Tokio task that reads from `task_rx` (outbound bytes from
    /// the sync side) and writes to `task_tx` (inbound items for the sync side), and
    /// exits when `task_shutdown_rx` fires.
    pub(crate) fn new() -> (Self, Sender<R>, Receiver<u8>, oneshot::Receiver<()>) {
        let (in_tx, in_rx) = bounded::<R>(CHANNEL_CAPACITY);
        let (out_tx, out_rx) = bounded::<u8>(CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let bridge = Self {
            rx: in_rx,
            tx: out_tx,
            shutdown_tx: Some(shutdown_tx),
        };
        (bridge, in_tx, out_rx, shutdown_rx)
    }

    pub(crate) fn try_recv(&mut self) -> Option<R> {
        match self.rx.try_recv() {
            Ok(v) => Some(v),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    pub(crate) fn send(&mut self, byte: u8) -> Result<(), TransportError> {
        match self.tx.try_send(byte) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Full(_)) => Err(TransportError::Full),
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => Err(TransportError::Disconnected),
        }
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

// ... TransportError, ChannelBridge<R> unchanged from before ...

pub trait Transport: Send {
    fn try_recv(&mut self) -> Option<u8>;

    fn send(&mut self, byte: u8) -> Result<(), TransportError>;

    fn is_connected(&self) -> bool;

    /// Returns the next event from the channel.
    /// For a transport type that accepts multiple client connections, the sequence of events for
    /// any given client tag starts with a [`Connected`](TransportEvent::Connected) event,
    /// followed by zero or more [`Data`](TransportEvent::Data) events, followed by a
    /// [`Disconnected`](TransportEvent::Disconnected) event. Because the space of client tags
    /// is small, in the unlikely event of a very long-running client connection in conjunction
    /// with a large sequence of short-lived client connections, the client tag may wrap. When
    /// this happens the existing cl
    ///
    /// came from, or `None` if no byte is available.
    ///
    /// This tag is a truncated, wrapping view of [`connection_id`](Transport::connection_id)
    /// (kept to a single byte to minimize per-byte channel overhead), intended only to
    /// distinguish between the handful of clients that might be concurrently connected
    /// at once — not as a durable session identifier. Transports with at most one
    /// logical connection (pipe, PTY) use the default implementation, which tags
    /// every byte `0`.
    fn try_recv_tagged(&mut self) -> Option<TransportEvent> {
        self.try_recv().map(|b| TransportEvent::Data(0, b))
    }

    fn shutdown(&mut self);
}

// --- Shared machinery for multi-client, channel-based transports (TCP, Unix socket) ---

pub(crate) async fn pump_outbound(
    out_rx: Receiver<u8>,
    fanout_tx: broadcast::Sender<u8>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => break,
            _ = async {
                while let Ok(byte) = out_rx.try_recv() {
                    let _ = fanout_tx.send(byte);
                }
                tokio::task::yield_now().await;
            } => {}
        }
    }
}

/// Per-connection context needed to run a client session: identity, the
/// channels bridging it to the sync side and to the other clients' fan-out,
/// the shutdown signal, and the shared bookkeeping (`client_count`,
/// `tag_allocator`) it must update on exit.
pub(crate) struct ClientSession {
    pub(crate) conn_tag: u8,
    pub(crate) in_tx: Sender<TransportEvent>,
    pub(crate) fanout_rx: broadcast::Receiver<u8>,
    pub(crate) shutdown_rx: watch::Receiver<bool>,
    pub(crate) client_count: Arc<AtomicUsize>,
    pub(crate) tag_allocator: Arc<TagAllocator>,
}

/// Handles one connected client for the lifetime of its session: reads bytes
/// tagged with `session.conn_tag` into `session.in_tx`, and writes bytes
/// fanned out via `session.fanout_rx` to the client. Generic over any
/// split-able async stream, so it's shared between `TcpTransport` and
/// `UnixSocketTransport`.
pub(crate) async fn run_client_task<R, W>(
    mut reader: R,
    mut writer: W,
    session: ClientSession,
) where
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
    } = session;

    if in_tx.send(TransportEvent::Connected(conn_tag)).is_err() {
        client_count.fetch_sub(1, Ordering::Release);
        tag_allocator.release(conn_tag);
        return;
    }

    let mut buf = [0u8; 1];
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => break,

            result = reader.read(&mut buf) => {
                match result {
                    Ok(1) => {
                        if in_tx.send(TransportEvent::Data(conn_tag, buf[0])).is_err() {
                            break;
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

    let _ = in_tx.send(TransportEvent::Disconnected(conn_tag));
    client_count.fetch_sub(1, Ordering::Release);
    tag_allocator.release(conn_tag);
}
