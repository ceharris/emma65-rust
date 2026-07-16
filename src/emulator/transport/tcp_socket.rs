//! Transport that listens for incoming TCP connections.
//!
//! A Tokio task owns the `TcpListener` and accepts connections in a loop,
//! spawning a per-client task for each one so multiple clients can be
//! connected concurrently. Outbound bytes are fanned out to every connected
//! client; inbound bytes are tagged with their originating connection ID so
//! they can be demultiplexed downstream.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::{pump_outbound, run_client_task, ChannelBridge, ClientSession, TagAllocator, Transport, TransportError, TransportEvent, BROADCAST_CAPACITY};
use crossbeam_channel::{Receiver, Sender};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, oneshot, watch};

/// Transport that listens for incoming TCP connections.
///
/// Supports multiple concurrently connected clients. All clients receive the
/// same outbound byte stream (fan-out); inbound bytes are tagged with the ID
/// of the connection they came from (see [`Transport::try_recv_tagged`]).
pub struct TcpTransport {
    bridge: ChannelBridge<TransportEvent>,
    client_count: Arc<AtomicUsize>,
    local_addr: SocketAddr,
}

impl TcpTransport {
    pub async fn listen(addr: SocketAddr) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        let (bridge, in_tx, out_rx, shutdown_rx) = ChannelBridge::<TransportEvent>::new();
        let client_count = Arc::new(AtomicUsize::new(0));

        let (shutdown_watch_tx, shutdown_watch_rx) = watch::channel(false);
        tokio::spawn(propagate_shutdown(shutdown_rx, shutdown_watch_tx));

        tokio::spawn(run_tcp_task(
            listener,
            in_tx,
            out_rx,
            shutdown_watch_rx,
            Arc::clone(&client_count),
        ));
        Ok(Self { bridge, client_count, local_addr })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

impl Transport for TcpTransport {
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

async fn run_tcp_task(
    listener: TcpListener,
    in_tx: Sender<TransportEvent>,
    out_rx: Receiver<u8>,
    mut shutdown_rx: watch::Receiver<bool>,
    client_count: Arc<AtomicUsize>,
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
    use tokio::net::TcpStream;

    async fn make_transport() -> (TcpTransport, SocketAddr) {
        let t = TcpTransport::listen("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = t.local_addr();
        (t, addr)
    }

    #[tokio::test]
    async fn listen_accept_send_recv() {
        let (mut transport, addr) = make_transport().await;

        let mut client = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        client.write_all(&[0xAB]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv(), Some(0xAB));

        transport.send(0xCD).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut buf = [0u8; 1];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf[0], 0xCD);
    }

    #[tokio::test]
    async fn reconnection() {
        let (mut transport, addr) = make_transport().await;

        let mut c1 = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        c1.write_all(&[0x01]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv(), Some(0x01));
        drop(c1);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let mut c2 = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        c2.write_all(&[0x02]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert_eq!(transport.try_recv(), Some(0x02));
    }

    #[tokio::test]
    async fn send_while_no_client() {
        let (mut transport, _addr) = make_transport().await;

        assert!(!transport.is_connected());
        assert!(transport.send(0xFF).is_ok());
    }

    #[tokio::test]
    async fn is_connected_reflects_client_state() {
        let (transport, addr) = make_transport().await;

        assert!(!transport.is_connected());

        let client = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(transport.is_connected());

        drop(client);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());
    }

    #[tokio::test]
    async fn shutdown() {
        let (mut transport, addr) = make_transport().await;

        let _client = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        transport.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());
    }

    #[tokio::test]
    async fn concurrent_clients_are_tagged_and_counted() {
        let (mut transport, addr) = make_transport().await;

        let mut c1 = TcpStream::connect(addr).await.unwrap();
        let mut c2 = TcpStream::connect(addr).await.unwrap();
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
        assert_ne!(connected_tags[0], connected_tags[1]);

        assert_eq!(data.len(), 2);
        assert_ne!(data[0].0, data[1].0);
        for (tag, _) in &data {
            assert!(connected_tags.contains(tag));
        }

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
        assert!(transport.is_connected());

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
    }

}