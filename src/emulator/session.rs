use super::{Cpu, ErrorReceiver};

pub struct EmulatorSession {
    /// Fully configured CPU for the session.
    pub cpu: Cpu,
    /// Receiver for errors emitted by machine components.
    pub error_receiver: ErrorReceiver,
}

