//! An error reporter directs transport errors resulting from I/O with a connected
//! peripheral to an [`ErrorSender`].
//!
use crate::emulator::{DeviceEvent, DeviceId, ErrorSender, TransportError};


/// A transport error reporter.
pub struct ErrorReporter {
    error_sender: ErrorSender,
    device_id: DeviceId,
}

impl ErrorReporter {

    // Creates a new reporter for the device identified as `device_id` and
    // and targeting the given [`ErrorSender`]
    pub fn new(error_sender: ErrorSender, device_id: DeviceId) -> Self {
        Self {
            error_sender,
            device_id,
        }
    }

    fn report(&self, error: TransportError) {
        let _ = self.error_sender.send(DeviceEvent::TransportError { device: self.device_id, error });
    }

}

/// Reports a transport error if an [`ErrorReporter`] is available.
pub fn report(error: TransportError, error_reporter: Option<&mut ErrorReporter>) {
    if let Some(reporter) = error_reporter {
        reporter.report(error);
    }
}