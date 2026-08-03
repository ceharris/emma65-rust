//! Integration test: `Bus::drop` must signal every attached transport to
//! shut down and the whole teardown must complete promptly rather than
//! hanging, for both transport categories (P2P and multipoint).

use emma65::emulator::device::{Console, R6551};
use emma65::emulator::{
    AddressRange, Bus, DeviceId, InternalPipeTransport, TcpSocketTransport, TransportRelay,
    TransportReporter,
};
use tokio::net::TcpStream;

/// Builds a `Bus` with one P2P transport (`Console` over an
/// `InternalPipeTransport::pair()`, relay-backed) and one multipoint
/// transport (`R6551` over a `TcpSocketTransport` with a connected client),
/// then drops it. Confirms the drop completes within a bounded timeout
/// rather than hanging on a relay thread's `join()` — the failure mode
/// `Bus::drop`'s `IoDevice::shutdown()` wiring exists to prevent, by
/// signaling every attached transport to shut down before its `Drop` blocks
/// on the relay thread joining.
#[tokio::test(flavor = "multi_thread")]
async fn bus_drop_shuts_down_all_transports_without_hanging() {
    let ((pipe_local, pipe_relay), _pipe_remote) =
        InternalPipeTransport::pair(TransportReporter::pending(None)).unwrap();
    let mut console = Console::new("console").with_address(0x8000);
    console.attach_transport(Box::new(pipe_local), TransportRelay::Byte(pipe_relay));

    let (tcp_transport, tcp_relay) =
        TcpSocketTransport::listen("127.0.0.1:0".parse().unwrap(), TransportReporter::pending(None))
            .await
            .unwrap();
    let tcp_addr = tcp_transport.local_addr();
    let _client = TcpStream::connect(tcp_addr).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let mut acia = R6551::new("acia").with_address(0x8010);
    acia.attach_transport(Box::new(tcp_transport), TransportRelay::Tagged(tcp_relay));

    let bus = Bus::config()
        .device(AddressRange::new(0x8000, 0x8001), DeviceId(0), Box::new(console))
        .unwrap()
        .device(AddressRange::new(0x8010, 0x8013), DeviceId(1), Box::new(acia))
        .unwrap()
        .build();

    let dropped = tokio::task::spawn_blocking(move || drop(bus));
    tokio::time::timeout(std::time::Duration::from_secs(5), dropped)
        .await
        .expect("Bus::drop hung instead of completing promptly")
        .unwrap();
}
