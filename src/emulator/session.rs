//! Emulator session configuration.
use super::bus::DeviceIdAllocator;
use super::{Cpu, ErrorReceiver};

// Emulator session for use in a CLI or UI context.
pub struct EmulatorSession {
    /// Fully configured CPU for the session.
    pub cpu: Cpu,
    /// Receiver for errors emitted by machine components.
    pub error_receiver: ErrorReceiver,
    /// Device ID allocator, left in its post-configuration state so callers
    /// (e.g. the debugger UI) can allocate additional IDs — such as an IRQ
    /// source for a UI-driven control — guaranteed not to collide with any
    /// configured device.
    pub id_allocator: DeviceIdAllocator,
}

