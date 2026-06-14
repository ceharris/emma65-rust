pub mod acia6551;
pub mod console;
pub mod mc6850;
pub mod via;
pub mod via_protocol;

pub use self::acia6551::Acia6551;
pub use self::console::Console;
pub use self::mc6850::Mc6850;
pub use self::via::Via6522;
pub use self::via_protocol::{ViaProtocolDecoder, ViaProtocolEncoder, ViaProtocolFormat, ViaProtocolMessage};

use tokio::sync::mpsc;

/// Uniquely identifies a device registered on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(pub u32);

/// Asynchronous event emitted by a device to notify the host application of transport state changes.
#[derive(Debug)]
pub enum DeviceEvent {
    /// A transport connection was established for the given device.
    TransportConnected { device: DeviceId },
    /// A transport connection was lost.
    TransportDisconnected {
        /// The device that lost its transport.
        device: DeviceId,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn send_events_receive_from_other_thread() {
        let (sender, mut receiver) = device_event_channel();
        let id = DeviceId(1);

        let handle = thread::spawn(move || {
            sender.send(DeviceEvent::TransportConnected { device: id }).unwrap();
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
}

/// A device that can be mapped into the bus address space.
pub trait IoDevice: Send {
    /// Reads a byte from `offset` relative to the device's base address, with side effects.
    fn read(&mut self, offset: u16) -> u8;
    /// Writes `value` to `offset` relative to the device's base address.
    fn write(&mut self, offset: u16, value: u8);
    /// Reads a byte from `offset` relative to the device's base address, without side effects.
    fn peek(&self, offset: u16) -> u8;
    /// Advances device state by `cycles` clock cycles. Called after each CPU instruction.
    fn tick(&mut self, _cycles: u32) {}
    /// Returns `true` if this device is currently asserting an IRQ.
    fn irq_active(&self) -> bool { false }
    /// Consumes a pending NMI edge event from this device, returning `true` if one was pending.
    ///
    /// Called once per CPU step. Implementations set an internal flag on the triggering write and
    /// clear it here. The default returns `false` (no NMI capability).
    fn take_nmi(&mut self) -> bool { false }
    /// Returns a human-readable name for this device, used in diagnostics and tracing.
    fn name(&self) -> &str { "unknown" }
}