use crate::emulator::{Transport, TransportError};
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum ProtocolMessageEncoding {
    Ascii,
    Binary,
}

impl Display for ProtocolMessageEncoding {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolMessageEncoding::Ascii => write!(f, "ASCII"),
            ProtocolMessageEncoding::Binary => write!(f, "Binary"),
        }
    }
}

impl From<ProtocolMessageEncoding> for String {
    fn from(v: ProtocolMessageEncoding) -> Self {
        v.to_string()
    }
}

impl TryFrom<String> for ProtocolMessageEncoding {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl FromStr for ProtocolMessageEncoding {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower_s = s.to_ascii_lowercase();
        let ls = lower_s.as_str();
        match ls {
            "ascii" => Ok(ProtocolMessageEncoding::Ascii),
            "binary" => Ok(ProtocolMessageEncoding::Binary),
            _ => Err(format!("Invalid transport message encoding '{s}'; try '{}' or '{}'",
                             ProtocolMessageEncoding::Ascii, ProtocolMessageEncoding::Binary)),
        }
    }

}

/// A message protocol encoder.
pub trait ProtocolMessageEncoder<T>: Send {

    /// Encodes `message` appending the encoded form to `out`.
    fn encode(&mut self, message: &T, out: &mut Vec<u8>);

    /// Resets this encoder to its initial state.
    fn reset(&mut self);

}

/// A message protocol decoder.
pub trait ProtocolMessageDecoder<T>: Send {

    /// Feeds the byte `b` received from the transport into the decoder's state machine.
    /// Returns `Some(T)` if the state machine outputs a valid message, otherwise `None`.
    fn feed(&mut self, b: u8) -> Option<T>;

    /// Resets this decoder its to initial state.
    fn reset(&mut self);

}

struct ProtocolSlot<T> {
    transport: Box<dyn Transport>,
    encoder: Box<dyn ProtocolMessageEncoder<T>>,
    decoder: Box<dyn ProtocolMessageDecoder<T>>,
    initial_dump_sent: bool,
    last_connection_id: u64,
}

impl<T> ProtocolSlot<T> {

    fn new(transport: Box<dyn Transport>,
           encoder: Box<dyn ProtocolMessageEncoder<T>>,
           decoder: Box<dyn ProtocolMessageDecoder<T>>) -> Self {
        let last_connection_id = transport.connection_id();
        Self {
            transport,
            encoder,
            decoder,
            initial_dump_sent: false,
            last_connection_id,
        }
    }

    /// Resets codec state for a new peripheral session.
    fn reset(&mut self) {
        self.encoder.reset();
        self.decoder.reset();
    }

    /// Sends `message` to the connected peripheral if the handshake has
    /// been completed.
    fn send(&mut self, message: &T) -> Result<(), TransportError> {
        let mut bytes = Vec::new();
        self.encoder.encode(message, &mut bytes);
        for b in bytes {
            self.transport.send(b)?;
        }
        Ok(())
    }

    /// Sends `messages` to the connected peripheral.
    fn send_all(&mut self, messages: &[T]) -> Result<(), TransportError> {
        for message in messages.iter() {
            self.send(message)?
        }
        Ok(())
    }

    fn poll(&mut self, state: &[T]) -> Result<Option<T>, TransportError> {
        let current_id = self.transport.connection_id();
        if current_id != self.last_connection_id || !self.initial_dump_sent {
            self.last_connection_id = current_id;
            self.initial_dump_sent = true;
            self.reset();
            self.send_all(state)?;
        }

        while let Some(byte) = self.transport.try_recv() {
            let msg = self.decoder.feed(byte);

            if let Some(m) = msg {
                return Ok(Some(m))
            }
        }
        Ok(None)
    }
}

/// A protocol manager takes responsibility for relaying peripheral protocol
/// messages between peripherals connected via a transport protocol and an
/// I/O device that accepts multiple concurrently connected peripherals.
///
/// For each transport connection, the manager provides a state dump from the 
/// I/O device. Subsequently, on each call to the [`poll_transports`] method, it 
/// checks for a valid message from any connected peripheral. Messages can be 
/// delivered to peripherals using either the [`send_to_all`] or [`send_all_to_all`] 
/// methods.
pub struct ProtocolManager<T> {
    slots: Vec<ProtocolSlot<T>>,
}

impl<T> Default for ProtocolManager<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ProtocolManager<T> {

    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
        }
    }

    pub fn attach_transport(&mut self, transport: Box<dyn Transport>,
                            encoder: Box<dyn ProtocolMessageEncoder<T>>,
                            decoder: Box<dyn ProtocolMessageDecoder<T>>) {
        self.slots.push(ProtocolSlot::new(transport, encoder, decoder));
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
    use super::*;
    use crate::emulator::UnixSocketTransport;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;
    use tokio::time::{sleep, Duration};

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