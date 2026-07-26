//! Protocol codec for the 6522 VIA peripheral interface.
//!
//! This module provides encoders and decoders for the protocol used to communicate between
//! a peripheral and the 6522 VIA device over a [transport](crate::emulator::transport).
//! The VIA device supports multiple concurrent peripheral attachments via distinct transport
//! connections. Just as is the case with real hardware, care must be taken to ensure that
//! connected peripherals don't interfere with each other.
//!
//! When a peripheral connects to the VIA transport endpoint, the current state of the VIA
//! is immediately sent to the peripheral. Subsequently, when any connected peripheral changes the
//! state of the VIA, every peripheral is informed of the change.
//!
//! Two protocol message encodings are supported; ASCII and Binary. The ASCII protocol is useful
//! for interactive sessions using a terminal or socket utility program, for the purpose of
//! education or debugging, as well as for use in simple scripting scenarios. The binary protocol
//! is compact and efficient, and is the best choice when creating peripheral implementations.
//!
//! Unrecognized peripheral input is silently ignored by the VIA.
//!
//! ## Message Types and Functions
//!
//! Six messages types are defined for the protocol:
//! 1. **Port State** - sent by the VIA to convey the current state of Port A or B; may also be sent
//!    by a peripheral to configure the state of all pins of the subject port.
//! 2. **Ctrl State** - sent by the VIA to convey the current state of the control pins for Port A
//!    or B; may also be sent by a peripheral to configure the state of both control pins for the
//!    subject port.
//! 3. **Reset Port** - sent by a peripheral to reset any combination of bits in the specified
//!    port. The VIA sends this message to convey bit-level state changes, as needed. The message
//!    includes a mask byte that identifies (with ones in the corresponding bit positions) the bits
//!    to be reset.
//! 4. **Set Port** - sent by a peripheral to set any combination of bits in the specified
//!    port. The VIA sends this message to convey bit-level state changes, as needed. The message
//!    includes a mask byte that identifies (with ones in the corresponding bit positions) the bits
//!    to be set.
//! 5. **Reset Ctrl** - sent by a peripheral to reset either control pin (`Cx1` or `Cx2`) for
//!    the specified port. The VIA sends this message to convey changes in individual control
//!    signals. The ASCII protocol message specifies the pin to reset; the binary
//!    protocol specifies a mask that identifies (with ones in the corresponding bit positions)
//!    the control bits to reset.
//! 6. **Set Ctrl** - sent by a peripheral to set either control pin (`Cx1` or `Cx2`) for
//!    the specified port. The VIA sends this message to convey changes in individual control
//!    signals.  The ASCII protocol message specifies the pin to reset; the binary
//!    protocol specifies a mask that identifies (with ones in the corresponding bit positions)
//!    the control bits to set.
//!
//! ## ASCII Protocol
//!
//! The ASCII protocol consists of short strings of printable ASCII characters. A receiver MUST
//! discard non-printable ASCII control characters (`0x00..0x1F`, `0x7F`), spaces (`0x20`), and
//! any input byte with the high-order bit set. A receiver MUST NOT distinguish upper case and
//! lower case letters.
//!
//! As an aid to human readability, distinct messages are separated by a single space character
//! when sent by the VIA. When more than 72 characters of messages and spaces have been sent by
//! the VIA, it will output a canonical ASCII CR (`0xD`) LF (`0xA`) sequence.
//!
//! | Message Type | Format | Example | Description                       |
//! |--------------|--------|---------|-----------------------------------|
//! | Port State   | `pxx`  | `A55`   | Port A state is `0x55`            |
//! | Ctrl State   | `Cpuv` | `CB10`  | Port B CB1 is high and CB2 is low |
//! | Reset Port   | `Rpxx` | `RBF0`  | Reset port B bits 4 through 7     |
//! | Set Port     | `Spxx` | `SA03`  | Set port A bits 0 and 1           |
//! | Reset Ctrl   | `RCpu` | `RCA2`  | Reset ctrl CA2 (low)              |
//! | Set Ctrl     | `SCpu` | `SCB1`  | Set ctrl CB1 (high)               |
//!
//! - _p_ is a port identifier `A` or `B`
//! - _u_ is a control pin identifier `1` or `2`
//! - _v_ is a bit state `0` or `1`
//! - _x_ is an ASCII hexadecimal digit `0`..`F`
//!
//! ## Binary Protocol
//!
//! Each message in the binary protocol starts with a byte whose high order bit is set. The upper
//! four bits of this byte are used to distinguish the message type, while the lower four bits
//! are used for message parameters. **Port State**, **Reset Port**, and **Set Port** messages are
//! followed by an additional data byte.
//!
//! | Message Type | Format                | Example               | Description                         |
//! |--------------|-----------------------|-----------------------|-------------------------------------|
//! | Port State   | `10000p00` `bbbbbbbb` | `10000110` `01010101` | Port B state: PB=0x55               |
//! | Ctrl State   | `10010puv`            | `10010001`            | Port A control state: CA1=0 CA2=1   |
//! | Reset Port   | `10100pyz` `bbbbbbbb` | `10100000` `00000011` | Reset PA bits 0 and 1               |
//! | Set Port     | `10110pyz` `bbbbbbbb` | `10110110` `11110000` | Set CB1 and PB bits 4 through 7     |
//! | Reset Ctrl   | `11000pyz`            | `11000101`            | Reset CB2                           |
//! | Set Ctrl     | `11010pyz`            | `11010011`            | Set CA1 and CA2                     |
//!
//! - _b_ is an arbitrary bit, `0` or `1`
//! - _p_ identifies the port; `0` for port A, `1` for port B
//! - _u_ and _v_ are the states of control pins `Cp1` and `Cp2` (respectively); `0` or `1`
//! - _y_ and _z_ are mask bits that identify whether controls pins `Cp1` and `Cp2` (respectively)
//!   are affected by a reset or set operation; `0` or `1`

