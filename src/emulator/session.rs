//! Emulator session configuration.
use super::{Cpu, ErrorReceiver};

// Emulator session for use in a CLI or UI context.
pub struct EmulatorSession {
    /// Fully configured CPU for the session.
    pub cpu: Cpu,
    /// Receiver for errors emitted by machine components.
    pub error_receiver: ErrorReceiver,
}

