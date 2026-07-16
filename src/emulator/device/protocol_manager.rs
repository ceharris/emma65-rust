use crate::emulator::{Transport, TransportError, TransportEvent};
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

}

/// A message protocol decoder.
pub trait ProtocolMessageDecoder<T>: Send {

    /// Feeds the byte `b` received from the transport into the decoder's state machine.
    /// Returns `Some(T)` if the state machine outputs a valid message, otherwise `None`.
    fn feed(&mut self, b: u8) -> Option<T>;

}

type CodecSupplier<T> = fn(ProtocolMessageEncoding)
    -> (Box<dyn ProtocolMessageEncoder<T>>, Box<dyn ProtocolMessageDecoder<T>>);

struct ProtocolSlot<T> {
    client_tag: u8,
    encoder: Box<dyn ProtocolMessageEncoder<T>>,
    decoder: Box<dyn ProtocolMessageDecoder<T>>,
    initial_dump_sent: bool,
}

impl<T> ProtocolSlot<T> {

    fn new(client_tag: u8,
           encoding: ProtocolMessageEncoding,
           codec_supplier: CodecSupplier<T>) -> Self {
        let (encoder, decoder) = codec_supplier(encoding);
        Self {
            client_tag,
            encoder,
            decoder,
            initial_dump_sent: false,
        }
    }

    fn send(&mut self, message: &T, transport: &mut Box<dyn Transport>) -> Result<(), TransportError> {
        let mut bytes = Vec::new();
        self.encoder.encode(message, &mut bytes);
        for b in bytes {
            transport.send(b)?;
        }
        Ok(())
    }

     fn send_all(&mut self, messages: &[T], transport: &mut Box<dyn Transport>) -> Result<(), TransportError> {
        for message in messages.iter() {
            self.send(message, transport)?
        }
        Ok(())
    }

    fn feed(&mut self, b: u8) -> Option<T> {
        let msg = self.decoder.feed(b);
        if let Some(m) = msg {
            return Some(m)
        }
        None
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
    encoding: ProtocolMessageEncoding,
    transport: Box<dyn Transport>,
    codec_supplier: CodecSupplier<T>,
    slots: Vec<ProtocolSlot<T>>,
}

impl<T> ProtocolManager<T> {
    pub fn new(encoding: ProtocolMessageEncoding,
               transport: Box<dyn Transport>,
               codec_supplier: CodecSupplier<T>) -> Self {
        Self {
            encoding,
            transport,
            codec_supplier,
            slots: Vec::new(),
        }
    }

    pub fn send_to_all(&mut self, message: &T) -> Result<(), TransportError> {
        for slot in self.slots.iter_mut() {
            slot.send(message, &mut self.transport)?
        }
        Ok(())
    }

    pub fn send_all_to_all(&mut self, messages: &[T]) -> Result<(), TransportError> {
        for message in messages.iter() {
            self.send_to_all(message)?;
        }
        Ok(())
    }

    pub fn poll_transport(&mut self, init_state: &[T]) -> Result<Option<T>, TransportError> {
        while let Some(event) = self.transport.try_recv_tagged() {
            match event {
                TransportEvent::Connected(tag) => {
                    // Drop any stale slot for this tag before creating a fresh one —
                    // guards against the (rare) case of a wrapped/reassigned tag
                    // aliasing onto a still-referenced old connection.
                    self.slots.retain(|s| s.client_tag != tag);
                    let mut slot = ProtocolSlot::new(tag, self.encoding, self.codec_supplier);
                    slot.send_all(init_state, &mut self.transport)?;
                    slot.initial_dump_sent = true;
                    self.slots.push(slot);
                }
                TransportEvent::Data(tag, byte) => {
                    let slot = Self::find_slot(tag, &mut self.slots)
                        .expect("Data event for tag with no prior Connected event");
                    if let Some(message) = slot.feed(byte) {
                        return Ok(Some(message));
                    }
                }
                TransportEvent::Disconnected(tag) => {
                    self.slots.retain(|s| s.client_tag != tag);
                }
            }
        }
        Ok(None)
    }

    fn find_slot(tag: u8, slots: &mut [ProtocolSlot<T>]) -> Option<&mut ProtocolSlot<T>> {
        slots.iter_mut().find(|s| s.client_tag == tag)
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