use super::{ProtocolMessageDecoder, ProtocolMessageEncoder, ProtocolMessageEncoding};

/// Position of control signal 1 (`Cp1`) bit in a binary-encoded protocol message.
pub const VIA_CTRL1_MASK: u8 = 0b00000010;
/// Position of control signal 2 (`Cp2`) bit in a binary-encoded protocol message.
pub const VIA_CTRL2_MASK: u8 = 0b00000001;
/// Position of the ctrl signals field in a binary-encoded protocol message.
pub const VIA_CTRL_MASK: u8 = VIA_CTRL1_MASK | VIA_CTRL2_MASK;
/// Position of the port bit in a binary-encoded protocol message.
pub const VIA_PORT_MASK: u8 = 0b00000100;
/// Bit mask used to isolate the message type field in a binary-encoded protocol message.
pub const VIA_TYPE_MASK: u8 = 0b11110000;
/// Field value for the Port State message type in a binary-encoded protocol message.
pub const VIA_TYPE_PORT_STATE: u8 = 0b10000000;
/// Field value for the Ctrl State message type in a binary-encoded protocol message.
pub const VIA_TYPE_CTRL_STATE: u8 = 0b10010000;
/// Field value for the Reset Port message type in a binary-encoded protocol message.
pub const VIA_TYPE_RESET_PORT: u8 = 0b10100000;
/// Field value for the Set Port message type in a binary-encoded protocol message.
pub const VIA_TYPE_SET_PORT: u8 = 0b10110000;
/// Field value for the Reset Ctrl message type in a binary-encoded protocol message.
pub const VIA_TYPE_RESET_CTRL: u8 = 0b11000000;
/// Field value for the Set Ctrl message type in a binary-encoded protocol message.
pub const VIA_TYPE_SET_CTRL: u8 = 0b11010000;

/// A decoded VIA protocol message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViaProtocolMessage {
    /// Conveys the state of a port.
    PortState {
        /// Port identifier: `'A'` or `'B'`
        port: u8,
        /// 8-bit state of the port
        port_state: u8,
    },
    /// Conveys the state of the control pins for a port.
    CtrlState {
        /// Port identifier: `'A'` or `'B'`
        port: u8,
        /// State for control pins `Cp1` and `Cp2`.
        ctrl_state: u8,
    },
    /// Resets data and/or control pins for a port
    ResetPort {
        /// Port identifier: `'A'` or `'B'`
        port: u8,
        /// Mask to apply to the port state
        port_mask: u8,
        /// Mask to apply to the control pins for the port
        ctrl_mask: u8,
    },
    /// Sets data and/or control pins for a port
    SetPort {
        /// Port identifier: `'A'` or `'B'`
        port: u8,
        /// Mask to apply to the port state
        port_mask: u8,
        /// Mask to apply to the control pins for the port
        ctrl_mask: u8,
    },
    /// Resets control pins for a port
    ResetCtrl {
        /// Port identifier: `'A'` or `'B'`
        port: u8,
        /// Mask to apply to the control pins for the port
        ctrl_mask: u8,
    },
    /// Sets control pins for a port
    SetCtrl {
        /// Port identifier: `'A'` or `'B'`
        port: u8,
        /// Mask to apply to the control pins for the port
        ctrl_mask: u8,
    },
}

/// Creates a new encoder for protocol format `encoding`.
pub fn new_encoder(encoding: ProtocolMessageEncoding)
                   -> Box<dyn ProtocolMessageEncoder<ViaProtocolMessage>> {
    match encoding {
        ProtocolMessageEncoding::Ascii => Box::new(ViaAsciiProtocolEncoder::new()),
        ProtocolMessageEncoding::Binary => Box::new(ViaBinaryProtocolEncoder::new())
    }
}

/// Creates a new decoder for protocol format `encoding`.
pub fn new_decoder(encoding: ProtocolMessageEncoding)
                   -> Box<dyn ProtocolMessageDecoder<ViaProtocolMessage>> {
    match encoding {
        ProtocolMessageEncoding::Ascii => Box::new(ViaAsciiProtocolDecoder::new()),
        ProtocolMessageEncoding::Binary => Box::new(ViaBinaryProtocolDecoder::new())
    }
}

/// Encodes [`ViaProtocolMessage`] values into ASCII format for transmission.
///
/// A space is inserted between messages as a human readability aid.
pub struct ViaAsciiProtocolEncoder {
    /// Whether at least one message has been encoded (used to insert inter-message spaces).
    line_length: u8,
}

impl Default for ViaAsciiProtocolEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolMessageEncoder<ViaProtocolMessage> for ViaAsciiProtocolEncoder {

    /// Encodes `message` and appends the resulting bytes to `out`.
    ///
    /// In ASCII mode a space separator is prepended before every message after the first.
    fn encode(&mut self, message: &ViaProtocolMessage, out: &mut Vec<u8>) {
        self.encode_ascii(message, out);
    }

}

impl ViaAsciiProtocolEncoder {
    /// Creates a new encoder in ASCII mode.
    pub fn new() -> Self {
        Self { line_length: 0 }
    }

