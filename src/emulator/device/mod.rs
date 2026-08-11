//! IO device trait, device identification, and async device event channel, built-in devices.
pub mod r6551;
pub mod console;
pub mod finch;
pub mod lfsr;
pub mod mc6840;
pub mod mc6850;
pub mod phoebe;
pub mod pic_finch;
pub mod via6522;
mod ring;
pub mod vireo;
pub mod protocol;
pub mod led_matrix;

pub use self::console::Console;
pub use self::finch::Finch;
pub use self::lfsr::Lfsr16;
pub use self::mc6840::Mc6840;
pub use self::mc6850::Mc6850;
pub use self::phoebe::Phoebe;
pub use self::pic_finch::PicFinch;
pub use protocol::{ProtocolMessageDecoder, ProtocolMessageEncoder, ProtocolMessageEncoding};
pub use protocol::ptm::{PtmAsciiProtocolDecoder, PtmAsciiProtocolEncoder, PtmBinaryProtocolDecoder, PtmBinaryProtocolEncoder, PtmProtocolMessage};
pub use self::r6551::R6551;
pub use self::via6522::Via6522;
pub use protocol::via::{ViaAsciiProtocolDecoder, ViaAsciiProtocolEncoder, ViaBinaryProtocolDecoder, ViaBinaryProtocolEncoder, ViaProtocolMessage};
pub use self::vireo::Vireo;

use std::fmt::{Display, Formatter, Result};
use tokio::sync::mpsc;

use crate::emulator::{LogCategory, LogLevel, LogSender, log_msg};

/// Uniquely identifies a device registered on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(pub u32);

impl Display for DeviceId {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        let DeviceId(addr) = self;
        write!(f, "@{:04x}", addr)
    }
}

/// Asynchronous event emitted by a device to notify the host application of transport state changes.
#[derive(Debug)]
pub enum DeviceEvent {
    /// A transport connection was established for the given device.
    TransportConnected {
        /// The device that gained a transport connection.
        device: DeviceId,
        /// Identifies which peer connected, for transports that accept more
        /// than one concurrent client (e.g. TCP/Unix socket). `None` for
        /// point-to-point transports, which have only one peer to
        /// disambiguate.
        peer: Option<String>,
    },
    /// A transport connection was lost.
    TransportDisconnected {
        /// The device that lost its transport.
        device: DeviceId,
        /// Identifies which peer disconnected; see
        /// [`TransportConnected`](DeviceEvent::TransportConnected)'s `peer`.
        peer: Option<String>,
        /// Human-readable reason for the disconnection.
        reason: String,
    },
    /// A transport error occurred during IO.
    TransportError {
        /// The device that encountered the error.
        device: DeviceId,
        /// The error that occurred.
        error: crate::emulator::transport::TransportError,
    },
    /// An informational message from the device.
    DeviceInfo {
        /// The device emitting the message.
        device: DeviceId,
        /// The message text.
        message: String,
    },
    /// A report of an attempted write to a read-only register/location in a device
    RejectedWrite {
        device: DeviceId,
        address: u16,
    },
    /// Diagnostic count of outbound bytes dropped due to a full outbound
    /// ring buffer. Purely diagnostic (never gates device/transport
    /// behavior); the expected remediation is to resize the ring and
    /// restart.
    OutboundBytesDropped {
        /// The device whose outbound ring overflowed.
        device: DeviceId,
        /// Number of bytes dropped since the last report.
        count: u64,
    },
    /// Diagnostic count of inbound events dropped due to a full ingress
    /// channel, for multipoint transports only. Purely diagnostic; see
    /// [`OutboundBytesDropped`](DeviceEvent::OutboundBytesDropped).
    InboundEventsDropped {
        /// The device whose inbound ingress channel overflowed.
        device: DeviceId,
        /// Number of events dropped since the last report.
        count: u64,
    },
}

/// Sending half of a device event channel.
///
/// Devices hold an `ErrorSender` to report asynchronous transport events to the host.
pub type ErrorSender = mpsc::UnboundedSender<DeviceEvent>;

