//! IO device trait, device identification, and async device event channel, built-in devices.
pub mod r6551;
pub mod console;
pub mod mc6850;
pub mod phoebe;
pub mod via6522;
pub mod via_protocol;
mod ring;

pub use self::r6551::R6551;
pub use self::console::Console;
pub use self::mc6850::Mc6850;
pub use self::phoebe::Phoebe;
pub use self::via6522::Via6522;
pub use self::via_protocol::{ViaProtocolDecoder, ViaProtocolEncoder, ViaProtocolFormat, ViaProtocolMessage};

use std::fmt::{Display, Formatter, Result};
use tokio::sync::mpsc;

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
    /// A report of an attempted write to a read-only register/location in a device
    RejectedWrite {
        device: DeviceId,
        address: u16,
    }
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
    /// The address at which this device is registered via `BusConfig::device()`.
    ///
    /// Used by the default `*_absolute` methods to translate an absolute bus address
    /// into a range-relative offset. A device that overrides all three `*_absolute`
    /// methods (and `claims`) is free to return any value here, since nothing else
    /// consults it.
    fn base_address(&self) -> u16;

    /// Reads a byte from `offset` relative to `base_address()`, with side effects.
    fn read_relative(&mut self, offset: u16) -> u8;
    /// Writes `value` to `offset` relative to `base_address()`.
    fn write_relative(&mut self, offset: u16, value: u8);
    /// Reads a byte from `offset` relative to `base_address()`, without side effects.
    fn peek_relative(&self, offset: u16) -> u8;

    /// Reads a byte at the absolute bus address `addr`, with side effects.
    ///
    /// The default implementation subtracts `base_address()` and delegates to `read_relative()` —
    /// correct for any device mapped at a single region. A device mapped at more than
    /// one region (via `BusConfig::extend_device()`) overrides this directly,
    /// classifying `addr` against whatever address information it retains for its own
    /// regions.
    fn read_absolute(&mut self, addr: u16) -> u8 {
        self.read_relative(addr - self.base_address())
    }

    /// Writes `value` at the absolute bus address `addr`.
    /// The default implementation subtracts `base_address()` and delegates to `write_relative()` —
    /// correct for any device mapped at a single region. A device mapped at more than
    /// one region (via `BusConfig::extend_device()`) overrides this directly,
    /// classifying `addr` against whatever address information it retains for its own
    /// regions.
    fn write_absolute(&mut self, addr: u16, value: u8) {
        self.write_relative(addr - self.base_address(), value)
    }

    /// Reads a byte at the absolute bus address `addr`, without side effects.
    /// The default implementation subtracts `base_address()` and delegates to `peek_relative()` —
    /// correct for any device mapped at a single region. A device mapped at more than
    /// one region (via `BusConfig::extend_device()`) overrides this directly,
    /// classifying `addr` against whatever address information it retains for its own
    /// regions.
    fn peek_absolute(&self, addr: u16) -> u8 {
        self.peek_relative(addr - self.base_address())
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
    /// Returns a human-readable name for this device, used in diagnostics and tracing.
    fn name(&self) -> &str { "unknown" }
}