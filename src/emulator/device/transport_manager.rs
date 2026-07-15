use std::fmt::Debug;
use crate::emulator::{Transport, TransportError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMessageEncoding {
    Ascii,
    Binary,
}

/// A message protocol encoder.
pub trait TransportMessageEncoder<T>: Send {

    /// Locks the encoder into binary mode.
    fn select_binary(&mut self);

    /// Returns the encoding currently used by this encoder.
    fn encoding(&self) -> TransportMessageEncoding;

    /// Encodes `message` appending the encoded form to `out`.
    fn encode(&mut self, message: &T, out: &mut Vec<u8>);

    /// Resets this encoder to its initial state.
    fn reset(&mut self);

}

/// A message protocol decoder.
pub trait TransportMessageDecoder<T>: Send {

    /// Returns the selected encoding.
    fn encoding(&self) -> Option<TransportMessageEncoding>;

    /// Feeds the byte `b` received from the transport into the decoder's state machine.
    /// Returns `Some(T)` if the state machine outputs a valid message, otherwise `None`.
    fn feed(&mut self, b: u8) -> Option<T>;

    /// Resets this decoder its to initial state.
    fn reset(&mut self);

}

struct TransportSlot<T> {
    transport: Box<dyn Transport>,
    encoder: Box<dyn TransportMessageEncoder<T>>,
    decoder: Box<dyn TransportMessageDecoder<T>>,
    handshake_done: bool,
    last_connection_id: u64,
}

impl<T> TransportSlot<T> {

    fn new(transport: Box<dyn Transport>,
           encoder: Box<dyn TransportMessageEncoder<T>>,
           decoder: Box<dyn TransportMessageDecoder<T>>) -> Self {
        let last_connection_id = transport.connection_id();
        Self {
            transport,
            encoder,
            decoder,
            handshake_done: false,
            last_connection_id,
        }
    }

    /// Resets handshake and codec state for a new peripheral session.
    fn reset(&mut self) {
        self.encoder.reset();
        self.decoder.reset();
        self.handshake_done = false;
    }

    /// Sends `message` to the connected peripheral if the handshake has
    /// been completed.
    fn send(&mut self, message: &T) -> Result<(), TransportError> {
        if self.handshake_done {
            let mut bytes = Vec::new();
            self.encoder.encode(message, &mut bytes);
            for b in bytes {
                self.transport.send(b)?;
            }
        }
        Ok(())
    }

    /// Sends `messages` to the connected peripheral.
    /// Panics if the handshake has not been completed.
    fn send_all(&mut self, messages: &[T]) -> Result<(), TransportError> {
        assert!(self.handshake_done);
        for message in messages.iter() {
            self.send(message)?
        }
        Ok(())
    }

    fn poll(&mut self, state: &[T]) -> Result<Option<T>, TransportError> {
        let current_id = self.transport.connection_id();
        if current_id != self.last_connection_id {
            self.last_connection_id = current_id;
            self.reset();
        }

        while let Some(byte) = self.transport.try_recv() {
            let msg = self.decoder.feed(byte);

            // First qualifying byte locks the format → complete the handshake.
            if !self.handshake_done && self.decoder.encoding().is_some() {
                self.handshake_done = true;
                if self.decoder.encoding() == Some(TransportMessageEncoding::Binary) {
                    self.encoder.select_binary();
                }
                self.send_all(state)?;
            }

            if let Some(m) = msg {
                return Ok(Some(m))
            }
        }
        Ok(None)
    }
}

/// A transport manager takes responsibility for relaying peripheral protocol
/// messages between peripherals connected via a transport protocol and an
/// I/O device that accepts multiple concurrently connected peripherals.
///
/// For each transport connection, the manager accepts the initial protocol
/// handshake and provides a state dump from the I/O device. Subsequently, on
/// each call to the [`poll_transports`] method, it checks for a valid message
/// from any connected peripheral. Messages can be delivered to peripherals using
/// either the [`send_to_all`] or [`send_all_to_all`] methods.
pub struct TransportManager<T> {
    slots: Vec<TransportSlot<T>>,
}

impl<T> Default for TransportManager<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TransportManager<T> {

    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
        }
    }

    pub fn attach_transport(&mut self, transport: Box<dyn Transport>,
                            encoder: Box<dyn TransportMessageEncoder<T>>,
                            decoder: Box<dyn TransportMessageDecoder<T>>) {
        self.slots.push(TransportSlot::new(transport, encoder, decoder));
    }

    pub fn send_to_all(&mut self, message: &T) -> Result<(), TransportError> {
        for slot in self.slots.iter_mut() {
            slot.send(message)?
        }
        Ok(())
    }

    pub fn send_all_to_all(&mut self, messages: &[T]) -> Result<(), TransportError> {
        for message in messages.iter() {
            self.send_to_all(message)?;
        }
        Ok(())
    }

    pub fn poll_transports(&mut self, state: &[T]) -> Result<Option<T>, TransportError> {
        for slot in self.slots.iter_mut() {
            if let Some(message) = slot.poll(state)? {
                return Ok(Some(message))
            }
        }
        Ok(None)
    }

}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::net::UnixStream;
    use tokio::io::AsyncWriteExt;
    use tokio::time::{sleep, Duration};
    use crate::emulator::{UnixSocketTransport};
    use super::*;

    async fn unix_listener(path: &PathBuf) -> Result<Box<dyn Transport>, TransportError> {
        let transport = UnixSocketTransport::listen(path).await;
        match transport {
            Ok(transport) => Ok(Box::new(transport)),
            Err(e) => Err(TransportError::Io(e))
        }
    }

    async fn unix_connection(path: &PathBuf) -> Result<UnixStream, Box<dyn std::error::Error>> {
        Ok(UnixStream::connect(path).await?)
    }

    #[tokio::test]
    async fn test() {
        // TODO this needs to wait until the transport layer is fixed to handle
        //      multiple concurrent clients
        let tmp_dir = TempDir::new().unwrap();
        let socket_path = tmp_dir.path().join("test.sock");
        let mut listener = unix_listener(&socket_path).await.unwrap();
        let mut connection1 = unix_connection(&socket_path).await.unwrap();
        let mut connection2 = unix_connection(&socket_path).await.unwrap();

        connection1.write_all("hello1".as_bytes()).await.unwrap();
        connection1.flush().await.unwrap();
        connection2.write_all("hello2".as_bytes()).await.unwrap();
        connection2.flush().await.unwrap();
        sleep(Duration::from_millis(1)).await;
        while let Some(b) = listener.try_recv() {
            eprint!("{}", b as char);
        }
        eprintln!();
    }
}