//! Peripheral protocols and related support.
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::str::FromStr;

pub mod via;
pub mod ptm;
pub(crate) mod manager;

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

impl From<ProtocolMessageEncoding> for String {
    fn from(v: ProtocolMessageEncoding) -> Self {
        v.to_string()
    }
}

/// A message protocol encoder.
pub trait ProtocolMessageEncoder<T>: Send {

    /// Encodes `message` appending the encoded form to `out`.
    fn encode(&mut self, message: &T, out: &mut Vec<u8>);

}

type EncoderSupplier<T> = fn(encoding: ProtocolMessageEncoding) -> Box<dyn ProtocolMessageEncoder<T>>;
type DecoderSupplier<T> = fn(encoding: ProtocolMessageEncoding) -> Box<dyn ProtocolMessageDecoder<T>>;

/// A message protocol decoder.
pub trait ProtocolMessageDecoder<T>: Send {

    /// Feeds the byte `b` received from the transport into the decoder's state machine.
    /// Returns `Some(T)` if the state machine outputs a valid message, otherwise `None`.
    fn feed(&mut self, b: u8) -> Option<T>;

}