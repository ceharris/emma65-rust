//! Protocol codec for the virtual 6522 VIA peripheral interface.
//!
//! This module implements encoding and decoding of the two-message protocol used between
//! a virtual VIA and an external peripheral over a byte-stream transport (socket, PTY, etc.).
//!
//! The protocol defines two message types, sent in both directions:
//! - [`ViaProtocolMessage::PortStateChange`] — communicates the state of GPIO port A or B.
//! - [`ViaProtocolMessage::ControlSignalChange`] — sets or clears one or more control signals.
//!
//! Two wire formats are supported:
//! - [`ViaProtocolFormat::Binary`] — compact one- or two-byte encoding; selected by the first
//!   received byte with its high-order bit set.
//! - [`ViaProtocolFormat::Ascii`] — human-readable three- or four-character encoding; selected
//!   by the first received byte with its high-order bit clear.
//!
//! Format selection is automatic and permanent: the first qualifying byte locks the format for
//! the lifetime of the connection. Until the format is locked, the encoder transmits ASCII.
//! See [`ViaProtocolDecoder`] and [`ViaProtocolEncoder`] for details.

/// Wire format used on a VIA protocol connection.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ViaProtocolFormat {
        /// Compact binary encoding.
        Binary,
        /// Human-readable ASCII encoding.
        Ascii,
    }

    /// A decoded VIA protocol message.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ViaProtocolMessage {
        /// The state of one GPIO port has changed.
        PortStateChange {
            /// The port identifier: `'A'` or `'B'`.
            port: char,
            /// The 8-bit pin state of the port.
            value: u8,
        },
        /// One or more control signals have changed state.
        ControlSignalChange {
            /// Bitmask of the affected control signals (CB1=bit3, CB2=bit2, CA1=bit1, CA2=bit0).
            signals: u8,
            /// `true` = signals set to logic 1; `false` = signals cleared to logic 0.
            state: bool,
        },
    }

    /// Encodes [`ViaProtocolMessage`] values into bytes for transmission.
    ///
    /// The encoder starts in ASCII mode and switches permanently to binary mode once
    /// [`ViaProtocolEncoder::select_binary`] is called. In ASCII mode a space is inserted
    /// between messages as a readability aid.
    pub struct ViaProtocolEncoder {
        /// The format currently in use for encoding.
        format: ViaProtocolFormat,
        /// Whether at least one message has been encoded (used to insert inter-message spaces).
        has_prior: bool,
    }

    impl ViaProtocolEncoder {
        /// Creates a new encoder in ASCII mode.
        pub fn new() -> Self {
            Self { format: ViaProtocolFormat::Ascii, has_prior: false }
        }

        /// Locks the encoder into binary mode. Has no effect if already in binary mode.
        pub fn select_binary(&mut self) {
            self.format = ViaProtocolFormat::Binary;
        }

        /// Returns the format currently used by this encoder.
        pub fn format(&self) -> ViaProtocolFormat {
            self.format
        }

        /// Encodes `message` and appends the resulting bytes to `out`.
        ///
        /// In ASCII mode a space separator is prepended before every message after the first.
        pub fn encode(&mut self, message: ViaProtocolMessage, out: &mut Vec<u8>) {
            match self.format {
                ViaProtocolFormat::Binary => self.encode_binary(message, out),
                ViaProtocolFormat::Ascii => self.encode_ascii(message, out),
            }
        }

        fn encode_binary(&self, message: ViaProtocolMessage, out: &mut Vec<u8>) {
            match message {
                ViaProtocolMessage::PortStateChange { port, value } => {
                    let tag = if port == 'A' { 0x80u8 } else { 0x90u8 };
                    out.push(tag);
                    out.push(value);
                }
                ViaProtocolMessage::ControlSignalChange { signals, state } => {
                    let high = if state { 0xD0u8 } else { 0xC0u8 };
                    out.push(high | (signals & 0x0F));
                }
            }
        }

        fn encode_ascii(&mut self, message: ViaProtocolMessage, out: &mut Vec<u8>) {
            if self.has_prior {
                out.push(b' ');
            }
            self.has_prior = true;
            match message {
                ViaProtocolMessage::PortStateChange { port, value } => {
                    let tag = if port == 'A' { b'A' } else { b'B' };
                    out.push(tag);
                    out.push(hex_nibble(value >> 4));
                    out.push(hex_nibble(value & 0x0F));
                }
                ViaProtocolMessage::ControlSignalChange { signals, state } => {
                    out.push(b'C');
                    // Emit each affected signal as a separate message character sequence.
                    // The ASCII format encodes one signal at a time: C<p><n><v>.
                    // Emit the lowest affected signal; callers wanting multiple signals
                    // must call encode() once per signal, or use encode_all().
                    // Per spec, <p>=A|B, <n>=1|2, <v>=0|1.
                    let (port_char, signal_num) = signal_bits_to_ascii(signals);
                    out.push(port_char);
                    out.push(signal_num);
                    out.push(if state { b'1' } else { b'0' });
                }
            }
        }
    }

    impl Default for ViaProtocolEncoder {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Decodes a byte stream into [`ViaProtocolMessage`] values.
    ///
    /// Format negotiation is automatic:
    /// - The format starts as undecided.
    /// - The first byte with its high bit set locks the decoder into binary mode.
    /// - The first byte with its high bit clear locks the decoder into ASCII mode.
    /// - Until the format is locked the decoder treats all input as potential ASCII.
    ///
    /// Invalid data is silently ignored per the protocol specification.
    pub struct ViaProtocolDecoder {
        /// Locked format, or `None` until the first qualifying byte arrives.
        format: Option<ViaProtocolFormat>,
        /// Internal parse state.
        state: DecoderState,
    }

    /// Internal state machine for the decoder.
    #[derive(Debug)]
    enum DecoderState {
        /// Waiting for the start of a message.
        Idle,
        /// Received 0x80 (port A) or 0x90 (port B); waiting for the value byte.
        BinaryPortValue { port: char },
        /// Received 'A' or 'B'; waiting for first hex digit.
        AsciiPortFirst { port: char },
        /// Received port tag and first hex digit; waiting for second hex digit.
        AsciiPortSecond { port: char, hi: u8 },
        /// Received 'C'; waiting for port char (A|B).
        AsciiControlPort,
        /// Received 'C' and port char; waiting for signal number (1|2).
        AsciiControlSignal { port: char },
        /// Received 'C', port, signal; waiting for value (0|1).
        AsciiControlValue { port: char, signal: u8 },
    }

    impl ViaProtocolDecoder {
        /// Creates a new decoder with no format selected.
        pub fn new() -> Self {
            Self { format: None, state: DecoderState::Idle }
        }

        /// Returns the format that has been locked in, or `None` if not yet determined.
        pub fn format(&self) -> Option<ViaProtocolFormat> {
            self.format
        }

        /// Feeds a single byte into the decoder.
        ///
        /// Returns `Some(message)` when a complete, valid message has been decoded, or `None`
        /// if more bytes are needed or the byte was ignored.
        pub fn feed(&mut self, byte: u8) -> Option<ViaProtocolMessage> {
            // Lock format on first qualifying byte.
            if self.format.is_none() {
                if byte & 0x80 != 0 {
                    self.format = Some(ViaProtocolFormat::Binary);
                } else {
                    self.format = Some(ViaProtocolFormat::Ascii);
                }
            }

            match self.format.unwrap() {
                ViaProtocolFormat::Binary => self.feed_binary(byte),
                ViaProtocolFormat::Ascii => self.feed_ascii(byte),
            }
        }

        fn feed_binary(&mut self, byte: u8) -> Option<ViaProtocolMessage> {
            match self.state {
                DecoderState::Idle => {
                    match byte {
                        0x80 => { self.state = DecoderState::BinaryPortValue { port: 'A' }; None }
                        0x90 => { self.state = DecoderState::BinaryPortValue { port: 'B' }; None }
                        b if (b & 0xF0) == 0xC0 => {
                            Some(ViaProtocolMessage::ControlSignalChange {
                                signals: b & 0x0F,
                                state: false,
                            })
                        }
                        b if (b & 0xF0) == 0xD0 => {
                            Some(ViaProtocolMessage::ControlSignalChange {
                                signals: b & 0x0F,
                                state: true,
                            })
                        }
                        _ => None, // silently ignore
                    }
                }
                DecoderState::BinaryPortValue { port } => {
                    self.state = DecoderState::Idle;
                    Some(ViaProtocolMessage::PortStateChange { port, value: byte })
                }
                _ => {
                    // Binary mode should only use Idle and BinaryPortValue states.
                    self.state = DecoderState::Idle;
                    None
                }
            }
        }

        fn feed_ascii(&mut self, byte: u8) -> Option<ViaProtocolMessage> {
            match &self.state {
                DecoderState::Idle => {
                    // Ignore everything except valid message start chars.
                    match byte.to_ascii_uppercase() {
                        b'A' => { self.state = DecoderState::AsciiPortFirst { port: 'A' }; None }
                        b'B' => { self.state = DecoderState::AsciiPortFirst { port: 'B' }; None }
                        b'C' => { self.state = DecoderState::AsciiControlPort; None }
                        _ => None,
                    }
                }
                DecoderState::AsciiPortFirst { port } => {
                    let port = *port;
                    // Must consume full message length even if body is invalid.
                    let hi = parse_hex_nibble(byte).unwrap_or(0xFF);
                    self.state = DecoderState::AsciiPortSecond { port, hi };
                    None
                }
                DecoderState::AsciiPortSecond { port, hi } => {
                    let (port, hi) = (*port, *hi);
                    self.state = DecoderState::Idle;
                    let lo = parse_hex_nibble(byte).unwrap_or(0xFF);
                    if hi <= 0x0F && lo <= 0x0F {
                        Some(ViaProtocolMessage::PortStateChange {
                            port,
                            value: (hi << 4) | lo,
                        })
                    } else {
                        None // invalid hex digit(s) — silently ignore
                    }
                }
                DecoderState::AsciiControlPort => {
                    let port = match byte.to_ascii_uppercase() {
                        b'A' => 'A',
                        b'B' => 'B',
                        _ => {
                            self.state = DecoderState::Idle;
                            return None;
                        }
                    };
                    self.state = DecoderState::AsciiControlSignal { port };
                    None
                }
                DecoderState::AsciiControlSignal { port } => {
                    let port = *port;
                    let signal = match byte {
                        b'1' => 1u8,
                        b'2' => 2u8,
                        _ => {
                            self.state = DecoderState::Idle;
                            return None;
                        }
                    };
                    self.state = DecoderState::AsciiControlValue { port, signal };
                    None
                }
                DecoderState::AsciiControlValue { port, signal } => {
                    let (port, signal) = (*port, *signal);
                    self.state = DecoderState::Idle;
                    let state = match byte {
                        b'0' => false,
                        b'1' => true,
                        _ => return None,
                    };
                    let bits = ascii_signal_to_bits(port, signal);
                    Some(ViaProtocolMessage::ControlSignalChange { signals: bits, state })
                }
                DecoderState::BinaryPortValue { .. } => {
                    // Should not occur in ASCII mode; reset and ignore.
                    self.state = DecoderState::Idle;
                    None
                }
            }
        }

        /// Feeds a slice of bytes and collects all decoded messages into `out`.
        pub fn feed_all(&mut self, bytes: &[u8], out: &mut Vec<ViaProtocolMessage>) {
            for &b in bytes {
                if let Some(msg) = self.feed(b) {
                    out.push(msg);
                }
            }
        }
    }

impl Default for ViaProtocolDecoder {
    fn default() -> Self {
        Self::new()
    }
}

fn hex_nibble(n: u8) -> u8 {
    if n < 10 { b'0' + n } else { b'A' + n - 10 }
}

fn parse_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

/// Maps a single ASCII `C<p><n>` signal (port A|B, number 1|2) to its bit position.
///
/// Bit layout: CB1=bit3, CB2=bit2, CA1=bit1, CA2=bit0.
fn ascii_signal_to_bits(port: char, signal: u8) -> u8 {
    match (port, signal) {
        ('A', 1) => 0x02, // CA1 = bit1
        ('A', 2) => 0x01, // CA2 = bit0
        ('B', 1) => 0x08, // CB1 = bit3
        ('B', 2) => 0x04, // CB2 = bit2
        _ => 0,
    }
}

/// Maps the lowest set bit in `signals` to its ASCII port char and signal number.
///
/// Used by the encoder when encoding a `ControlSignalChange` in ASCII mode.
/// Callers that need to encode multiple signals must call encode() once per signal bit.
fn signal_bits_to_ascii(signals: u8) -> (u8, u8) {
    if signals & 0x08 != 0 { (b'B', b'1') } // CB1
    else if signals & 0x04 != 0 { (b'B', b'2') } // CB2
    else if signals & 0x02 != 0 { (b'A', b'1') } // CA1
    else { (b'A', b'2') } // CA2
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Encoder: binary format ---

    #[test]
    fn encoder_binary_port_a_state_change() {
        let mut enc = ViaProtocolEncoder::new();
        enc.select_binary();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::PortStateChange { port: 'A', value: 0x55 }, &mut out);
        assert_eq!(out, &[0x80, 0x55]);
    }

    #[test]
    fn encoder_binary_port_b_state_change() {
        let mut enc = ViaProtocolEncoder::new();
        enc.select_binary();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::PortStateChange { port: 'B', value: 0xAA }, &mut out);
        assert_eq!(out, &[0x90, 0xAA]);
    }

    #[test]
    fn encoder_binary_clear_control_signals() {
        let mut enc = ViaProtocolEncoder::new();
        enc.select_binary();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: false }, &mut out);
        assert_eq!(out, &[0xC8]); // CB1 clear
    }

    #[test]
    fn encoder_binary_set_control_signals() {
        let mut enc = ViaProtocolEncoder::new();
        enc.select_binary();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: true }, &mut out);
        assert_eq!(out, &[0xD1]); // CA2 set
    }

    // --- Encoder: ASCII format ---

    #[test]
    fn encoder_ascii_port_a_state_change() {
        let mut enc = ViaProtocolEncoder::new();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::PortStateChange { port: 'A', value: 0x5C }, &mut out);
        assert_eq!(out, b"A5C");
    }

    #[test]
    fn encoder_ascii_port_b_state_change() {
        let mut enc = ViaProtocolEncoder::new();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::PortStateChange { port: 'B', value: 0xD3 }, &mut out);
        assert_eq!(out, b"BD3");
    }

    #[test]
    fn encoder_ascii_control_ca1_clear() {
        let mut enc = ViaProtocolEncoder::new();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: false }, &mut out);
        assert_eq!(out, b"CA10");
    }

    #[test]
    fn encoder_ascii_control_cb2_set() {
        let mut enc = ViaProtocolEncoder::new();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: true }, &mut out);
        assert_eq!(out, b"CB21");
    }

    #[test]
    fn encoder_ascii_inserts_space_between_messages() {
        let mut enc = ViaProtocolEncoder::new();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::PortStateChange { port: 'A', value: 0x00 }, &mut out);
        enc.encode(ViaProtocolMessage::PortStateChange { port: 'B', value: 0xFF }, &mut out);
        assert_eq!(out, b"A00 BFF");
    }

    #[test]
    fn encoder_ascii_no_leading_space_on_first_message() {
        let mut enc = ViaProtocolEncoder::new();
        let mut out = Vec::new();
        enc.encode(ViaProtocolMessage::PortStateChange { port: 'A', value: 0x12 }, &mut out);
        assert!(!out.starts_with(b" "));
    }

    #[test]
    fn encoder_starts_in_ascii_mode() {
        let enc = ViaProtocolEncoder::new();
        assert_eq!(enc.format(), ViaProtocolFormat::Ascii);
    }

    #[test]
    fn encoder_select_binary_locks_format() {
        let mut enc = ViaProtocolEncoder::new();
        enc.select_binary();
        assert_eq!(enc.format(), ViaProtocolFormat::Binary);
    }

    // --- Decoder: format negotiation ---

    #[test]
    fn decoder_high_bit_byte_selects_binary() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0xFF); // 0xFF: selects binary, otherwise ignored
        assert_eq!(dec.format(), Some(ViaProtocolFormat::Binary));
    }

    #[test]
    fn decoder_space_selects_ascii() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20);
        assert_eq!(dec.format(), Some(ViaProtocolFormat::Ascii));
    }

    #[test]
    fn decoder_control_char_selects_ascii() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x0A); // newline
        assert_eq!(dec.format(), Some(ViaProtocolFormat::Ascii));
    }

    #[test]
    fn decoder_format_locked_after_first_qualifying_byte() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20); // ASCII selected
        dec.feed(0xFF); // high-bit byte: format already locked, must not change
        assert_eq!(dec.format(), Some(ViaProtocolFormat::Ascii));
    }

    // --- Decoder: binary messages ---

    #[test]
    fn decoder_binary_port_a_state_change() {
        let mut dec = ViaProtocolDecoder::new();
        assert!(dec.feed(0x80).is_none()); // tag byte
        let msg = dec.feed(0x55);
        assert_eq!(msg, Some(ViaProtocolMessage::PortStateChange { port: 'A', value: 0x55 }));
    }

    #[test]
    fn decoder_binary_port_b_state_change() {
        let mut dec = ViaProtocolDecoder::new();
        assert!(dec.feed(0x90).is_none());
        let msg = dec.feed(0xAA);
        assert_eq!(msg, Some(ViaProtocolMessage::PortStateChange { port: 'B', value: 0xAA }));
    }

    #[test]
    fn decoder_binary_clear_control_signal_cb1() {
        let mut dec = ViaProtocolDecoder::new();
        let msg = dec.feed(0xC8);
        assert_eq!(msg, Some(ViaProtocolMessage::ControlSignalChange { signals: 0x08, state: false }));
    }

    #[test]
    fn decoder_binary_set_control_signal_ca2() {
        let mut dec = ViaProtocolDecoder::new();
        let msg = dec.feed(0xD1);
        assert_eq!(msg, Some(ViaProtocolMessage::ControlSignalChange { signals: 0x01, state: true }));
    }

    #[test]
    fn decoder_binary_ignores_0xff_selector_byte() {
        let mut dec = ViaProtocolDecoder::new();
        // 0xFF has high bit set → binary mode; upper nibble is 0xF → not 0x80/0x90/0xC/0xD → ignored
        let msg = dec.feed(0xFF);
        assert!(msg.is_none());
        assert_eq!(dec.format(), Some(ViaProtocolFormat::Binary));
    }

    #[test]
    fn decoder_binary_sequential_messages() {
        let mut dec = ViaProtocolDecoder::new();
        let mut out = Vec::new();
        dec.feed_all(&[0x80, 0x12, 0xD2, 0x90, 0xFF], &mut out);
        assert_eq!(out, vec![
            ViaProtocolMessage::PortStateChange { port: 'A', value: 0x12 },
            ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: true },
            ViaProtocolMessage::PortStateChange { port: 'B', value: 0xFF },
        ]);
    }

    // --- Decoder: ASCII messages ---

    #[test]
    fn decoder_ascii_port_a_state_change() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20); // select ASCII
        let mut out = Vec::new();
        dec.feed_all(b"A5C", &mut out);
        assert_eq!(out, vec![ViaProtocolMessage::PortStateChange { port: 'A', value: 0x5C }]);
    }

    #[test]
    fn decoder_ascii_port_b_state_change() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20);
        let mut out = Vec::new();
        dec.feed_all(b"BD3", &mut out);
        assert_eq!(out, vec![ViaProtocolMessage::PortStateChange { port: 'B', value: 0xD3 }]);
    }

    #[test]
    fn decoder_ascii_case_insensitive() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20);
        let mut out = Vec::new();
        dec.feed_all(b"a5c", &mut out);
        assert_eq!(out, vec![ViaProtocolMessage::PortStateChange { port: 'A', value: 0x5C }]);
    }

    #[test]
    fn decoder_ascii_control_ca1_clear() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20);
        let mut out = Vec::new();
        dec.feed_all(b"CA10", &mut out);
        assert_eq!(out, vec![ViaProtocolMessage::ControlSignalChange { signals: 0x02, state: false }]);
    }

    #[test]
    fn decoder_ascii_control_cb2_set() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20);
        let mut out = Vec::new();
        dec.feed_all(b"CB21", &mut out);
        assert_eq!(out, vec![ViaProtocolMessage::ControlSignalChange { signals: 0x04, state: true }]);
    }

    #[test]
    fn decoder_ascii_ignores_spaces_between_messages() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20);
        let mut out = Vec::new();
        dec.feed_all(b"A00 BFF", &mut out);
        assert_eq!(out, vec![
            ViaProtocolMessage::PortStateChange { port: 'A', value: 0x00 },
            ViaProtocolMessage::PortStateChange { port: 'B', value: 0xFF },
        ]);
    }

    #[test]
    fn decoder_ascii_ignores_newlines_between_messages() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20);
        let mut out = Vec::new();
        dec.feed_all(b"A12\nB34", &mut out);
        assert_eq!(out, vec![
            ViaProtocolMessage::PortStateChange { port: 'A', value: 0x12 },
            ViaProtocolMessage::PortStateChange { port: 'B', value: 0x34 },
        ]);
    }

    #[test]
    fn decoder_ascii_invalid_hex_in_port_message_silently_ignored() {
        let mut dec = ViaProtocolDecoder::new();
        dec.feed(0x20);
        // 'G' and 'Z' are not valid hex digits; full message length must still be consumed.
        let mut out = Vec::new();
        dec.feed_all(b"AGZB12", &mut out);
        // The A message is consumed (3 chars) then discarded; B12 is decoded normally.
        assert_eq!(out, vec![ViaProtocolMessage::PortStateChange { port: 'B', value: 0x12 }]);
    }

    #[test]
    fn decoder_ascii_control_all_signal_combinations() {
        let cases = [
            (b"CA10" as &[u8], 0x02u8, false),
            (b"CA11", 0x02, true),
            (b"CA20", 0x01, false),
            (b"CA21", 0x01, true),
            (b"CB10", 0x08, false),
            (b"CB11", 0x08, true),
            (b"CB20", 0x04, false),
            (b"CB21", 0x04, true),
        ];
        for (input, signals, state) in cases {
            let mut dec = ViaProtocolDecoder::new();
            dec.feed(0x20);
            let mut out = Vec::new();
            dec.feed_all(input, &mut out);
            assert_eq!(
                out,
                vec![ViaProtocolMessage::ControlSignalChange { signals, state }],
                "input = {:?}",
                std::str::from_utf8(input).unwrap(),
            );
        }
    }

    // --- Round-trip ---

    #[test]
    fn binary_round_trip_port_state_change() {
        let messages = vec![
            ViaProtocolMessage::PortStateChange { port: 'A', value: 0xDE },
            ViaProtocolMessage::PortStateChange { port: 'B', value: 0xAD },
            ViaProtocolMessage::ControlSignalChange { signals: 0x0F, state: true },
            ViaProtocolMessage::ControlSignalChange { signals: 0x05, state: false },
        ];
        let mut enc = ViaProtocolEncoder::new();
        enc.select_binary();
        let mut bytes = Vec::new();
        for &m in &messages {
            enc.encode(m, &mut bytes);
        }
        let mut dec = ViaProtocolDecoder::new();
        let mut decoded = Vec::new();
        dec.feed_all(&bytes, &mut decoded);
        assert_eq!(decoded, messages);
    }

    #[test]
    fn ascii_round_trip_port_state_change() {
        let messages = vec![
            ViaProtocolMessage::PortStateChange { port: 'A', value: 0x5C },
            ViaProtocolMessage::PortStateChange { port: 'B', value: 0xD3 },
        ];
        let mut enc = ViaProtocolEncoder::new();
        let mut bytes = Vec::new();
        for &m in &messages {
            enc.encode(m, &mut bytes);
        }
        // The encoded bytes start with 'A' (ASCII, high bit clear) → decoder selects ASCII.
        let mut dec = ViaProtocolDecoder::new();
        let mut decoded = Vec::new();
        dec.feed_all(&bytes, &mut decoded);
        assert_eq!(decoded, messages);
    }
}
