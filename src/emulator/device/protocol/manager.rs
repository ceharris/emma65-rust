use crate::emulator::device::protocol::ProtocolMessageDecoder;
use crate::emulator::device::protocol::{DecoderSupplier, EncoderSupplier, ProtocolMessageEncoder, ProtocolMessageEncoding};
use crate::emulator::{ChannelRelay, Transport, TransportEvent};

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
/// I/O device. Subsequently, on each call to the [`poll_transport`](ProtocolManager::poll_transport) method, it
/// checks for a valid message from any connected peripheral. Messages can be
/// delivered to peripherals using either the [`send_to_all`](ProtocolManager::send_to_all) or
/// [`send_all_to_all`](ProtocolManager::send_all_to_all) methods.
pub(crate) struct ProtocolManager<T> {
    encoding: ProtocolMessageEncoding,
    transport: Box<dyn Transport>,
    /// Paired with `transport`; drained once per [`poll_transport`](Self::poll_transport)
    /// call. Unlike the P2P devices' `TransportRelay`, this is always the tagged variant directly
    /// — demultiplexing per-client requires the tag `TransportRelay::drain_bytes_into`
    /// would otherwise discard.
    relay: ChannelRelay<TransportEvent>,
    decoder_supplier: DecoderSupplier<T>,
    encoder: Box<dyn ProtocolMessageEncoder<T>>,
    slots: Vec<ProtocolSlot<T>>,
}

impl<T> ProtocolManager<T> {
    pub fn new(encoding: ProtocolMessageEncoding,
               transport: Box<dyn Transport>,
               relay: ChannelRelay<TransportEvent>,
               encoder_supplier: EncoderSupplier<T>,
               decoder_supplier: DecoderSupplier<T>) -> Self {
        Self {
            encoding,
            transport,
            relay,
            decoder_supplier,
            encoder: encoder_supplier(encoding),
            slots: Vec::new(),
        }
    }

    /// Encodes `message` once and sends it via the transport, which fans it
    /// out to every currently connected client. Never blocks and never
    /// errors — the transport itself owns drop-counting and error reporting
    /// via its `TransportReporter`.
    pub fn send_to_all(&mut self, message: &T) {
        let mut bytes = Vec::new();
        self.encoder.encode(message, &mut bytes);
        for b in bytes {
            self.transport.send(b);
        }
    }

    pub fn send_all_to_all(&mut self, messages: &[T]) {
        for message in messages.iter() {
            self.send_to_all(message);
        }
    }

    /// Forwards to the owned transport's [`shutdown`](Transport::shutdown).
    pub fn shutdown(&mut self) {
        self.transport.shutdown();
    }

    /// Returns `true` if the relay has at least one event buffered.
    ///
    /// `Via6522`/`Mc6840` call this before building the (heap-allocating)
    /// snapshot they pass to [`poll_transport`](Self::poll_transport) as
    /// `init_state` — that snapshot is only ever consumed when a
    /// `TransportEvent::Connected` shows up in this call's drained batch,
    /// which never happens while the relay is empty (the overwhelmingly
    /// common case with no client attached). Skipping straight to "nothing
    /// to do" in that case avoids paying for a state snapshot on every
    /// single `tick()` — previously unconditional, and significant at an
    /// unthrottled clock speed's tick rate.
    pub fn has_pending(&self) -> bool {
        !self.relay.is_empty()
    }

    /// Returns `true` if at least one client is currently connected.
    ///
    /// `Mc6840::tick()` calls this before encoding and broadcasting a live
    /// timer-state update — with zero clients connected, that work (a
    /// `Vec<PtmProtocolMessage>` allocation, encoding, and a ring push per
    /// encoded byte) has no observer and would otherwise still run on every
    /// tick in which any timer's clock/gate/output state changes, which is
    /// most of them while a timer is actually running.
    pub fn has_clients(&self) -> bool {
        !self.slots.is_empty()
    }

    /// Drains every event currently buffered in the relay, dispatching
    /// connect/data/disconnect handling for each, and returns every
    /// newly-decoded message in arrival order.
    ///
    /// This replaces the old "one message per call" contract: `ChannelRelay::drain_into`
    /// has no partial-drain mode, so a caller can no longer be throttled to
    /// one message per call the way it could when polling the transport
    /// directly — nothing depended on that throttling, so callers now take
    /// whatever's decoded in one pass.
    pub fn poll_transport(&mut self, init_state: &[T]) -> Vec<T> {
        let mut events = Vec::new();
        self.relay.drain_into(|event| events.push(event));

        let mut decoded = Vec::new();
        for event in events {
            match event {
                TransportEvent::Connected(tag) => {
                    // Drop any stale slot for this tag before creating a fresh one —
                    // guards against the (rare) case of a wrapped/reassigned tag
                    // aliasing onto a still-referenced old connection.
                    self.slots.retain(|s| s.client_tag != tag);
                    let mut slot = ProtocolSlot::new(tag, self.encoding, self.decoder_supplier);
                    slot.initial_dump_sent = true;
                    self.slots.push(slot);
                    self.send_all_to_all(init_state);
                }
                TransportEvent::Data(tag, byte) => {
                    let slot = Self::find_slot(tag, &mut self.slots)
                        .expect("Data event for tag with no prior Connected event");
                    if let Some(message) = slot.feed(byte) {
                        decoded.push(message);
                    }
                }
                TransportEvent::Disconnected(tag) => {
                    self.slots.retain(|s| s.client_tag != tag);
                }
            }
        }
        decoded
    }