    fn encode_ascii(&mut self, message: &ViaProtocolMessage, out: &mut Vec<u8>) {
        match message {
            ViaProtocolMessage::PortState { port, port_state} => {
                self.encode_ascii_port_state(*port, *port_state, out);
            }
            ViaProtocolMessage::CtrlState { port, ctrl_state } => {
                self.encode_ascii_ctrl_state(*port, *ctrl_state, out);
            }
            ViaProtocolMessage::ResetPort { port, port_mask, ctrl_mask } => {
                self.encode_ascii_port_change(b'R', *port, *port_mask, out);
                self.encode_ascii_port_ctrl_change(b'R', *port, *ctrl_mask, out);
            }
            ViaProtocolMessage::SetPort { port, port_mask, ctrl_mask } => {
                self.encode_ascii_port_change(b'S', *port, *port_mask, out);
                self.encode_ascii_port_ctrl_change(b'S', *port, *ctrl_mask, out);
            }
            ViaProtocolMessage::ResetCtrl { port, ctrl_mask } => {
                self.encode_ascii_port_ctrl_change(b'R', *port, *ctrl_mask, out);
            }
            ViaProtocolMessage::SetCtrl { port, ctrl_mask } => {
                self.encode_ascii_port_ctrl_change(b'S', *port, *ctrl_mask, out);
            }
        }
    }

    fn encode_ascii_port_state(&mut self, port: u8, port_state: u8, out: &mut Vec<u8>) {
        self.encode_ascii_prefix(port, out);
        self.encode_ascii_byte(port_state, out);
        self.encode_ascii_space(out);
    }

    fn encode_ascii_port_change(&mut self, which: u8, port: u8, port_mask: u8, out: &mut Vec<u8>) {
        self.encode_ascii_prefix(which, out);
        self.encode_ascii_char(port, out);
        self.encode_ascii_byte(port_mask, out);
        self.encode_ascii_space(out);
    }

    fn encode_ascii_port_ctrl_change(&mut self, which: u8, port: u8, ctrl_mask: u8, out: &mut Vec<u8>) {
        if ctrl_mask & VIA_CTRL1_MASK != 0 {
            self.encode_ascii_ctrl_change(which, port, b'1', out);
        }
        if ctrl_mask & VIA_CTRL2_MASK != 0 {
            self.encode_ascii_ctrl_change(which, port, b'2', out);
        }
    }

    fn encode_ascii_ctrl_change(&mut self, which: u8, port: u8, pin: u8, out: &mut Vec<u8>) {
        self.encode_ascii_prefix(which, out);
        self.encode_ascii_char(b'C', out);
        self.encode_ascii_char(port, out);
        self.encode_ascii_char(pin, out);
        self.encode_ascii_space(out);
    }

    fn encode_ascii_ctrl_state(&mut self, port: u8, ctrl_state: u8, out: &mut Vec<u8>) {
        self.encode_ascii_prefix(b'C', out);
        self.encode_ascii_char(port, out);
        self.encode_ascii_bit(ctrl_state & VIA_CTRL1_MASK != 0, out);
        self.encode_ascii_bit(ctrl_state & VIA_CTRL2_MASK != 0, out);
        self.encode_ascii_space(out);
    }

    fn encode_ascii_prefix(&mut self, prefix: u8, out: &mut Vec<u8>) {
        if self.line_length >= 72 {
            self.encode_ascii_newline(out);
        }
        self.encode_ascii_char(prefix, out);
    }

    fn encode_ascii_bit(&mut self, bit: bool, out: &mut Vec<u8>) {
        self.encode_ascii_char(if bit { b'1' } else { b'0' }, out);
    }

