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

type EncoderSupplier<T> = fn(encoding: ProtocolMessageEncoding) -> Box<dyn ProtocolMessageEncoder<T>>;
type DecoderSupplier<T> = fn(encoding: ProtocolMessageEncoding) -> Box<dyn ProtocolMessageDecoder<T>>;

/// Per-connection decode state. Encoding is stateless across connections
/// (every slot shares the same `ProtocolMessageEncoding`), so only the
/// decoder — which must track partial-message state per connection to
/// demultiplex correctly — lives here. Outgoing messages are encoded once,
/// centrally, by [`ProtocolManager`] and relayed to all clients via the
/// transport's own fan-out.
struct ProtocolSlot<T> {
    client_tag: u8,
    decoder: Box<dyn ProtocolMessageDecoder<T>>,
    initial_dump_sent: bool,
}

impl<T> ProtocolSlot<T> {

    fn new(client_tag: u8,
           encoding: ProtocolMessageEncoding,
           decoder_supplier: DecoderSupplier<T>) -> Self {
        let decoder = decoder_supplier(encoding);
        Self {
            client_tag,
            decoder,
            initial_dump_sent: false,
        }
    }

    fn feed(&mut self, b: u8) -> Option<T> {
        self.decoder.feed(b)
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
    decoder_supplier: DecoderSupplier<T>,
    encoder: Box<dyn ProtocolMessageEncoder<T>>,
    slots: Vec<ProtocolSlot<T>>,
}

impl<T> ProtocolManager<T> {
    pub fn new(encoding: ProtocolMessageEncoding,
               transport: Box<dyn Transport>,
               encoder_supplier: EncoderSupplier<T>,
               decoder_supplier: DecoderSupplier<T>) -> Self {
        Self {
            encoding,
            transport,
            decoder_supplier,
            encoder: encoder_supplier(encoding),
            slots: Vec::new(),
        }
    }

