//! Transport that listens for incoming Unix domain socket connections.
//!
//! A Tokio task owns the `UnixListener` and accepts connections in a loop,
//! spawning a per-client task for each one so multiple clients can be
//! connected concurrently. Outbound bytes are fanned out to every connected
//! client; inbound bytes are tagged with their originating connection ID so
//! they can be demultiplexed downstream.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, oneshot, watch};

use super::{pump_outbound, run_client_task, ChannelBridge, ClientSession, TagAllocator, Transport, TransportError, TransportEvent, BROADCAST_CAPACITY};

/// Transport that listens for incoming TCP connections.
///
/// Supports multiple concurrently connected clients. All clients receive the
/// same outbound byte stream (fan-out); inbound bytes are tagged with the ID
/// of the connection they came from (see [`Transport::try_recv_tagged`]).
pub struct UnixSocketTransport {
    bridge: ChannelBridge<TransportEvent>,
    client_count: Arc<AtomicUsize>,
    // Monotonic, non-wrapping; source of truth for `connection_id()`.
    // The per-byte wire tag is a truncated (`as u8`) view of this value.
    connection_counter: Arc<AtomicU64>,
    path: PathBuf,
}

impl UnixSocketTransport {
    pub async fn listen(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let (bridge, in_tx, out_rx, shutdown_rx) = ChannelBridge::<TransportEvent>::new();
        let client_count = Arc::new(AtomicUsize::new(0));
        let connection_counter = Arc::new(AtomicU64::new(0));

        let (shutdown_watch_tx, shutdown_watch_rx) = watch::channel(false);
        tokio::spawn(propagate_shutdown(shutdown_rx, shutdown_watch_tx));

        tokio::spawn(run_unix_task(
            listener,
            in_tx,
            out_rx,
            shutdown_watch_rx,
            Arc::clone(&client_count),
            Arc::clone(&connection_counter),
        ));
        Ok(Self { bridge, client_count, connection_counter, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Transport for UnixSocketTransport {
    fn try_recv(&mut self) -> Option<u8> {
        loop {
            match self.bridge.try_recv()? {
                TransportEvent::Data(_, byte) => return Some(byte),
                TransportEvent::Connected(_) | TransportEvent::Disconnected(_) => continue,
            }
        }
    }

    fn send(&mut self, byte: u8) -> Result<(), TransportError> {
        if self.client_count.load(Ordering::Acquire) == 0 {
            return Ok(());
        }
        self.bridge.send(byte)
    }

    fn is_connected(&self) -> bool {
        self.client_count.load(Ordering::Acquire) > 0
    }

    fn connection_id(&self) -> u64 {
        self.connection_counter.load(Ordering::Acquire)
    }

    fn try_recv_tagged(&mut self) -> Option<TransportEvent> {
        self.bridge.try_recv()
    }

    fn shutdown(&mut self) {
        self.bridge.shutdown();
    }
}

async fn propagate_shutdown(shutdown_rx: oneshot::Receiver<()>, shutdown_tx: watch::Sender<bool>) {
    let _ = shutdown_rx.await;
    let _ = shutdown_tx.send(true);
}

async fn run_unix_task(
    listener: UnixListener,
    in_tx: Sender<TransportEvent>,
    out_rx: Receiver<u8>,
    mut shutdown_rx: watch::Receiver<bool>,
    client_count: Arc<AtomicUsize>,
    connection_counter: Arc<AtomicU64>,
) {
    let (fanout_tx, _) = broadcast::channel::<u8>(BROADCAST_CAPACITY);
    tokio::spawn(pump_outbound(out_rx, fanout_tx.clone(), shutdown_rx.clone()));

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

        connection_counter.fetch_add(1, Ordering::Release);
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
            },
        ));
    }
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

        client.write_all(&[0xAB]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv(), Some(0xAB));

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

        let mut c1 = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        c1.write_all(&[0x01]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv(), Some(0x01));
        drop(c1);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

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

        assert!(!transport.is_connected());
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

    #[tokio::test]
    async fn concurrent_clients_are_tagged_and_counted() {
        let mut transport = make_transport("unix_concurrent").await;
        let path = transport.path().to_path_buf();

        let mut c1 = UnixStream::connect(&path).await.unwrap();
        let mut c2 = UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(transport.is_connected());

        c1.write_all(&[0x11]).await.unwrap();
        c2.write_all(&[0x22]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let mut events = Vec::new();
        while let Some(event) = transport.try_recv_tagged() {
            events.push(event);
        }

        let connected_tags: Vec<u8> = events.iter()
            .filter_map(|e| match e { TransportEvent::Connected(tag) => Some(*tag), _ => None })
            .collect();
        let data: Vec<(u8, u8)> = events.iter()
            .filter_map(|e| match e { TransportEvent::Data(tag, byte) => Some((*tag, *byte)), _ => None })
            .collect();

        assert_eq!(connected_tags.len(), 2, "expected a Connected event for each client");
        // Different clients must be tagged with different connection IDs.
        assert_ne!(connected_tags[0], connected_tags[1]);

        assert_eq!(data.len(), 2);
        assert_ne!(data[0].0, data[1].0);
        assert!(data.contains(&(connected_tags[0], if connected_tags[0] == data[0].0 { data[0].1 } else { data[1].1 })));

        // Fan-out: a single send() reaches both clients.
        transport.send(0xEE).unwrap();
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
        while let Some(event) = transport.try_recv_tagged() {
            if let TransportEvent::Disconnected(tag) = event {
                assert!(connected_tags.contains(&tag));
                saw_disconnect = true;
            }
        }
        assert!(saw_disconnect, "expected a Disconnected event after dropping c1");

        drop(c2);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());

        let _ = std::fs::remove_file(&path);
    }
}