    fn find_slot(tag: u8, slots: &mut [ProtocolSlot<T>]) -> Option<&mut ProtocolSlot<T>> {
        slots.iter_mut().find(|s| s.client_tag == tag)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // --- Mock transport: captures sent bytes only. Inbound events go
    // straight into a hand-fed ChannelRelay<TransportEvent> instead (see
    // `manager` below), since ProtocolManager no longer polls the
    // transport itself.

    struct MockTransport {
        sent: Arc<Mutex<Vec<u8>>>,
    }

    impl MockTransport {
        /// Returns the transport plus a handle to its captured output, since
        /// the transport itself gets moved into the `ProtocolManager`.
        fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
            let sent = Arc::new(Mutex::new(Vec::new()));
            (Self { sent: Arc::clone(&sent) }, sent)
        }
    }

    impl Transport for MockTransport {
        fn try_recv(&mut self) -> Option<u8> { None }

        fn send(&mut self, byte: u8) {
            self.sent.lock().unwrap().push(byte);
        }

        fn is_connected(&self) -> bool { true }

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

    /// Feeds `events` into a hand-fed `ChannelRelay<TransportEvent>` (built
    /// from a plain `crossbeam_channel`) and sleeps briefly so the relay
    /// thread has drained them all before returning — `ProtocolManager` no
    /// longer polls the transport directly, so tests can't rely on a
    /// synchronous `VecDeque::pop_front` the way they used to.
    fn manager(events: Vec<TransportEvent>) -> (ProtocolManager<TwoByteMsg>, Arc<Mutex<Vec<u8>>>) {
        let (transport, sent) = MockTransport::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        for event in events {
            tx.send(event).unwrap();
        }
        let relay = ChannelRelay::spawn(rx, 256);
        std::thread::sleep(std::time::Duration::from_millis(20));
        (ProtocolManager::new(ProtocolMessageEncoding::Binary, Box::new(transport), relay,
                              two_byte_encoder, two_byte_decoder), sent)
    }

    // --- Tests ---

    #[test]
    fn connected_sends_initial_dump() {
        let (mut mgr, sent) = manager(vec![TransportEvent::Connected(1)]);
        let init_state = [TwoByteMsg(0xAA, 0xBB)];

        assert_eq!(mgr.poll_transport(&init_state), vec![]);
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

        assert_eq!(mgr.poll_transport(&init_state), vec![]);
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

        // drain_into has no partial-drain mode, so both completed messages
        // come back from a single call, in arrival order.
        assert_eq!(mgr.poll_transport(&[]), vec![TwoByteMsg(0x01, 0x02), TwoByteMsg(0x10, 0x20)]);
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
        assert_eq!(mgr.poll_transport(&[]), vec![TwoByteMsg(0x02, 0x03)]);
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

        assert_eq!(mgr.poll_transport(&[]), vec![TwoByteMsg(0x02, 0x03)]);
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
        mgr.poll_transport(&[]); // establish 3 slots, dumps go out (ignored below)

        sent.lock().unwrap().clear();
        mgr.send_to_all(&TwoByteMsg(0x99, 0x77));

        assert_eq!(*sent.lock().unwrap(), vec![0x99, 0x77]);
    }

    #[test]
    fn send_to_all_with_no_slots_still_sends() {
        // Sending doesn't depend on slot bookkeeping at all — even with zero
        // known connections, the manager still encodes and forwards to the
        // transport (which is free to drop it if nothing is connected).
        let (mut mgr, sent) = manager(vec![]);

        mgr.send_to_all(&TwoByteMsg(0x11, 0x22));

        assert_eq!(*sent.lock().unwrap(), vec![0x11, 0x22]);
    }

    #[test]
    fn send_all_to_all_sends_each_message_once_in_order() {
        let (mut mgr, sent) = manager(vec![]);
        let messages = [TwoByteMsg(0x01, 0x02), TwoByteMsg(0x03, 0x04)];

        mgr.send_all_to_all(&messages);

        assert_eq!(*sent.lock().unwrap(), vec![0x01, 0x02, 0x03, 0x04]);
    }
}