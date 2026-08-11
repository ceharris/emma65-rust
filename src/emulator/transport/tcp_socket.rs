//! Transport that listens for incoming TCP connections.
//!
//! A Tokio task owns the `TcpListener` and accepts connections in a loop,
//! spawning a per-client task for each one so multiple clients can be
//! connected concurrently — the "multipoint" transport shape, contrasted
//! with point-to-point transports like
//! [`PtyTransport`](super::PtyTransport). Inbound bytes from every
//! client are tagged with their originating connection and relayed through a
//! single [`ChannelRelay<TransportEvent>`](ChannelRelay), the same mechanism
//! point-to-point transports use for `ChannelRelay<u8>`. Outbound bytes are
//! pushed into an `rtrb::Producer<u8>` by `send()` (never blocking; overflow
//! is counted via [`TransportReporter`], not surfaced as an error) and
//! fanned out to every connected client via a `broadcast::channel`.
//!
//! The accept loop itself (`pump_outbound`, `run_client_task`,
//! `ClientSession`, and the loop body driving them) is shared with
//! [`UnixSocketTransport`](super::UnixSocketTransport) via
//! `super::run_listener_task`, generic over the `ClientListener` trait —
//! this module supplies only the `TcpListener`-specific `accept`/peer-naming
//! logic (PR #227 review). Construction and the `Transport` plumbing
//! (`send`/`is_connected`/`shutdown`) are likewise shared via
//! `super::ListenerCore`.

use std::net::SocketAddr;

use tokio::net::TcpListener;

use super::{CHANNEL_CAPACITY, ChannelRelay, ClientListener, ListenerCore, Transport, TransportEvent, TransportReporter};

/// Transport that listens for incoming TCP socket connections.
pub struct TcpSocketTransport {
    /// Shared listener state; see [`ListenerCore`].
    core: ListenerCore,
    /// Address the listener is bound to.
    local_addr: SocketAddr,
}

impl TcpSocketTransport {
    /// Binds a TCP listener at `addr` and starts accepting connections,
    /// using the crate's default channel/ring capacity for both the
    /// inbound relay and the outbound ring.
    pub async fn listen(addr: SocketAddr, reporter: TransportReporter) -> std::io::Result<(Self, ChannelRelay<TransportEvent>)> {
        Self::listen_with_capacity(addr, reporter, CHANNEL_CAPACITY).await
    }

    /// Same as [`listen`](Self::listen), with the inbound/outbound ring
    /// capacity parameterized. `pub(crate)` and used only by the drop-count
    /// tests below, to force deterministic ring/channel overflow (capacity
    /// 1) rather than relying on timing.
    pub(crate) async fn listen_with_capacity(
        addr: SocketAddr,
        reporter: TransportReporter,
        capacity: usize,
    ) -> std::io::Result<(Self, ChannelRelay<TransportEvent>)> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;

        let (core, relay) = ListenerCore::spawn(listener, reporter, capacity);
        Ok((Self { core, local_addr }, relay))
    }

    /// Returns the address the listener is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

impl Transport for TcpSocketTransport {
    fn send(&mut self, byte: u8) {
        self.core.send(byte);
    }

    fn is_connected(&self) -> bool {
        self.core.is_connected()
    }

    fn shutdown(&mut self) {
        self.core.shutdown();
    }
}

impl ClientListener for TcpListener {
    type Reader = tokio::net::tcp::OwnedReadHalf;
    type Writer = tokio::net::tcp::OwnedWriteHalf;
    /// `Ok`/`Err` from `peer_addr()` itself, so [`format_peer`](Self::format_peer)
    /// can fall back to `conn_tag` on error, same as Unix sockets.
    type PeerInfo = std::io::Result<SocketAddr>;

    async fn accept(&self) -> std::io::Result<(Self::Reader, Self::Writer, Self::PeerInfo)> {
        let (stream, _) = TcpListener::accept(self).await?;
        let peer_info = stream.peer_addr();
        let (reader, writer) = stream.into_split();
        Ok((reader, writer, peer_info))
    }

