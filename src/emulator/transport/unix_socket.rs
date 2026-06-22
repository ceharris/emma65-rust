use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::{Receiver, Sender};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::oneshot;

use super::{ChannelBridge, Transport, TransportError};

/// Transport that listens for incoming Unix domain socket connections.
///
/// A Tokio task owns the `UnixListener`; it accepts one client at a time, exchanges
/// bytes via bounded `crossbeam` channels, and loops back to waiting when the client
/// disconnects. The CPU thread never blocks on async IO.
pub struct UnixSocketTransport {
    bridge: ChannelBridge,
    /// Reflects whether a client is currently connected; shared with the Tokio task.
    client_connected: Arc<AtomicBool>,
    path: PathBuf,
}

impl UnixSocketTransport {
    /// Binds a Unix domain socket listener at `path` and begins accepting connections.
    ///
    /// Any stale socket file at `path` is removed before binding.
    pub async fn listen(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let (bridge, in_tx, out_rx, shutdown_rx) = ChannelBridge::new();
        let client_connected = Arc::new(AtomicBool::new(false));
        tokio::spawn(run_unix_task(
            listener,
            in_tx,
            out_rx,
            shutdown_rx,
            Arc::clone(&client_connected),
        ));
        Ok(Self { bridge, client_connected, path })
    }

    /// Returns the path this transport is listening on.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Transport for UnixSocketTransport {
    fn try_recv(&mut self) -> Option<u8> {
        self.bridge.try_recv()
    }

    /// Sends a byte. Returns `Ok(())` silently if no client is connected.
    fn send(&mut self, byte: u8) -> Result<(), TransportError> {
        if !self.client_connected.load(Ordering::Acquire) {
            return Ok(());
        }
        self.bridge.send(byte)
    }

    fn is_connected(&self) -> bool {
        self.client_connected.load(Ordering::Acquire)
    }

    fn shutdown(&mut self) {
        self.bridge.shutdown();
    }
}

/// Tokio task: listens for Unix socket clients and bridges each connection to sync channels.
async fn run_unix_task(
    listener: UnixListener,
    in_tx: Sender<u8>,
    out_rx: Receiver<u8>,
    mut shutdown_rx: oneshot::Receiver<()>,
    client_connected: Arc<AtomicBool>,
) {
    'outer: loop {
        // Phase 1: waiting for a client
        let stream = tokio::select! {
            _ = &mut shutdown_rx => break,
            result = listener.accept() => match result {
                Ok((stream, _)) => stream,
                Err(_) => break,
            },
        };

        // Drain any stale outbound bytes before the new client sees them.
        while out_rx.try_recv().is_ok() {}
        client_connected.store(true, Ordering::Release);

        // Phase 2: connected I/O
        let (mut reader, mut writer) = stream.into_split();
        let mut buf = [0u8; 1];
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break 'outer,

                result = reader.read(&mut buf) => {
                    match result {
                        Ok(1) => {
                            if in_tx.send(buf[0]).is_err() {
                                break 'outer;
                            }
                        }
                        _ => break, // EOF or error → back to waiting phase
                    }
                }

                _ = drain_outbound(&mut writer, &out_rx) => {}
            }
        }

        client_connected.store(false, Ordering::Release);
    }
}

/// Flushes all pending outbound bytes from `out_rx` into `writer`.
async fn drain_outbound(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    out_rx: &Receiver<u8>,
) {
    while let Ok(byte) = out_rx.try_recv() {
        if writer.write_all(&[byte]).await.is_err() {
            return;
        }
    }
    tokio::task::yield_now().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    fn tmp_socket_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/emma65_test_{}.sock", name))
    }

    async fn make_transport(name: &str) -> UnixSocketTransport {
        let path = tmp_socket_path(name);
        UnixSocketTransport::listen(&path).await.unwrap()
    }

    #[tokio::test]
    async fn listen_accept_send_recv() {
        let mut transport = make_transport("unix_listen_send_recv").await;
        let path = transport.path().to_path_buf();

        let mut client = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // client → transport
        client.write_all(&[0xAB]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv(), Some(0xAB));

        // transport → client
        transport.send(0xCD).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut buf = [0u8; 1];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf[0], 0xCD);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn reconnection() {
        let mut transport = make_transport("unix_reconnection").await;
        let path = transport.path().to_path_buf();

        // First client session
        let mut c1 = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        c1.write_all(&[0x01]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv(), Some(0x01));
        drop(c1);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Second client session
        let mut c2 = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        c2.write_all(&[0x02]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv(), Some(0x02));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn send_while_no_client() {
        let mut transport = make_transport("unix_no_client").await;
        let path = transport.path().to_path_buf();

        // No client connected; send should succeed silently
        assert_eq!(transport.is_connected(), false);
        assert!(transport.send(0xFF).is_ok());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn is_connected_reflects_client_state() {
        let transport = make_transport("unix_is_connected").await;
        let path = transport.path().to_path_buf();

        assert!(!transport.is_connected());

        let client = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(transport.is_connected());

        drop(client);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn shutdown() {
        let mut transport = make_transport("unix_shutdown").await;
        let path = transport.path().to_path_buf();

        let _client = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        transport.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());

        let _ = std::fs::remove_file(&path);
    }
}