/// Receiving half of a device event channel.
///
/// The host holds an `ErrorReceiver` and polls it independently of CPU execution.
pub type ErrorReceiver = mpsc::UnboundedReceiver<DeviceEvent>;

/// Creates a new `(ErrorSender, ErrorReceiver)` pair for device event reporting.
pub fn device_event_channel() -> (ErrorSender, ErrorReceiver) {
    mpsc::unbounded_channel()
}

/// Logs `event` through `sender` under `LogCategory::Transport`, at a severity appropriate to
/// its kind. This is a second, always-available path to the user alongside whatever host-specific
/// reaction a caller's `ErrorReceiver` consumption loop performs (e.g. the CLI's own
/// connect/disconnect messages) — a host with no such loop of its own (e.g. the debugger, today)
/// still gets these messages via the log facility instead of losing them entirely.
pub fn log_device_event(sender: &LogSender, event: &DeviceEvent) {
    match event {
        DeviceEvent::TransportConnected { device, peer: Some(peer) } =>
            log_msg!(sender, LogLevel::Info, LogCategory::Transport, "{device} connected: {peer}"),
        DeviceEvent::TransportConnected { device, peer: None } =>
            log_msg!(sender, LogLevel::Info, LogCategory::Transport, "{device} connected"),
        DeviceEvent::TransportDisconnected { device, peer: Some(peer), reason } =>
            log_msg!(sender, LogLevel::Warn, LogCategory::Transport, "{device} disconnected: {reason} ({peer})"),
        DeviceEvent::TransportDisconnected { device, peer: None, reason } =>
            log_msg!(sender, LogLevel::Warn, LogCategory::Transport, "{device} disconnected: {reason}"),
        DeviceEvent::TransportError { device, error } =>
            log_msg!(sender, LogLevel::Error, LogCategory::Transport, "{device} transport error: {error}"),
        DeviceEvent::DeviceInfo { device, message } =>
            log_msg!(sender, LogLevel::Info, LogCategory::Transport, "{device}: {message}"),
        DeviceEvent::RejectedWrite { device, address } =>
            log_msg!(sender, LogLevel::Warn, LogCategory::Transport, "{device} rejected write at 0x{address:04x}"),
        DeviceEvent::OutboundBytesDropped { device, count } =>
            log_msg!(sender, LogLevel::Warn, LogCategory::Transport, "{device} dropped {count} outbound bytes"),
        DeviceEvent::InboundEventsDropped { device, count } =>
            log_msg!(sender, LogLevel::Warn, LogCategory::Transport, "{device} dropped {count} inbound events"),
    }
}

/// Drains `receiver`, logging every event through `sender` via [`log_device_event`], until every
/// `ErrorSender` clone for this channel has been dropped. Intended to be spawned as a background
/// task by a host that has no other use for its `ErrorReceiver` (see [`log_device_event`]).
pub async fn log_device_events(mut receiver: ErrorReceiver, sender: LogSender) {
    while let Some(event) = receiver.recv().await {
        log_device_event(&sender, &event);
    }
}

/// A device that can be mapped into the bus address space.
pub trait IoDevice: Send {

    /// Reads a byte at the absolute bus address `address`, with side effects.
    fn read(&mut self, address: u16) -> u8;

    /// Writes `value` at the absolute bus address `address`.
    fn write(&mut self, address: u16, value: u8);

    /// Reads a byte at the absolute bus address `address`, without side effects.
    fn peek(&self, address: u16) -> u8;

    /// Writes a `value` at the absolute bus address `address`, bypassing read-only restrictions
    /// at the target device.
    fn patch(&mut self, address: u16, value: u8) {
        self.write(address, value);
    }

    /// Returns `true` if this device currently responds to `addr`, the absolute bus
    /// address. Consulted before dispatching `*_absolute`; declining causes the bus to
    /// fall through to the next most-specific region containing `addr`, or to the
    /// unmapped-address policy if none remain.
    ///
    /// Default implementation always claims (unconditional chip-select).
    fn claims(&self, _addr: u16) -> bool { true }

    /// Advances device state by `cycles` clock cycles. Called after each CPU instruction.
    fn tick(&mut self, _cycles: u32) {}

