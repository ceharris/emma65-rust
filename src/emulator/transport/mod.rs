//! Transport abstraction and implementations for device IO.
pub mod pipe;
pub mod tcp;
pub mod unix_socket;
pub mod pty;

pub use self::pipe::PipeTransport;
pub use self::tcp::TcpTransport;
pub use self::unix_socket::UnixSocketTransport;
pub use self::pty::PtyTransport;

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use thiserror::Error;
use tokio::sync::oneshot;

pub(crate) const CHANNEL_CAPACITY: usize = 256;

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

/// Shared sync-side state for channel-based transports (TCP, Unix socket, PTY).
///
/// Each of these transports spawns a Tokio task that owns the async IO handle and
/// communicates with the CPU-thread sync side via a pair of bounded `crossbeam`
/// channels. `ChannelBridge` encapsulates those channels and the shutdown signal,
/// providing the common `Transport` method implementations.
pub(crate) struct ChannelBridge {
    pub(crate) rx: Receiver<u8>,
    pub(crate) tx: Sender<u8>,
    /// One-shot signal sent to the Tokio task to request shutdown.
    pub(crate) shutdown_tx: Option<oneshot::Sender<()>>,
}

impl ChannelBridge {
    /// Creates a new bridge and returns `(bridge, task_rx, task_tx, task_shutdown_rx)`.
    ///
    /// The caller spawns a Tokio task that reads from `task_rx` (outbound bytes from
    /// the sync side) and writes to `task_tx` (inbound bytes for the sync side), and
    /// exits when `task_shutdown_rx` fires.
    pub(crate) fn new() -> (Self, Sender<u8>, Receiver<u8>, oneshot::Receiver<()>) {
        let (in_tx, in_rx) = bounded::<u8>(CHANNEL_CAPACITY);
        let (out_tx, out_rx) = bounded::<u8>(CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let bridge = Self {
            rx: in_rx,
            tx: out_tx,
            shutdown_tx: Some(shutdown_tx),
        };
        (bridge, in_tx, out_rx, shutdown_rx)
    }

    pub(crate) fn try_recv(&mut self) -> Option<u8> {
        match self.rx.try_recv() {
            Ok(b) => Some(b),
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

/// Byte-stream transport used by IO devices.
///
/// Implementations must be `Send` so they can be moved between threads during setup.
pub trait Transport: Send {
    /// Returns the next received byte, or `None` if no byte is available.
    fn try_recv(&mut self) -> Option<u8>;
    /// Sends a byte. Returns an error if the transport is full or disconnected.
    fn send(&mut self, byte: u8) -> Result<(), TransportError>;
    /// Returns `true` if the transport is currently connected.
    fn is_connected(&self) -> bool;
    /// Returns a monotonically increasing ID that increments each time a new client connects.
    ///
    /// Callers that need to detect reconnection can compare this value between polls;
    /// a change means a new session has begun and any per-connection state should be reset.
    /// Transports that do not support reconnection (e.g. [`PipeTransport`]) always return 0.
    fn connection_id(&self) -> u64;
    /// Initiates a graceful shutdown of the transport.
    fn shutdown(&mut self);
}
