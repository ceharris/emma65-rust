//! Protocol codec for the MC6840 peripheral interface.
//!

use crate::emulator::device::protocol_manager::{ProtocolMessageDecoder, ProtocolMessageEncoder};
use crate::emulator::ProtocolMessageEncoding;

const BINARY_TYPE_MASK: u8 = 0b11110000;
const BINARY_CLOCK_EDGE: u8   = 0b10000000;
const BINARY_GATE_EDGE: u8    = 0b10010000;
const BINARY_CLOCK_STATE: u8  = 0b10100000;
const BINARY_GATE_STATE: u8   = 0b10110000;
const BINARY_OUTPUT_STATE: u8 = 0b11000000;

const BINARY_POLARITY_BIT: u8 = 0b00001000;

/// A decoded PTM protocol message
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtmProtocolMessage {
    /// One or more clock inputs have changed
    ClockEdge {
        /// For each clock (C1, C2, C3), indicates whether the clock changed state
        clocks: [bool; 3],
        /// Indicates whether transition for indicated clocks was negative or positive
        positive: bool,
    },
    /// One or more gate inputs have changed
    GateEdge {
        /// For each gate (G1, G2, G3), indicates whether the gate changed state
        gates: [bool; 3],
        /// Indicates whether transition for indicated gates was negative or positive
        positive: bool,
    },
    /// Conveys the current state of all clock inputs
    ClockState {
        /// Current state of each clock input
        clocks: [bool; 3],
    },
    /// Conveys the current state of all gate inputs
    GateState {
        /// Current state of each gate input
        gates: [bool; 3],
    },
    /// Conveys the current state of all timer outputs
    OutputState {
        /// Current state of each timer output
        outputs: [bool; 3],
    },
}

/// Creates a new encoder/decoder pair.
pub fn new_codecs(encoding: ProtocolMessageEncoding)
                  -> (Box<dyn ProtocolMessageEncoder<PtmProtocolMessage>>,
                      Box<dyn ProtocolMessageDecoder<PtmProtocolMessage>>) {
    match encoding {
        ProtocolMessageEncoding::Ascii =>
            (Box::new(PtmAsciiProtocolEncoder::new()), Box::new(PtmAsciiProtocolDecoder::new())),
        ProtocolMessageEncoding::Binary =>
            (Box::new(PtmBinaryProtocolEncoder::new()), Box::new(PtmBinaryProtocolDecoder::new())),
    }
}

/// Encodes [`PtmProtocolMessage`] values into ASCII format for transmission.
///
/// A space is inserted between messages for human readability, and a
/// carriage return plus line feed pair is output each time the length of
/// the current output line exceeds 72 bytes.
pub struct PtmAsciiProtocolEncoder {
    line_length: u8,
}

impl Default for PtmAsciiProtocolEncoder {
    fn default() -> Self { Self::new() }
}

impl ProtocolMessageEncoder<PtmProtocolMessage> for PtmAsciiProtocolEncoder {

    /// Encodes the given message at the tail of the given output vector.
    fn encode(&mut self, message: &PtmProtocolMessage, out: &mut Vec<u8>) {
        match message {
            PtmProtocolMessage::ClockEdge { clocks, positive } => {
                for (i, clock) in clocks.iter().enumerate() {
                    if *clock {
                        self.encode_ascii_prefix(b'C', out);
                        self.encode_ascii_timer_id(i as u8, out);
                        self.encode_ascii_state(*positive, out);
                        self.encode_ascii_space(out);
                    }
                }
            },
            PtmProtocolMessage::GateEdge { gates, positive } => {
                for (i, gate) in gates.iter().enumerate() {
                    if *gate {
                        self.encode_ascii_prefix(b'G', out);
                        self.encode_ascii_timer_id(i as u8, out);
                        self.encode_ascii_state(*positive, out);
                        self.encode_ascii_space(out);
                    }
                }
            },
            PtmProtocolMessage::ClockState { clocks } => {
                self.encode_ascii_prefix(b'T', out);
                for clock in clocks.iter() {
                    self.encode_ascii_state(*clock, out);
                }
                self.encode_ascii_space(out);
            },
            PtmProtocolMessage::GateState { gates } => {
                self.encode_ascii_prefix(b'U', out);
                for gate in gates.iter() {
                    self.encode_ascii_state(*gate, out);
                }
                self.encode_ascii_space(out);
            },
            PtmProtocolMessage::OutputState { outputs } => {
                self.encode_ascii_prefix(b'V', out);
                for output in outputs.iter() {
                    self.encode_ascii_state(*output, out);
                }
                self.encode_ascii_space(out);
            },
        }
    }

}