    fn format_peer(info: Self::PeerInfo, conn_tag: u8) -> String {
        info.map(|addr| addr.to_string()).unwrap_or_else(|_| format!("conn#{conn_tag}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::{DeviceEvent, device_event_channel};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn only_data(events: Vec<TransportEvent>) -> Vec<(u8, u8)> {
        events.into_iter().filter_map(|e| match e { TransportEvent::Data(tag, byte) => Some((tag, byte)), _ => None }).collect()
    }

    /// Shuts `transport` down (stopping the background Tokio tasks, not just
    /// the relay) and drops it plus `relay`. Must be called before a test
    /// ends if any client connection is still expected to be alive —
    /// dropping `relay` first would stop its relay thread and disconnect
    /// `in_tx`, causing client tasks to exit early.
    fn close(mut transport: TcpSocketTransport, relay: ChannelRelay<TransportEvent>) {
        transport.shutdown();
        drop((transport, relay));
    }

    async fn make_transport() -> (TcpSocketTransport, ChannelRelay<TransportEvent>, SocketAddr) {
        let (t, relay) = TcpSocketTransport::listen("127.0.0.1:0".parse().unwrap(), TransportReporter::pending(None)).await.unwrap();
        let addr = t.local_addr();
        (t, relay, addr)
    }

    #[tokio::test]
    async fn listen_accept_send_recv() {
        let (mut transport, mut relay, addr) = make_transport().await;

        let mut client = TcpStream::connect(addr).await.unwrap();
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
    }

    #[tokio::test]
    async fn reconnection() {
        let (transport, mut relay, addr) = make_transport().await;

        let mut c1 = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        c1.write_all(&[0x01]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut got = Vec::new();
        relay.drain_into(|event| got.push(event));
        assert_eq!(only_data(got).into_iter().map(|(_, b)| b).collect::<Vec<_>>(), vec![0x01]);
        drop(c1);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let mut c2 = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        c2.write_all(&[0x02]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let mut got2 = Vec::new();
        relay.drain_into(|event| got2.push(event));
        assert_eq!(only_data(got2).into_iter().map(|(_, b)| b).collect::<Vec<_>>(), vec![0x02]);

        close(transport, relay);
    }

    #[tokio::test]
    async fn send_while_no_client() {
        let (mut transport, relay, _addr) = make_transport().await;

        assert!(!transport.is_connected());
        transport.send(0xFF);

        close(transport, relay);
    }

    #[tokio::test]
    async fn send_while_no_client_does_not_count_as_a_drop() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind("test-device-103");
        let (mut transport, relay) = TcpSocketTransport::listen("127.0.0.1:0".parse().unwrap(), reporter.clone()).await.unwrap();

        assert!(!transport.is_connected());
        for _ in 0..10 {
            transport.send(0xFF);
        }

        reporter.report_counts();
        assert!(receiver.try_recv().is_err(), "sending with no client connected must not count as an outbound drop");

        close(transport, relay);
    }

    #[tokio::test]
    async fn is_connected_reflects_client_state() {
        let (transport, relay, addr) = make_transport().await;

        assert!(!transport.is_connected());

        let client = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(transport.is_connected());

        drop(client);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());

        close(transport, relay);
    }

    #[tokio::test]
    async fn shutdown() {
        let (mut transport, relay, addr) = make_transport().await;

        let _client = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        transport.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!transport.is_connected());

        close(transport, relay);
    }

    #[tokio::test]
    async fn concurrent_clients_are_tagged_and_counted() {
        let (mut transport, mut relay, addr) = make_transport().await;

        let mut c1 = TcpStream::connect(addr).await.unwrap();
        let mut c2 = TcpStream::connect(addr).await.unwrap();
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
        assert_ne!(connected_tags[0], connected_tags[1]);

        assert_eq!(data.len(), 2);
        assert_ne!(data[0].0, data[1].0);
        for (tag, _) in &data {
            assert!(connected_tags.contains(tag));
        }

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
        assert!(transport.is_connected());

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
    }

    #[tokio::test]
    async fn outbound_overflow_increments_drop_counter_and_is_reported() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind("test-device-99");
        let (mut transport, relay) = TcpSocketTransport::listen_with_capacity("127.0.0.1:0".parse().unwrap(), reporter.clone(), 1).await.unwrap();
        let addr = transport.local_addr();

        let _client = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(transport.is_connected());
        assert!(matches!(receiver.try_recv(), Ok(DeviceEvent::TransportConnected { .. })));

        // Capacity 1, two sends back-to-back with no `.await` in between —
        // the spawned Tokio task can't be scheduled to drain in between, so
        // the second send is guaranteed to see the ring still full.
        transport.send(0x01);
        transport.send(0x02);

        reporter.report_counts();

        match receiver.try_recv() {
            Ok(DeviceEvent::OutboundBytesDropped { device, count }) => {
                assert_eq!(device, "test-device-99");
                assert!(count >= 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        close(transport, relay);
    }

    #[tokio::test]
    async fn inbound_overflow_increments_drop_counter_and_is_reported() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind("test-device-100");
        let (transport, relay) = TcpSocketTransport::listen_with_capacity("127.0.0.1:0".parse().unwrap(), reporter.clone(), 1).await.unwrap();
        let addr = transport.local_addr();

        let mut client = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(matches!(receiver.try_recv(), Ok(DeviceEvent::TransportConnected { .. })));

        // Never drain the relay: with channel/ring capacity 1, a burst large
        // enough is guaranteed to overflow before it's exhausted.
        let burst = [0xAAu8; 64];
        client.write_all(&burst).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        reporter.report_counts();

        match receiver.try_recv() {
            Ok(DeviceEvent::InboundEventsDropped { device, count }) => {
                assert_eq!(device, "test-device-100");
                assert!(count >= 1);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        close(transport, relay);
    }

    #[tokio::test]
    async fn client_connect_disconnect_reports_peer_events() {
        let (sender, mut receiver) = device_event_channel();
        let reporter = TransportReporter::pending(Some(sender));
        reporter.bind("test-device-102");
        let (transport, relay) = TcpSocketTransport::listen("127.0.0.1:0".parse().unwrap(), reporter).await.unwrap();
        let addr = transport.local_addr();

        let client = TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let peer = match receiver.try_recv() {
            Ok(DeviceEvent::TransportConnected { device, peer: Some(peer) }) => {
                assert_eq!(device, "test-device-102");
                assert!(!peer.is_empty());
                peer
            }
            other => panic!("unexpected event: {other:?}"),
        };

        drop(client);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        match receiver.try_recv() {
            Ok(DeviceEvent::TransportDisconnected { device, peer: Some(disconnected_peer), .. }) => {
                assert_eq!(device, "test-device-102");
                assert_eq!(disconnected_peer, peer);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        close(transport, relay);
    }
}
