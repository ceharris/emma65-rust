/// Transport abstraction for device IO, plus concrete implementations.
pub mod pipe;
pub mod tcp;
pub mod unix_socket;
pub mod pty;

pub use self::pipe::PipeTransport;
pub use self::tcp::TcpTransport;
pub use self::unix_socket::UnixSocketTransport;
pub use self::pty::PtyTransport;

use thiserror::Error;

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
    /// Initiates a graceful shutdown of the transport.
    fn shutdown(&mut self);
}