impl PtmAsciiProtocolEncoder {

    /// Creates a new encoder that uses ASCII mode.
    pub fn new() -> Self {
        PtmAsciiProtocolEncoder {
            line_length: 0,
        }
    }

    fn encode_ascii_prefix(&mut self, prefix: u8, out: &mut Vec<u8>) {
        if self.line_length >= 72 {
            self.encode_ascii_newline(out);
        }
        self.encode_ascii_char(prefix, out);
    }

    fn encode_ascii_timer_id(&mut self, timer_id: u8, out: &mut Vec<u8>) {
        self.encode_ascii_char(timer_id + b'1', out);
    }

    fn encode_ascii_state(&mut self, state: bool, out: &mut Vec<u8>) {
        self.encode_ascii_char(if state { b'1' } else { b'0' }, out);
    }

    fn encode_ascii_space(&mut self, out: &mut Vec<u8>) {
        self.encode_ascii_char(b' ', out);
    }

    fn encode_ascii_char(&mut self, c: u8, out: &mut Vec<u8>) {
        out.push(c);
        self.line_length += 1;
    }

    fn encode_ascii_newline(&mut self, out: &mut Vec<u8>) {
        out.push(b'\r');
        out.push(b'\n');
        self.line_length = 0;
    }

}

/// Encodes [`PtmProtocolMessage`] values into binary format for transmission.
pub struct PtmBinaryProtocolEncoder;

impl Default for PtmBinaryProtocolEncoder {
    fn default() -> Self { Self::new() }
}

impl ProtocolMessageEncoder<PtmProtocolMessage> for PtmBinaryProtocolEncoder {

    /// Encodes the given message at the tail of the given output vector.
    fn encode(&mut self, message: &PtmProtocolMessage, out: &mut Vec<u8>) {
        match message {
            PtmProtocolMessage::ClockEdge { clocks, positive } => {
                self.encode_binary_edges(BINARY_CLOCK_EDGE, *positive, *clocks, out);
            },
            PtmProtocolMessage::GateEdge { gates, positive } => {
                self.encode_binary_edges(BINARY_GATE_EDGE, *positive, *gates, out);
            },
            PtmProtocolMessage::ClockState { clocks } => {
                self.encode_binary_states(BINARY_CLOCK_STATE, *clocks, out);
            },
            PtmProtocolMessage::GateState { gates } => {
                self.encode_binary_states(BINARY_GATE_STATE, *gates, out);
            },
            PtmProtocolMessage::OutputState { outputs } => {
                self.encode_binary_states(BINARY_OUTPUT_STATE, *outputs, out);
            },
        }
    }

}

impl PtmBinaryProtocolEncoder {

    /// Creates a new encoder that uses ASCII mode.
    pub fn new() -> Self {
        PtmBinaryProtocolEncoder {}
    }

    fn encode_binary_edges(&self, mut message: u8, positive: bool, edges: [bool; 3], out: &mut Vec<u8>) {
        if positive {
            message |= BINARY_POLARITY_BIT;
        }
        for (i, edge) in edges.iter().enumerate() {
            if *edge {
                message |= 1 << i;
            }
        }
        out.push(message);
    }

    fn encode_binary_states(&self, mut message: u8, states: [bool; 3], out: &mut Vec<u8>) {
        for (i, state) in states.iter().enumerate() {
            if *state {
                message |= 1 << i;
            }
        }
        out.push(message);
    }

}

