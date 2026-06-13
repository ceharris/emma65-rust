use std::net::SocketAddr;

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

use super::{Transport, TransportError};

const CHANNEL_CAPACITY: usize = 256;

/// Transport over a TCP connection.
///
/// A Tokio task owns the `TcpStream`; the sync side communicates via bounded
/// `crossbeam` channels so the CPU thread never blocks on async IO.
pub struct TcpTransport {
    rx: Receiver<u8>,
    tx: Sender<u8>,
    /// Signals the Tokio task to shut down.
    shutdown_tx: Option<oneshot::Sender<()>>,
    connected: bool,
}

impl TcpTransport {
    /// Connects to `addr` on the current Tokio runtime and returns a `TcpTransport`.
    pub async fn connect(addr: SocketAddr) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self::from_stream(stream))
    }

    /// Wraps an already-connected `TcpStream`.
    pub fn from_stream(stream: TcpStream) -> Self {
        let (in_tx, in_rx) = bounded::<u8>(CHANNEL_CAPACITY);
        let (out_tx, out_rx) = bounded::<u8>(CHANNEL_CAPACITY);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        tokio::spawn(run_tcp_task(stream, in_tx, out_rx, shutdown_rx));

        Self {
            rx: in_rx,
            tx: out_tx,
            shutdown_tx: Some(shutdown_tx),
            connected: true,
        }
    }
}

impl Transport for TcpTransport {
    fn try_recv(&mut self) -> Option<u8> {
        match self.rx.try_recv() {
            Ok(b) => Some(b),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.connected = false;
                None
            }
        }
    }

    fn send(&mut self, byte: u8) -> Result<(), TransportError> {
        if !self.connected {
            return Err(TransportError::Disconnected);
        }
        match self.tx.try_send(byte) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Full(_)) => Err(TransportError::Full),
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                self.connected = false;
                Err(TransportError::Disconnected)
            }
        }
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.connected = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn connect_send_recv() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            TcpTransport::from_stream(stream)
        });

        let mut client = TcpTransport::connect(addr).await.unwrap();
        let mut server = server_task.await.unwrap();

        client.send(0xAB).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(server.try_recv(), Some(0xAB));
    }

    #[tokio::test]
    async fn disconnect_detection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            TcpTransport::from_stream(stream)
        });

        let client = TcpTransport::connect(addr).await.unwrap();
        let mut server = server_task.await.unwrap();

        drop(client);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        // Server should observe None from a disconnected channel.
        assert_eq!(server.try_recv(), None);
    }
}

/// Tokio task: bridges the async `TcpStream` to sync crossbeam channels.
async fn run_tcp_task(
    stream: TcpStream,
    in_tx: Sender<u8>,
    out_rx: Receiver<u8>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = [0u8; 1];

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,

            result = reader.read(&mut buf) => {
                match result {
                    Ok(1) => {
                        if in_tx.send(buf[0]).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }

            _ = drain_outbound(&mut writer, &out_rx) => {}
        }
    }
}

/// Flushes all pending outbound bytes from `out_rx` into `writer`.
async fn drain_outbound(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    out_rx: &Receiver<u8>,
) {
    while let Ok(byte) = out_rx.try_recv() {
        if writer.write_all(&[byte]).await.is_err() {
            return;
        }
    }
    // Yield so tokio::select! can poll the other branches.
    tokio::task::yield_now().await;
}