    /// Resets the state of the device in a manner comparable to a hardware reset
    fn reset(&mut self) {}

    /// Returns `true` if this device is currently asserting an IRQ.
    fn irq_active(&self) -> bool { false }

    /// Consumes a pending NMI edge event from this device, returning `true` if one was pending.
    ///
    /// Called once per CPU step. Implementations set an internal flag on the triggering write and
    /// clear it here. The default returns `false` (no NMI capability).
    fn take_nmi(&mut self) -> bool { false }

    /// Consumes a pending device-initiated CPU RESET request, returning `true` if one was pending.
    ///
    /// Called once per CPU step, alongside `take_nmi`. Implementations set an internal flag when
    /// the device wants to reset the CPU (e.g. a watchdog timer or reset-button peripheral) and
    /// clear it here. The default returns `false` (no reset capability); none of this crate's
    /// built-in devices use it.
    fn take_reset(&mut self) -> bool { false }

    /// Returns a human-readable name for this device, used in diagnostics and tracing.
    fn name(&self) -> &str { "unknown" }

    /// Signals any transport owned by this device to begin shutdown.
    ///
    /// Called by `Bus::drop` on every device before Rust's normal field-drop
    /// order runs each device's own `Drop` impl, so that the owning
    /// transport's background task has a chance to start its shutdown
    /// branch (dropping its inbound sender first, per the shutdown contract
    /// documented in `transport::mod`) before that same device's `Drop`
    /// reaches its `ChannelRelay` field and blocks on `join()`.
    ///
    /// Default no-op, so devices without a transport need no changes.
    fn shutdown(&mut self) {}

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn send_events_receive_from_other_thread() {
        let (sender, mut receiver) = device_event_channel();
        let id = DeviceId(1);

        let handle = thread::spawn(move || {
            sender.send(DeviceEvent::TransportConnected { device: id, peer: None }).unwrap();
            sender.send(DeviceEvent::DeviceInfo {
                device: id,
                message: "hello".to_string(),
            }).unwrap();
        });

        handle.join().unwrap();

        // Use tokio runtime to drive the async receiver.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let ev1 = receiver.recv().await.unwrap();
            assert!(matches!(ev1, DeviceEvent::TransportConnected { .. }));
            let ev2 = receiver.recv().await.unwrap();
            assert!(matches!(ev2, DeviceEvent::DeviceInfo { .. }));
        });
    }

    #[test]
    fn log_device_event_reports_transport_error_at_error_level() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let device = DeviceId(0xc000);

        log_device_event(&sender, &DeviceEvent::TransportError {
            device,
            error: crate::emulator::TransportError::Disconnected,
        });

        let received = rx.recv().unwrap();
        assert_eq!(received.level, LogLevel::Error);
        assert_eq!(received.category, LogCategory::Transport);
        assert_eq!(received.message, format!("{device} transport error: disconnected"));
    }

    #[test]
    fn log_device_event_reports_connected_with_and_without_peer() {
        let (sender, rx) = crate::emulator::logging::test_channel_sender(4);
        let device = DeviceId(0xc000);

        log_device_event(&sender, &DeviceEvent::TransportConnected { device, peer: Some("client-1".to_string()) });
        log_device_event(&sender, &DeviceEvent::TransportConnected { device, peer: None });

        assert_eq!(rx.recv().unwrap().message, format!("{device} connected: client-1"));
        assert_eq!(rx.recv().unwrap().message, format!("{device} connected"));
    }

    #[tokio::test]
    async fn log_device_events_drains_until_every_sender_is_dropped() {
        let (event_tx, event_rx) = device_event_channel();
        let (log_tx, log_rx) = crate::emulator::logging::test_channel_sender(4);
        let device = DeviceId(1);

        event_tx.send(DeviceEvent::DeviceInfo { device, message: "hello".to_string() }).unwrap();
        drop(event_tx);

        log_device_events(event_rx, log_tx).await;

        let received = log_rx.recv().unwrap();
        assert_eq!(received.message, format!("{device}: hello"));
        assert!(log_rx.try_recv().is_err());
    }

}