#[derive(Debug, Clone, Copy)]
enum AsciiDecoderState {
    Idle,
    AsciiClockEdgeTimer,
    AsciiClockEdgePolarity { t: u8 },
    AsciiGateEdgeTimer,
    AsciiGateEdgePolarity { t: u8 },
    AsciiClockStatusT1,
    AsciiClockStatusT2 { t1: u8 },
    AsciiClockStatusT3 { t1: u8, t2: u8 },
    AsciiGateStatusT1,
    AsciiGateStatusT2 { t1: u8 },
    AsciiGateStatusT3 { t1: u8, t2: u8 },
    AsciiOutputStatusT1,
    AsciiOutputStatusT2 { t1: u8 },
    AsciiOutputStatusT3 { t1: u8, t2: u8 },
}

/// Decodes an ASCII encoded byte stream into [`PtmProtocolMessage`] values.
///
/// Invalid data is silently ignored per the protocol specification.
pub struct PtmAsciiProtocolDecoder {
    state: AsciiDecoderState,
    next_state: AsciiDecoderState,
}

impl Default for PtmAsciiProtocolDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolMessageDecoder<PtmProtocolMessage> for PtmAsciiProtocolDecoder {

    /// Feeds a single byte into the decoder.
    ///
    /// Returns `Some(message)` when a complete, valid message has been decoded, or `None`
    /// if more bytes are needed or the byte was ignored.
    fn feed(&mut self, b: u8) -> Option<PtmProtocolMessage> {
        let result = self.feed_ascii(b);
        self.state = self.next_state;
        result
    }

}

impl PtmAsciiProtocolDecoder {

    pub fn new() -> Self {
        PtmAsciiProtocolDecoder {
            state: AsciiDecoderState::Idle,
            next_state: AsciiDecoderState:: Idle,
        }
    }