    fn encode_ascii_byte(&mut self, b: u8, out: &mut Vec<u8>) {
        self.encode_ascii_char(hex_nibble(b >> 4), out);
        self.encode_ascii_char(hex_nibble(b & 0xF), out);
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

/// Encodes [`ViaProtocolMessage`] values into binary format for transmission.
pub struct ViaBinaryProtocolEncoder;

impl Default for ViaBinaryProtocolEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolMessageEncoder<ViaProtocolMessage> for ViaBinaryProtocolEncoder {

    /// Encodes `message` and appends the resulting bytes to `out`.
    ///
    /// In ASCII mode a space separator is prepended before every message after the first.
    fn encode(&mut self, message: &ViaProtocolMessage, out: &mut Vec<u8>) {
        self.encode_binary(message, out);
    }

}

impl ViaBinaryProtocolEncoder {
    /// Creates a new encoder in ASCII mode.
    pub fn new() -> Self {
        Self {}
    }

    fn encode_binary(&self, message: &ViaProtocolMessage, out: &mut Vec<u8>) {
        match message {
            ViaProtocolMessage::PortState { port, port_state} => {
                out.push(Self::encode_message_byte(VIA_TYPE_PORT_STATE, *port, 0));
                out.push(*port_state);
            }
            ViaProtocolMessage::CtrlState { port, ctrl_state } => {
                out.push(Self::encode_message_byte(VIA_TYPE_CTRL_STATE, *port, *ctrl_state));
            }
            ViaProtocolMessage::ResetPort { port, port_mask, ctrl_mask} => {
                out.push(Self::encode_message_byte(VIA_TYPE_RESET_PORT, *port, *ctrl_mask));
                out.push(*port_mask);
            }
            ViaProtocolMessage::SetPort { port, port_mask, ctrl_mask} => {
                out.push(Self::encode_message_byte(VIA_TYPE_SET_PORT, *port, *ctrl_mask));
                out.push(*port_mask);
            }
            ViaProtocolMessage::ResetCtrl { port, ctrl_mask} => {
                out.push(Self::encode_message_byte(VIA_TYPE_RESET_CTRL, *port, *ctrl_mask));
            }
            ViaProtocolMessage::SetCtrl { port, ctrl_mask} => {
                out.push(Self::encode_message_byte(VIA_TYPE_SET_CTRL, *port, *ctrl_mask));
            }
        }
    }

    fn encode_message_byte(message_type: u8, port: u8, ctrl_field: u8) -> u8 {
        (message_type & VIA_TYPE_MASK)
            | (if port == b'B' { VIA_PORT_MASK } else { 0 })
            | (ctrl_field & VIA_CTRL_MASK)
    }

}

#[derive(Debug)]
enum AsciiDecoderState {
    /// Waiting for the start of a message.
    Idle,
    PortState { port: u8 },
    PortStateHigh { port: u8, high_nibble: u8},
    CtrlState,
    CtrlStatePort { port: u8 },
    CtrlStatePin1 { port: u8, pin: bool },
    Reset,
    ResetPort { port: u8 },
    ResetPortHigh { port: u8, high_nibble: u8 },
    Set,
    SetPort { port: u8 },
    SetPortHigh { port: u8, high_nibble: u8 },
    ResetCtrl,
    ResetCtrlPort { port: u8 },
    SetCtrl,
    SetCtrlPort { port: u8 },
}

/// Decodes an ASCII-encoded byte stream into [`ViaProtocolMessage`] values.
///
/// Invalid data is silently ignored per the protocol specification.
pub struct ViaAsciiProtocolDecoder {
    /// Internal parse state.
    state: AsciiDecoderState,
}

impl Default for ViaAsciiProtocolDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolMessageDecoder<ViaProtocolMessage> for ViaAsciiProtocolDecoder {
    
    /// Feeds a single byte into the decoder.
    ///
    /// Returns `Some(message)` when a complete, valid message has been decoded, or `None`
    /// if more bytes are needed or the byte was ignored.
    fn feed(&mut self, byte: u8) -> Option<ViaProtocolMessage> {
        self.feed_ascii(byte)
    }

}

impl ViaAsciiProtocolDecoder {
    /// Creates a new decoder with no format selected.
    pub fn new() -> Self {
        Self { state: AsciiDecoderState::Idle }
    }

    fn feed_ascii(&mut self, byte: u8) -> Option<ViaProtocolMessage> {
        match &self.state {
            AsciiDecoderState::Idle => {
                match byte.to_ascii_uppercase() {
                    b'A' => {
                        self.state = AsciiDecoderState::PortState { port: b'A' };
                    }
                    b'B' => {
                        self.state = AsciiDecoderState::PortState { port: b'B' };
                    }
                    b'C' => {
                        self.state = AsciiDecoderState::CtrlState;
                    }
                    b'R' => {
                        self.state = AsciiDecoderState::Reset;
                    }
                    b'S' => {
                        self.state = AsciiDecoderState::Set;
                    }
                    _ => {}
                }
                None
            }
            AsciiDecoderState::PortState { port } => {
                if let Some(high_nibble) = parse_hex_nibble(byte) {
                    self.state = AsciiDecoderState::PortStateHigh { port: *port, high_nibble };
                } else {
                    self.state = AsciiDecoderState::Idle;
                }
                None
            }
            AsciiDecoderState::PortStateHigh { port, high_nibble } => {
                let port = *port;
                let high_nibble = *high_nibble;
                self.state = AsciiDecoderState::Idle;
                if let Some(low_nibble) = parse_hex_nibble(byte) {
                    let port_state = high_nibble << 4 | low_nibble;
                    Some(ViaProtocolMessage::PortState { port, port_state })
                } else {
                    None
                }
            }
            AsciiDecoderState::CtrlState => {
                let port = byte.to_ascii_uppercase();
                match port {
                    b'A' | b'B' => {
                        self.state = AsciiDecoderState::CtrlStatePort { port };
                    }
                    _ => {
                        self.state = AsciiDecoderState::Idle;
                    },
                }
                None
            }
            AsciiDecoderState::CtrlStatePort { port} => {
                match byte {
                    b'0' | b'1' => {
                        self.state = AsciiDecoderState::CtrlStatePin1 {
                            port: *port,
                            pin: byte == b'1',
                        };
                    }
                    _ => {
                        self.state = AsciiDecoderState::Idle;
                    }
                }
                None
            }
            AsciiDecoderState::CtrlStatePin1 { port, pin } => {
                match byte {
                    b'0' | b'1' => {
                        Some(ViaProtocolMessage::CtrlState {
                            port: *port,
                            ctrl_state: (if *pin { VIA_CTRL1_MASK } else { 0 })
                                | (if byte== b'1' { VIA_CTRL2_MASK } else { 0 })
                        })
                    }
                    _ => {
                        self.state = AsciiDecoderState::Idle;
                        None
                    }
                }
            }
            AsciiDecoderState::Reset => {
                let port = byte.to_ascii_uppercase();
                match port {
                    b'A' | b'B' => {
                        self.state = AsciiDecoderState::ResetPort { port };
                    }
                    b'C' => {
                        self.state = AsciiDecoderState::ResetCtrl;
                    }
                    _ => {
                        self.state = AsciiDecoderState::Idle;
                    }
                }
                None
            }
            AsciiDecoderState::ResetPort { port } => {
                if let Some(high_nibble) = parse_hex_nibble(byte) {
                    self.state = AsciiDecoderState::ResetPortHigh { port: *port, high_nibble };
                } else {
                    self.state = AsciiDecoderState::Idle;
                }
                None
            }
            AsciiDecoderState::ResetPortHigh { port, high_nibble } => {
                let port = *port;
                let high_nibble = *high_nibble;
                self.state = AsciiDecoderState::Idle;
                if let Some(low_nibble) = parse_hex_nibble(byte) {
                    let port_mask = high_nibble << 4 | low_nibble;
                    Some(ViaProtocolMessage::ResetPort { port, port_mask, ctrl_mask: 0 })
                } else {
                    None
                }
            }
            AsciiDecoderState::Set => {
                let port = byte.to_ascii_uppercase();
                match port {
                    b'A' | b'B' => {
                        self.state = AsciiDecoderState::SetPort { port };
                    }
                    b'C' => {
                        self.state = AsciiDecoderState::SetCtrl;
                    }
                    _ => {
                        self.state = AsciiDecoderState::Idle;
                    }
                }
                None
            }
            AsciiDecoderState::SetPort { port } => {
                if let Some(high_nibble) = parse_hex_nibble(byte) {
                    self.state = AsciiDecoderState::SetPortHigh { port: *port, high_nibble };
                } else {
                    self.state = AsciiDecoderState::Idle;
                }
                None
            }
            AsciiDecoderState::SetPortHigh { port, high_nibble } => {
                let port = *port;
                let high_nibble = *high_nibble;
                self.state = AsciiDecoderState::Idle;
                if let Some(low_nibble) = parse_hex_nibble(byte) {
                    let port_mask = high_nibble << 4 | low_nibble;
                    Some(ViaProtocolMessage::SetPort { port, port_mask, ctrl_mask: 0 })
                } else {
                    None
                }
            }
            AsciiDecoderState::ResetCtrl => {
                let port = byte.to_ascii_uppercase();
                match port {
                    b'A' | b'B' => {
                        self.state = AsciiDecoderState::ResetCtrlPort { port };
                    }
                    _ => {
                        self.state = AsciiDecoderState::Idle;
                    }
                }
                None
            }
            AsciiDecoderState::ResetCtrlPort { port} => {
                let port = *port;
                self.state = AsciiDecoderState::Idle;
                match byte {
                    b'1' => {
                        Some(ViaProtocolMessage::ResetCtrl {
                            port,
                            ctrl_mask: VIA_CTRL1_MASK
                        })
                    }
                    b'2' => {
                        Some(ViaProtocolMessage::ResetCtrl {
                            port,
                            ctrl_mask: VIA_CTRL2_MASK
                        })
                    }
                    _ => {
                        None
                    }
                }
            }
            AsciiDecoderState::SetCtrl => {
                let port = byte.to_ascii_uppercase();
                match port {
                    b'A' | b'B' => {
                        self.state = AsciiDecoderState::SetCtrlPort { port };
                    }
                    _ => {
                        self.state = AsciiDecoderState::Idle;
                    }
                }
                None
            }
            AsciiDecoderState::SetCtrlPort { port } => {
                let port = *port;
                self.state = AsciiDecoderState::Idle;
                match byte {
                    b'1' => {
                        Some(ViaProtocolMessage::SetCtrl { port, ctrl_mask: VIA_CTRL1_MASK })
                    }
                    b'2' => {
                        Some(ViaProtocolMessage::SetCtrl { port, ctrl_mask: VIA_CTRL2_MASK })
                    }
                    _ => {
                        None
                    }
                }
            }
        }
    }

}

#[derive(Debug)]
enum BinaryDecoderState {
    Idle,
    PortState { message: u8 },
    ResetPort { message: u8 },
    SetPort { message: u8 },
}

/// Decodes a binary-encoded byte stream into [`ViaProtocolMessage`] values.
///
/// Invalid data is silently ignored per the protocol specification.
pub struct ViaBinaryProtocolDecoder {
    state: BinaryDecoderState,   
}

impl Default for ViaBinaryProtocolDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolMessageDecoder<ViaProtocolMessage> for ViaBinaryProtocolDecoder {

    /// Feeds a single byte into the decoder.
    ///
    /// Returns `Some(message)` when a complete, valid message has been decoded, or `None`
    /// if more bytes are needed or the byte was ignored.
    fn feed(&mut self, byte: u8) -> Option<ViaProtocolMessage> {
        self.feed_binary(byte)
    }

}

impl ViaBinaryProtocolDecoder {
    /// Creates a new decoder with no format selected.
    pub fn new() -> Self {
        Self {
            state: BinaryDecoderState::Idle,
        }
    }

    fn feed_binary(&mut self, byte: u8) -> Option<ViaProtocolMessage> {
        match self.state {
            BinaryDecoderState::Idle => {
                match byte & VIA_TYPE_MASK {
                    VIA_TYPE_PORT_STATE => {
                        self.state = BinaryDecoderState::PortState { message: byte };
                        None
                    }
                    VIA_TYPE_CTRL_STATE => {
                        Some(ViaProtocolMessage::CtrlState {
                            port: if byte & VIA_PORT_MASK == 0 { b'A' } else { b'B' },
                            ctrl_state: byte & VIA_CTRL_MASK,
                        })
                    }
                    VIA_TYPE_RESET_PORT => {
                        self.state = BinaryDecoderState::ResetPort { message: byte };
                        None
                    }
                    VIA_TYPE_SET_PORT => {
                        self.state = BinaryDecoderState::SetPort { message: byte };
                        None
                    }
                    VIA_TYPE_RESET_CTRL => {
                        Some(ViaProtocolMessage::ResetCtrl {
                            port: if byte & VIA_PORT_MASK == 0 { b'A' } else { b'B' },
                            ctrl_mask: byte & VIA_CTRL_MASK,
                        })
                    }
                    VIA_TYPE_SET_CTRL => {
                        Some(ViaProtocolMessage::SetCtrl {
                            port: if byte & VIA_PORT_MASK == 0 { b'A' } else { b'B' },
                            ctrl_mask: byte & VIA_CTRL_MASK,
                        })
                    }
                    _ => {
                        None
                    }
                }
            }
            BinaryDecoderState::PortState { message } => {
                self.state = BinaryDecoderState::Idle;
                Some(ViaProtocolMessage::PortState {
                    port: if message & VIA_PORT_MASK == 0 { b'A' } else { b'B' },
                    port_state: byte,
                })
            }
            BinaryDecoderState::ResetPort { message } => {
                self.state = BinaryDecoderState::Idle;
                Some(ViaProtocolMessage::ResetPort {
                    port: if message & VIA_PORT_MASK == 0 { b'A' } else { b'B' },
                    port_mask: byte,
                    ctrl_mask: message & VIA_CTRL_MASK,
                })
            }
            BinaryDecoderState::SetPort { message } => {
                self.state = BinaryDecoderState::Idle;
                Some(ViaProtocolMessage::SetPort {
                    port: if message & VIA_PORT_MASK == 0 { b'A' } else { b'B' },
                    port_mask: byte,
                    ctrl_mask: message & VIA_CTRL_MASK,
                })
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii_encoder_and_out() -> (ViaAsciiProtocolEncoder, Vec<u8>) {
        (ViaAsciiProtocolEncoder::new(), Vec::new())
    }

    #[test]
    fn encode_ascii_port_a_state() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::PortState {
                port: b'A',
                port_state: 0x55,
            },
            &mut out);
        assert_eq!(out, b"A55 ");
    }

    #[test]
    fn encode_ascii_port_b_state() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::PortState {
                port: b'B',
                port_state: 0xAA,
            },
            &mut out);
        assert_eq!(out, b"BAA ");
    }

    #[test]
    fn encode_ascii_port_a_ctrl_state() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::CtrlState {
                port: b'A',
                ctrl_state: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"CA11 ");
    }