    /// Encodes `message` once and sends it via the transport, which fans it
    /// out to every currently connected client.
    pub fn send_to_all(&mut self, message: &T) -> Result<(), TransportError> {
        let mut bytes = Vec::new();
        self.encoder.encode(message, &mut bytes);
        for b in bytes {
            self.transport.send(b)?;
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
                    let mut slot = ProtocolSlot::new(tag, self.encoding, self.decoder_supplier);
                    slot.initial_dump_sent = true;
                    self.slots.push(slot);
                    self.send_all_to_all(init_state)?;
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
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    // --- Mock transport: scripted events in, captured bytes out ---

    struct MockTransport {
        events: VecDeque<TransportEvent>,
        sent: Arc<Mutex<Vec<u8>>>,
    }

    impl MockTransport {
        /// Returns the transport plus a handle to its captured output, since
        /// the transport itself gets moved into the `ProtocolManager`.
        fn new(events: Vec<TransportEvent>) -> (Self, Arc<Mutex<Vec<u8>>>) {
            let sent = Arc::new(Mutex::new(Vec::new()));
            (Self { events: events.into(), sent: Arc::clone(&sent) }, sent)
        }
    }

    impl Transport for MockTransport {
        fn try_recv(&mut self) -> Option<u8> {
            loop {
                match self.events.pop_front()? {
                    TransportEvent::Data(_, b) => return Some(b),
                    _ => continue,
                }
            }
        }

        fn send(&mut self, byte: u8) -> Result<(), TransportError> {
            self.sent.lock().unwrap().push(byte);
            Ok(())
        }

        fn is_connected(&self) -> bool { true }

        fn try_recv_tagged(&mut self) -> Option<TransportEvent> {
            self.events.pop_front()
        }

        fn shutdown(&mut self) {}
    }

    // --- Toy codec: a "message" is exactly two bytes ---

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TwoByteMsg(u8, u8);

    struct TwoByteEncoder;
    impl ProtocolMessageEncoder<TwoByteMsg> for TwoByteEncoder {
        fn encode(&mut self, message: &TwoByteMsg, out: &mut Vec<u8>) {
            out.push(message.0);
            out.push(message.1);
        }
    }

    #[derive(Default)]
    struct TwoByteDecoder {
        first: Option<u8>,
    }
    impl ProtocolMessageDecoder<TwoByteMsg> for TwoByteDecoder {
        fn feed(&mut self, b: u8) -> Option<TwoByteMsg> {
            match self.first.take() {
                None => { self.first = Some(b); None }
                Some(first) => Some(TwoByteMsg(first, b)),
            }
        }
    }

    fn two_byte_encoder(_encoding: ProtocolMessageEncoding)
                        -> Box<dyn ProtocolMessageEncoder<TwoByteMsg>> {
        Box::new(TwoByteEncoder)
    }

    fn two_byte_decoder(_encoding: ProtocolMessageEncoding)
                        -> Box<dyn ProtocolMessageDecoder<TwoByteMsg>> {
        Box::new(TwoByteDecoder::default())
    }

    fn manager(events: Vec<TransportEvent>) -> (ProtocolManager<TwoByteMsg>, Arc<Mutex<Vec<u8>>>) {
        let (transport, sent) = MockTransport::new(events);
        (ProtocolManager::new(ProtocolMessageEncoding::Binary, Box::new(transport),
                              two_byte_encoder, two_byte_decoder), sent)
    }

    // --- Tests ---

    #[test]
    fn connected_sends_initial_dump() {
        let (mut mgr, sent) = manager(vec![TransportEvent::Connected(1)]);
        let init_state = [TwoByteMsg(0xAA, 0xBB)];

        assert_eq!(mgr.poll_transport(&init_state).unwrap(), None);
        assert_eq!(*sent.lock().unwrap(), vec![0xAA, 0xBB]);
    }

    #[test]
    fn connected_sends_dump_once_per_new_connection_not_per_slot_count() {
        // Two clients connecting in turn: each Connected event should
        // trigger exactly one dump broadcast (relying on the transport's
        // own fan-out to reach everyone) — not one dump per currently
        // known slot.
        let (mut mgr, sent) = manager(vec![
            TransportEvent::Connected(1),
            TransportEvent::Connected(2),
        ]);
        let init_state = [TwoByteMsg(0xAA, 0xBB)];

        assert_eq!(mgr.poll_transport(&init_state).unwrap(), None);
        // Two Connected events => two broadcasts of the two-byte dump,
        // not four bytes duplicated per slot or eight bytes (2 events * 2 slots).
        assert_eq!(*sent.lock().unwrap(), vec![0xAA, 0xBB, 0xAA, 0xBB]);
    }

    #[test]
    fn data_is_demultiplexed_per_tag() {
        let (mut mgr, _sent) = manager(vec![
            TransportEvent::Connected(1),
            TransportEvent::Connected(2),
            TransportEvent::Data(1, 0x01),
            TransportEvent::Data(2, 0x10),
            TransportEvent::Data(1, 0x02), // completes tag 1's message
            TransportEvent::Data(2, 0x20), // completes tag 2's message
        ]);

        // First call stops at the first completed message (tag 1's).
        assert_eq!(mgr.poll_transport(&[]).unwrap(), Some(TwoByteMsg(0x01, 0x02)));
        // Second call drains the rest and completes tag 2's message.
        assert_eq!(mgr.poll_transport(&[]).unwrap(), Some(TwoByteMsg(0x10, 0x20)));
    }

    #[test]
    #[should_panic(expected = "Data event for tag with no prior Connected event")]
    fn data_without_prior_connected_panics() {
        let (mut mgr, _sent) = manager(vec![TransportEvent::Data(1, 0x01)]);
        let _ = mgr.poll_transport(&[]);
    }

    #[test]
    fn disconnected_discards_slot_and_partial_state() {
        let (mut mgr, _sent) = manager(vec![
            TransportEvent::Connected(1),
            TransportEvent::Data(1, 0x01),   // partial message, never completed
            TransportEvent::Disconnected(1),
            TransportEvent::Connected(1),    // tag reused by a new connection
            TransportEvent::Data(1, 0x02),
            TransportEvent::Data(1, 0x03),
        ]);

        // The stray 0x01 from the old session must not leak into the new
        // session's decoder — if it did, this would incorrectly complete
        // as (0x01, 0x02) instead of (0x02, 0x03).
        assert_eq!(mgr.poll_transport(&[]).unwrap(), Some(TwoByteMsg(0x02, 0x03)));
    }

    #[test]
    fn reconnect_without_disconnect_still_replaces_stale_slot() {
        // Simulates a wrapped/reused tag arriving via Connected before this
        // transport ever emitted a matching Disconnected for the old session.
        let (mut mgr, _sent) = manager(vec![
            TransportEvent::Connected(1),
            TransportEvent::Data(1, 0x01),
            TransportEvent::Connected(1),
            TransportEvent::Data(1, 0x02),
            TransportEvent::Data(1, 0x03),
        ]);

        assert_eq!(mgr.poll_transport(&[]).unwrap(), Some(TwoByteMsg(0x02, 0x03)));
    }

    #[test]
    fn send_to_all_encodes_and_sends_exactly_once_regardless_of_slot_count() {
        // send_to_all no longer iterates slots at all — it encodes once and
        // relies on the transport's own fan-out. Slot count (0, 1, or many)
        // must not affect how many times the message is encoded/sent.
        let (mut mgr, sent) = manager(vec![
            TransportEvent::Connected(1),
            TransportEvent::Connected(2),
            TransportEvent::Connected(3),
        ]);
        mgr.poll_transport(&[]).unwrap(); // establish 3 slots, dumps go out (ignored below)

        sent.lock().unwrap().clear();
        mgr.send_to_all(&TwoByteMsg(0x99, 0x77)).unwrap();

        assert_eq!(*sent.lock().unwrap(), vec![0x99, 0x77]);
    }

    #[test]
    fn send_to_all_with_no_slots_still_sends() {
        // Sending doesn't depend on slot bookkeeping at all — even with zero
        // known connections, the manager still encodes and forwards to the
        // transport (which is free to drop it if nothing is connected).
        let (mut mgr, sent) = manager(vec![]);

        mgr.send_to_all(&TwoByteMsg(0x11, 0x22)).unwrap();

        assert_eq!(*sent.lock().unwrap(), vec![0x11, 0x22]);
    }

    #[test]
    fn send_all_to_all_sends_each_message_once_in_order() {
        let (mut mgr, sent) = manager(vec![]);
        let messages = [TwoByteMsg(0x01, 0x02), TwoByteMsg(0x03, 0x04)];

        mgr.send_all_to_all(&messages).unwrap();

        assert_eq!(*sent.lock().unwrap(), vec![0x01, 0x02, 0x03, 0x04]);
    }
}