    fn feed_ascii(&mut self, b: u8) -> Option<PtmProtocolMessage> {
        self.next_state = AsciiDecoderState::Idle;
        match &self.state {
            AsciiDecoderState::Idle => {
                match b.to_ascii_uppercase() {
                    b'C' => {
                        self.next_state = AsciiDecoderState::AsciiClockEdgeTimer;
                        None
                    }
                    b'G' => {
                        self.next_state = AsciiDecoderState::AsciiGateEdgeTimer;
                        None
                    }
                    b'T' => {
                        self.next_state = AsciiDecoderState::AsciiClockStatusT1;
                        None
                    }
                    b'U' => {
                        self.next_state = AsciiDecoderState::AsciiGateStatusT1;
                        None
                    }
                    b'V' => {
                        self.next_state = AsciiDecoderState::AsciiOutputStatusT1;
                        None
                    }
                    _ => None
                }
            },
            AsciiDecoderState::AsciiClockEdgeTimer => {
                match b {
                    b'1'..=b'3' => {
                        self.next_state = AsciiDecoderState::AsciiClockEdgePolarity { t: b - b'0' };
                        None
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiClockEdgePolarity { t } => {
                match b {
                    b'0'..=b'1' => {
                        Some(PtmProtocolMessage::ClockEdge {
                            clocks: [*t == 1, *t == 2, *t == 3],
                            positive: b - b'0' != 0,
                        })
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiGateEdgeTimer => {
                match b {
                    b'1'..=b'3' => {
                        self.next_state = AsciiDecoderState::AsciiGateEdgePolarity { t: b - b'0' };
                        None
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiGateEdgePolarity { t } => {
                match b {
                    b'0'..=b'1' => {
                        Some(PtmProtocolMessage::GateEdge {
                            gates: [*t == 1, *t == 2, *t == 3],
                            positive: b - b'0' != 0,
                        })
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiClockStatusT1 => {
                match b {
                    b'0'..=b'1' => {
                        self.next_state = AsciiDecoderState::AsciiClockStatusT2 { t1: b - b'0' };
                        None
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiClockStatusT2 { t1} => {
                match b {
                    b'0'..=b'1' => {
                        self.next_state = AsciiDecoderState::AsciiClockStatusT3 { t1: *t1, t2: b - b'0' };
                        None
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiClockStatusT3 { t1, t2 } => {
                match b {
                    b'0'..=b'1' => {
                        Some(PtmProtocolMessage::ClockState {
                            clocks: [*t1 != 0, *t2 != 0, b - b'0' != 0]
                        })
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiGateStatusT1 => {
                match b {
                    b'0'..=b'1' => {
                        self.next_state = AsciiDecoderState::AsciiGateStatusT2 { t1: b - b'0' };
                        None
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiGateStatusT2 { t1} => {
                match b {
                    b'0'..=b'1' => {
                        self.next_state = AsciiDecoderState::AsciiGateStatusT3 { t1: *t1, t2: b - b'0' };
                        None
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiGateStatusT3 { t1, t2 } => {
                match b {
                    b'0'..=b'1' => {
                        Some(PtmProtocolMessage::GateState {
                            gates: [*t1 != 0, *t2 != 0, b - b'0' != 0]
                        })
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiOutputStatusT1 => {
                match b {
                    b'0'..=b'1' => {
                        self.next_state = AsciiDecoderState::AsciiOutputStatusT2 { t1: b - b'0' };
                        None
                    }
                    _ => None
                }
            }
            AsciiDecoderState::AsciiOutputStatusT2 { t1} => {
                match b {
                    b'0'..=b'1' => {
                        self.next_state = AsciiDecoderState::AsciiOutputStatusT3 { t1: *t1, t2: b - b'0' };
                        None
                    }
                    _ => None
                }

            }
            AsciiDecoderState::AsciiOutputStatusT3 { t1, t2 } => {
                match b {
                    b'0'..=b'1' => {
                        Some(PtmProtocolMessage::OutputState {
                            outputs: [*t1 != 0, *t2 != 0, b - b'0' != 0]
                        })
                    }
                    _ => None
                }

            }
        }
    }

}

/// Decodes a binary encoded byte stream into [`PtmProtocolMessage`] values.
///
/// Invalid data is silently ignored per the protocol specification.
pub struct PtmBinaryProtocolDecoder;

impl Default for PtmBinaryProtocolDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolMessageDecoder<PtmProtocolMessage> for PtmBinaryProtocolDecoder {

    /// Feeds a single byte into the decoder.
    ///
    /// Returns `Some(message)` when a complete, valid message has been decoded, or `None`
    /// if more bytes are needed or the byte was ignored.
    fn feed(&mut self, b: u8) -> Option<PtmProtocolMessage> {
        self.feed_binary(b)
    }

}

impl PtmBinaryProtocolDecoder {

    pub fn new() -> Self {
        PtmBinaryProtocolDecoder {}
    }

    fn feed_binary(&self, b: u8) -> Option<PtmProtocolMessage> {
        let message_type = b & BINARY_TYPE_MASK;
        if message_type == BINARY_CLOCK_EDGE {
            Some(self.decode_binary_clock_edge(b))
        } else if message_type == BINARY_GATE_EDGE {
            Some(self.decode_binary_gate_edge(b))
        } else if message_type == BINARY_CLOCK_STATE {
            Some(self.decode_binary_clock_state(b))
        } else if message_type == BINARY_GATE_STATE {
            Some(self.decode_binary_gate_state(b))
        } else if message_type == BINARY_OUTPUT_STATE {
            Some(self.decode_binary_output_state(b))
        } else {
            None
        }
    }

    fn decode_binary_clock_edge(&self, b: u8) -> PtmProtocolMessage {
        PtmProtocolMessage::ClockEdge {
            clocks: self.decode_binary_edges(b),
            positive: b & BINARY_POLARITY_BIT != 0,
        }
    }

    fn decode_binary_gate_edge(&self, b: u8) -> PtmProtocolMessage {
        PtmProtocolMessage::GateEdge {
            gates: self.decode_binary_edges(b),
            positive: b & BINARY_POLARITY_BIT != 0,
        }
    }

    fn decode_binary_clock_state(&self, b: u8) -> PtmProtocolMessage {
        PtmProtocolMessage::ClockState {
            clocks: self.decode_binary_states(b),
        }
    }

    fn decode_binary_gate_state(&self, b: u8) -> PtmProtocolMessage {
        PtmProtocolMessage::GateState {
            gates: self.decode_binary_states(b),
        }
    }

    fn decode_binary_output_state(&self, b: u8) -> PtmProtocolMessage {
        PtmProtocolMessage::OutputState {
            outputs: self.decode_binary_states(b),
        }
    }

    fn decode_binary_edges(&self, b: u8) -> [bool; 3]{
        let mut edges: [bool; 3] = [false; 3];
        for (i, edge) in edges.iter_mut().enumerate() {
            *edge = b & (1 << i) != 0;
        }
        edges
    }

    fn decode_binary_states(&self, b: u8) -> [bool; 3] {
        let mut states: [bool; 3] = [false; 3];
        for (i, state) in states.iter_mut().enumerate() {
            *state = b & (1 << i) != 0;
        }
        states
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_ascii_clock_edges_negative() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockEdge {
            clocks: [true, true, true],
            positive: false
        }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "C10 C20 C30 ");
    }

    #[test]
    fn encode_ascii_clock_edges_positive() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockEdge {
            clocks: [true, true, true],
            positive: true
        }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "C11 C21 C31 ");
    }

    #[test]
    fn encode_ascii_gate_edges_negative() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateEdge {
            gates: [true, true, true], positive: false }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "G10 G20 G30 ");
    }

    #[test]
    fn encode_ascii_gate_edges_positive() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateEdge {
            gates: [true, true, true], positive: true }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "G11 G21 G31 ");
    }

    #[test]
    fn encode_ascii_clock_state_t1() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockState {
            clocks: [true, false, false] }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "T100 ");
    }

    #[test]
    fn encode_ascii_clock_state_t2() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockState {
            clocks: [false, true, false] }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "T010 ");
    }

    #[test]
    fn encode_ascii_clock_state_t3() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockState {
            clocks: [false, false, true]
        }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "T001 ");
    }

    #[test]
    fn encode_ascii_gate_state_t1() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateState {
            gates: [true, false, false]
        }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "U100 ");
    }

    #[test]
    fn encode_ascii_gate_state_t2() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateState {
            gates: [false, true, false]
        }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "U010 ");
    }

    #[test]
    fn encode_ascii_gate_state_t3() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateState {
            gates: [false, false, true]
        }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "U001 ");
    }

    #[test]
    fn encode_ascii_output_state_t1() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::OutputState {
            outputs: [true, false, false]
        }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "V100 ");
    }

    #[test]
    fn encode_ascii_output_state_t2() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::OutputState {
            outputs: [false, true, false]
        }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "V010 ");
    }

    #[test]
    fn encode_ascii_output_state_t3() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::OutputState {
            outputs: [false, false, true]
        }, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "V001 ");
    }

    #[test]
    fn encode_ascii_inserts_newline() {
        let mut encoder = PtmAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        let mut expected: String = String::new();
        for _ in 0..(72 / 4) {
            encoder.encode(&PtmProtocolMessage::ClockEdge {
                clocks: [true, false, false],
                positive: false
            }, &mut out);
            expected.push_str("C10 ");
        }
        assert_eq!(out, expected.as_bytes());
        encoder.encode(&PtmProtocolMessage::ClockEdge {
            clocks: [true, false, false],
            positive: false
        }, &mut out);
        expected.push_str("\r\nC10 ");
        assert_eq!(out, expected.as_bytes());
    }

    #[test]
    fn encode_binary_clock_edge_t1() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockEdge {
            clocks: [true, false, false],
            positive: false
        }, &mut out);
        assert_eq!(out[0], 0b10000001);
    }

    #[test]
    fn encode_binary_clock_edge_t2() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockEdge {
            clocks: [false, true, false],
            positive: false
        }, &mut out);
        assert_eq!(out[0], 0b10000010);
    }

    #[test]
    fn encode_binary_clock_edge_t3() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockEdge {
            clocks: [false, false, true],
            positive: false
        }, &mut out);
        assert_eq!(out[0], 0b10000100);
    }

    #[test]
    fn encode_binary_clock_edge_positive() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockEdge {
            clocks: [true, false, false],
            positive: true
        }, &mut out);
        assert_eq!(out[0], 0b10001001);
    }

    #[test]
    fn encode_binary_gate_edge_t1() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateEdge {
            gates: [true, false, false],
            positive: false
        }, &mut out);
        assert_eq!(out[0], 0b10010001);
    }

    #[test]
    fn encode_binary_gate_edge_t2() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateEdge {
            gates: [false, true, false],
            positive: false
        }, &mut out);
        assert_eq!(out[0], 0b10010010);
    }

    #[test]
    fn encode_binary_gate_edge_t3() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateEdge {
            gates: [false, false, true],
            positive: false
        }, &mut out);
        assert_eq!(out[0], 0b10010100);
    }

    #[test]
    fn encode_binary_gate_edge_positive() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateEdge {
            gates: [true, false, false],
            positive: true
        }, &mut out);
        assert_eq!(out[0], 0b10011001);
    }

    #[test]
    fn encode_binary_clock_state_t1() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockState {
            clocks: [true, false, false]
        }, &mut out);
        assert_eq!(out[0], 0b10100001);
    }

    #[test]
    fn encode_binary_clock_state_t2() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockState {
            clocks: [false, true, false]
        }, &mut out);
        assert_eq!(out[0], 0b10100010);
    }

    #[test]
    fn encode_binary_clock_state_t3() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::ClockState {
            clocks: [false, false, true]
        }, &mut out);
        assert_eq!(out[0], 0b10100100);
    }

    #[test]
    fn encode_binary_gate_state_t1() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateState {
            gates: [true, false, false]
        }, &mut out);
        assert_eq!(out[0], 0b10110001);
    }

    #[test]
    fn encode_binary_gate_state_t2() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateState {
            gates: [false, true, false]
        }, &mut out);
        assert_eq!(out[0], 0b10110010);
    }

    #[test]
    fn encode_binary_gate_state_t3() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::GateState {
            gates: [false, false, true]
        }, &mut out);
        assert_eq!(out[0], 0b10110100);
    }

    #[test]
    fn encode_binary_output_state_t1() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::OutputState {
            outputs: [true, false, false]
        }, &mut out);
        assert_eq!(out[0], 0b11000001);
    }

    #[test]
    fn encode_binary_output_state_t2() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::OutputState {
            outputs: [false, true, false]
        }, &mut out);
        assert_eq!(out[0], 0b11000010);
    }

    #[test]
    fn encode_binary_output_state_t3() {
        let mut encoder = PtmBinaryProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        encoder.encode(&PtmProtocolMessage::OutputState {
            outputs: [false, false, true]
        }, &mut out);
        assert_eq!(out[0], 0b11000100);
    }

    #[test]
    fn decode_ascii_clock_edge_t1() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'1').is_none());
        assert!(matches!(decoder.feed(b'0'), Some(
            PtmProtocolMessage::ClockEdge { clocks: [true, false, false], positive: false })));
    }

    #[test]
    fn decode_ascii_clock_edge_t2() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'2').is_none());
        assert!(matches!(decoder.feed(b'1'), Some(
            PtmProtocolMessage::ClockEdge { clocks: [false, true, false], positive: true })));
    }

    #[test]
    fn decode_ascii_clock_edge_t3() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'3').is_none());
        assert!(matches!(decoder.feed(b'0'), Some(
            PtmProtocolMessage::ClockEdge { clocks: [false, false, true], positive: false })));
    }

    #[test]
    fn decode_ascii_gate_edge_t1() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'G').is_none());
        assert!(decoder.feed(b'1').is_none());
        assert!(matches!(decoder.feed(b'0'), Some(
            PtmProtocolMessage::GateEdge { gates: [true, false, false], positive: false })));
    }

    #[test]
    fn decode_ascii_gate_edge_t2() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'G').is_none());
        assert!(decoder.feed(b'2').is_none());
        assert!(matches!(decoder.feed(b'1'), Some(
            PtmProtocolMessage::GateEdge { gates: [false, true, false], positive: true })));
    }

    #[test]
    fn decode_ascii_gate_edge_t3() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'G').is_none());
        assert!(decoder.feed(b'3').is_none());
        assert!(matches!(decoder.feed(b'0'), Some(
            PtmProtocolMessage::GateEdge { gates: [false, false, true], positive: false })));
    }

    #[test]
    fn decode_ascii_clock_state() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'T').is_none());
        assert!(decoder.feed(b'0').is_none());
        assert!(decoder.feed(b'1').is_none());
        assert!(matches!(decoder.feed(b'0'), Some(
            PtmProtocolMessage::ClockState { clocks: [false, true, false] })));
    }

    #[test]
    fn decode_ascii_gate_state() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'U').is_none());
        assert!(decoder.feed(b'1').is_none());
        assert!(decoder.feed(b'0').is_none());
        assert!(matches!(decoder.feed(b'1'), Some(
            PtmProtocolMessage::GateState { gates: [true, false, true] })));
    }

    #[test]
    fn decode_ascii_output_state() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'V').is_none());
        assert!(decoder.feed(b'1').is_none());
        assert!(decoder.feed(b'1').is_none());
        assert!(matches!(decoder.feed(b'1'), Some(
            PtmProtocolMessage::OutputState { outputs: [true, true, true] })));
    }

    #[test]
    fn decode_ascii_ignore_invalid() {
        let mut decoder = PtmAsciiProtocolDecoder::new();
        assert!(decoder.feed(b' ').is_none());
        assert!(decoder.feed(b'Z').is_none());
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'1').is_none());
        assert!(matches!(decoder.feed(b'0'), Some(
            PtmProtocolMessage::ClockEdge { clocks: [true, false, false], positive: false })));
    }

    #[test]
    fn decode_binary_clock_edge() {
        let mut decoder = PtmBinaryProtocolDecoder::new();
        assert!(matches!(decoder.feed(BINARY_CLOCK_EDGE | BINARY_POLARITY_BIT | 0b101), Some(
            PtmProtocolMessage::ClockEdge { clocks: [true, false, true], positive: true })));
    }

    #[test]
    fn decode_binary_gate_edge() {
        let mut decoder = PtmBinaryProtocolDecoder::new();
        assert!(matches!(decoder.feed(BINARY_GATE_EDGE | 0b011), Some(
            PtmProtocolMessage::GateEdge { gates: [true, true, false], positive: false })));
    }

    #[test]
    fn decode_binary_clock_state() {
        let mut decoder = PtmBinaryProtocolDecoder::new();
        assert!(matches!(decoder.feed(BINARY_CLOCK_STATE | 0b110), Some(
            PtmProtocolMessage::ClockState { clocks: [false, true, true] })));
    }

    #[test]
    fn decode_binary_gate_state() {
        let mut decoder = PtmBinaryProtocolDecoder::new();
        assert!(matches!(decoder.feed(BINARY_GATE_STATE | 0b010), Some(
            PtmProtocolMessage::GateState { gates: [false, true, false] })));
    }

    #[test]
    fn decode_binary_output_state() {
        let mut decoder = PtmBinaryProtocolDecoder::new();
        assert!(matches!(decoder.feed(BINARY_OUTPUT_STATE | 0b101), Some(
            PtmProtocolMessage::OutputState { outputs: [true, false, true] })));
    }

}