    #[test]
    fn encode_ascii_port_b_ctrl_state() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::CtrlState {
                port: b'B',
                ctrl_state: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"CB11 ");
    }

    #[test]
    fn encode_ascii_port_a_reset_without_ctrl_mask() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetPort {
                port: b'A',
                port_mask: 0x55,
                ctrl_mask: 0x00,
            },
            &mut out);
        assert_eq!(out, b"RA55 ");
    }

    #[test]
    fn encode_ascii_port_a_reset_with_ctrl_mask() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetPort {
                port: b'A',
                port_mask: 0x55,
                ctrl_mask: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"RA55 RCA1 RCA2 ");
    }

    #[test]
    fn encode_ascii_port_b_reset_without_ctrl_mask() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetPort {
                port: b'B',
                port_mask: 0xAA,
                ctrl_mask: 0x00,
            },
            &mut out);
        assert_eq!(out, b"RBAA ");
    }

    #[test]
    fn encode_ascii_port_b_reset_with_ctrl_mask() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetPort {
                port: b'B',
                port_mask: 0xAA,
                ctrl_mask: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"RBAA RCB1 RCB2 ");
    }

    #[test]
    fn encode_ascii_port_a_set_without_ctrl_mask() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetPort {
                port: b'A',
                port_mask: 0x55,
                ctrl_mask: 0x00,
            },
            &mut out);
        assert_eq!(out, b"SA55 ");
    }

    #[test]
    fn encode_ascii_port_a_set_with_ctrl_mask() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetPort {
                port: b'A',
                port_mask: 0x55,
                ctrl_mask: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"SA55 SCA1 SCA2 ");
    }

    #[test]
    fn encode_ascii_port_b_set_without_ctrl_mask() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetPort {
                port: b'B',
                port_mask: 0xAA,
                ctrl_mask: 0x00,
            },
            &mut out);
        assert_eq!(out, b"SBAA ");
    }

    #[test]
    fn encode_ascii_port_b_set_with_ctrl_mask() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetPort {
                port: b'B',
                port_mask: 0xAA,
                ctrl_mask: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"SBAA SCB1 SCB2 ");
    }

    #[test]
    fn encode_ascii_port_a_reset_ctrl() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetCtrl {
                port: b'A',
                ctrl_mask: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"RCA1 RCA2 ");
    }

    #[test]
    fn encode_ascii_port_b_reset_ctrl() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetCtrl {
                port: b'B',
                ctrl_mask: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"RCB1 RCB2 ");
    }

    #[test]
    fn encode_ascii_port_a_set_ctrl() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetCtrl {
                port: b'A',
                ctrl_mask: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"SCA1 SCA2 ");
    }

    #[test]
    fn encode_ascii_port_b_set_ctrl() {
        let (mut enc, mut out) = ascii_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetCtrl {
                port: b'B',
                ctrl_mask: 0xFF,
            },
            &mut out);
        assert_eq!(out, b"SCB1 SCB2 ");
    }

    #[test]
    fn encode_ascii_inserts_newline() {
        let mut encoder = ViaAsciiProtocolEncoder::new();
        let mut out: Vec<u8> = Vec::new();
        let mut expected: String = String::new();
        for _ in 0..(72 / 4) {
            encoder.encode(&ViaProtocolMessage::PortState {
                port: b'A',
                port_state: 0xFF,
            }, &mut out);
            expected.push_str("AFF ");
        }
        assert_eq!(out, expected.as_bytes());
        encoder.encode(&ViaProtocolMessage::PortState {
            port: b'A',
            port_state: 0xFF,
        }, &mut out);
        expected.push_str("\r\nAFF ");
        assert_eq!(out, expected.as_bytes());
    }

    fn binary_encoder_and_out() -> (ViaBinaryProtocolEncoder, Vec<u8>) {
        (ViaBinaryProtocolEncoder::new(), Vec::new())
    }

    #[test]
    fn encode_binary_port_a_state() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::PortState {
                port: b'A',
                port_state: 0x55,
            },
            &mut out);
        assert_eq!(out, &[0b10000000, 0b01010101]);
    }

    #[test]
    fn encode_binary_port_b_state() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::PortState {
                port: b'B',
                port_state: 0xAA,
            },
            &mut out);
        assert_eq!(out, &[0b10000100, 0b10101010]);
    }

    #[test]
    fn encode_binary_port_a_ctrl_state() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::CtrlState {
                port: b'A',
                ctrl_state: 0x01,
            },
            &mut out);
        assert_eq!(out, &[0b10010001]);
    }

    #[test]
    fn encode_binary_port_b_ctrl_state() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::CtrlState {
                port: b'B',
                ctrl_state: 0x02,
            },
            &mut out);
        assert_eq!(out, &[0b10010110]);
    }

    #[test]
    fn encode_binary_port_a_reset() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetPort {
                port: b'A',
                port_mask: 0x55,
                ctrl_mask: 0x01,
            },
            &mut out);
        assert_eq!(out, &[0b10100001, 0b01010101]);
    }

    #[test]
    fn encode_binary_port_b_reset() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetPort {
                port: b'B',
                port_mask: 0xAA,
                ctrl_mask: 0x02,
            },
            &mut out);
        assert_eq!(out, &[0b10100110, 0b10101010]);
    }

    #[test]
    fn encode_binary_port_a_set() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetPort {
                port: b'A',
                port_mask: 0x55,
                ctrl_mask: 0x01,
            },
            &mut out);
        assert_eq!(out, &[0b10110001, 0b01010101]);
    }

    #[test]
    fn encode_binary_port_b_set() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetPort {
                port: b'B',
                port_mask: 0xAA,
                ctrl_mask: 0x02,
            },
            &mut out);
        assert_eq!(out, &[0b10110110, 0b10101010]);
    }

    #[test]
    fn encode_binary_port_a_ctrl_reset() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetCtrl {
                port: b'A',
                ctrl_mask: 0x01,
            },
            &mut out);
        assert_eq!(out, &[0b11000001]);
    }

    #[test]
    fn encode_binary_port_b_ctrl_reset() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::ResetCtrl {
                port: b'B',
                ctrl_mask: 0x02,
            },
            &mut out);
        assert_eq!(out, &[0b11000110]);
    }

    #[test]
    fn encode_binary_port_a_ctrl_set() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetCtrl {
                port: b'A',
                ctrl_mask: 0x01,
            },
            &mut out);
        assert_eq!(out, &[0b11010001]);
    }

    #[test]
    fn encode_binary_port_b_ctrl_set() {
        let (mut enc, mut out) = binary_encoder_and_out();
        enc.encode(
            &ViaProtocolMessage::SetCtrl {
                port: b'B',
                ctrl_mask: 0x02,
            },
            &mut out);
        assert_eq!(out, &[0b11010110]);
    }

    #[test]
    fn decode_ascii_port_a_state() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'A').is_none());
        assert!(decoder.feed(b'5').is_none());
        let message = decoder.feed(b'A');
        assert_eq!(message, Some(ViaProtocolMessage::PortState { port: b'A', port_state: 0x5A }));
    }

    #[test]
    fn decode_ascii_port_b_state() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'B').is_none());
        assert!(decoder.feed(b'A').is_none());
        let message = decoder.feed(b'5');
        assert_eq!(message, Some(ViaProtocolMessage::PortState { port: b'B', port_state: 0xA5 }));
    }

    #[test]
    fn decode_ascii_port_a_ctrl_state() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'A').is_none());
        assert!(decoder.feed(b'0').is_none());
        let message = decoder.feed(b'1');
        assert_eq!(message, Some(ViaProtocolMessage::CtrlState { port: b'A', ctrl_state: VIA_CTRL2_MASK }));
    }

    #[test]
    fn decode_ascii_port_b_ctrl_state() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'B').is_none());
        assert!(decoder.feed(b'1').is_none());
        let message = decoder.feed(b'0');
        assert_eq!(message, Some(ViaProtocolMessage::CtrlState { port: b'B', ctrl_state: VIA_CTRL1_MASK }));
    }

    #[test]
    fn decode_ascii_port_a_reset() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'R').is_none());
        assert!(decoder.feed(b'A').is_none());
        assert!(decoder.feed(b'5').is_none());
        let message = decoder.feed(b'A');
        assert_eq!(message, Some(ViaProtocolMessage::ResetPort { port: b'A', port_mask: 0x5A, ctrl_mask: 0 }));
    }

    #[test]
    fn decode_ascii_port_b_reset() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'R').is_none());
        assert!(decoder.feed(b'B').is_none());
        assert!(decoder.feed(b'A').is_none());
        let message = decoder.feed(b'5');
        assert_eq!(message, Some(ViaProtocolMessage::ResetPort { port: b'B', port_mask: 0xA5, ctrl_mask: 0 }));
    }

    #[test]
    fn decode_ascii_port_a_set() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'S').is_none());
        assert!(decoder.feed(b'A').is_none());
        assert!(decoder.feed(b'5').is_none());
        let message = decoder.feed(b'A');
        assert_eq!(message, Some(ViaProtocolMessage::SetPort { port: b'A', port_mask: 0x5A, ctrl_mask: 0 }));
    }

    #[test]
    fn decode_ascii_port_b_set() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'S').is_none());
        assert!(decoder.feed(b'B').is_none());
        assert!(decoder.feed(b'A').is_none());
        let message = decoder.feed(b'5');
        assert_eq!(message, Some(ViaProtocolMessage::SetPort { port: b'B', port_mask: 0xA5, ctrl_mask: 0 }));
    }

    #[test]
    fn decode_ascii_port_a_ctrl_reset() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'R').is_none());
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'A').is_none());
        let message = decoder.feed(b'1');
        assert_eq!(message, Some(ViaProtocolMessage::ResetCtrl { port: b'A', ctrl_mask: VIA_CTRL1_MASK }));
    }

    #[test]
    fn decode_ascii_port_b_ctrl_reset() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'R').is_none());
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'B').is_none());
        let message = decoder.feed(b'2');
        assert_eq!(message, Some(ViaProtocolMessage::ResetCtrl { port: b'B', ctrl_mask: VIA_CTRL2_MASK }));
    }

    #[test]
    fn decode_ascii_port_a_ctrl_set() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'S').is_none());
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'A').is_none());
        let message = decoder.feed(b'1');
        assert_eq!(message, Some(ViaProtocolMessage::SetCtrl { port: b'A', ctrl_mask: VIA_CTRL1_MASK }));
    }

    #[test]
    fn decode_ascii_port_b_ctrl_set() {
        let mut decoder = ViaAsciiProtocolDecoder::new();
        assert!(decoder.feed(b'S').is_none());
        assert!(decoder.feed(b'C').is_none());
        assert!(decoder.feed(b'B').is_none());
        let message = decoder.feed(b'2');
        assert_eq!(message, Some(ViaProtocolMessage::SetCtrl { port: b'B', ctrl_mask: VIA_CTRL2_MASK }));
    }

    #[test]
    fn decode_binary_port_a_state() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        assert!(decoder
            .feed(VIA_TYPE_PORT_STATE).is_none());
        let message = decoder.feed(0x55);
        assert_eq!(message, Some(ViaProtocolMessage::PortState { port: b'A', port_state: 0x55 }));
    }

    #[test]
    fn decode_binary_port_b_state() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        assert!(decoder
            .feed(VIA_TYPE_PORT_STATE | VIA_PORT_MASK).is_none());
        let message = decoder.feed(0x55);
        assert_eq!(message, Some(ViaProtocolMessage::PortState { port: b'B', port_state: 0x55 }));
    }

    #[test]
    fn decode_binary_port_a_ctrl_state() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        let message = decoder
            .feed(VIA_TYPE_CTRL_STATE | VIA_CTRL1_MASK | VIA_CTRL2_MASK);
        assert_eq!(message, Some(ViaProtocolMessage::CtrlState {
            port: b'A', ctrl_state: VIA_CTRL1_MASK | VIA_CTRL2_MASK}));
    }

    #[test]
    fn decode_binary_port_b_ctrl_state() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        let message = decoder
            .feed(VIA_TYPE_CTRL_STATE | VIA_PORT_MASK | VIA_CTRL2_MASK);
        assert_eq!(message, Some(ViaProtocolMessage::CtrlState {
            port: b'B', ctrl_state: VIA_CTRL2_MASK}));
    }

    #[test]
    fn decode_binary_port_a_reset() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        assert!(decoder
            .feed(VIA_TYPE_RESET_PORT | VIA_CTRL1_MASK | VIA_CTRL2_MASK).is_none());
        let message = decoder.feed(0x55);
        assert_eq!(message, Some(ViaProtocolMessage::ResetPort {
            port: b'A', port_mask: 0x55, ctrl_mask: VIA_CTRL1_MASK | VIA_CTRL2_MASK}));
    }

    #[test]
    fn decode_binary_port_b_reset() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        assert!(decoder
            .feed(VIA_TYPE_RESET_PORT | VIA_PORT_MASK | VIA_CTRL1_MASK).is_none());
        let message = decoder.feed(0x55);
        assert_eq!(message, Some(ViaProtocolMessage::ResetPort {
            port: b'B', port_mask: 0x55, ctrl_mask: VIA_CTRL1_MASK}));
    }

    #[test]
    fn decode_binary_port_a_set() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        assert!(decoder
            .feed(VIA_TYPE_SET_PORT | VIA_CTRL1_MASK | VIA_CTRL2_MASK).is_none());
        let message = decoder.feed(0xAA);
        assert_eq!(message, Some(ViaProtocolMessage::SetPort {
            port: b'A', port_mask: 0xAA, ctrl_mask: VIA_CTRL1_MASK | VIA_CTRL2_MASK}));
    }

    #[test]
    fn decode_binary_port_b_set() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        assert!(decoder
            .feed(VIA_TYPE_SET_PORT | VIA_PORT_MASK | VIA_CTRL2_MASK).is_none());
        let message = decoder.feed(0xAA);
        assert_eq!(message, Some(ViaProtocolMessage::SetPort {
            port: b'B', port_mask: 0xAA, ctrl_mask: VIA_CTRL2_MASK}));
    }

    #[test]
    fn decode_binary_port_a_ctrl_reset() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        let message = decoder
            .feed(VIA_TYPE_RESET_CTRL | VIA_CTRL1_MASK | VIA_CTRL2_MASK);
        assert_eq!(message, Some(ViaProtocolMessage::ResetCtrl {
            port: b'A', ctrl_mask: VIA_CTRL1_MASK | VIA_CTRL2_MASK}));
    }

    #[test]
    fn decode_binary_port_b_ctrl_reset() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        let message = decoder
            .feed(VIA_TYPE_RESET_CTRL | VIA_PORT_MASK | VIA_CTRL2_MASK);
        assert_eq!(message, Some(ViaProtocolMessage::ResetCtrl {
            port: b'B', ctrl_mask: VIA_CTRL2_MASK}));
    }

    #[test]
    fn decode_binary_port_a_ctrl_set() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        let message = decoder
            .feed(VIA_TYPE_SET_CTRL | VIA_CTRL1_MASK | VIA_CTRL2_MASK);
        assert_eq!(message, Some(ViaProtocolMessage::SetCtrl {
            port: b'A', ctrl_mask: VIA_CTRL1_MASK | VIA_CTRL2_MASK}));
    }

    #[test]
    fn decode_binary_port_b_ctrl_set() {
        let mut decoder = ViaBinaryProtocolDecoder::new();
        let message = decoder
            .feed(VIA_TYPE_SET_CTRL | VIA_PORT_MASK | VIA_CTRL2_MASK);
        assert_eq!(message, Some(ViaProtocolMessage::SetCtrl {
            port: b'B', ctrl_mask: VIA_CTRL2_MASK}));
    }

}
