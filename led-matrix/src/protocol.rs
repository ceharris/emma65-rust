//! Pure decode side of the `LedMatrix` external protocol (`doc/led-matrix-external-protocol.md`).
//!
//! The encode side lives in `emma65::emulator::device::led_matrix` (private to that crate, since
//! only `LedMatrix` itself ever encodes); this binary only ever decodes, so it gets its own small
//! mirror rather than reaching into a private module — same split as `display/src/protocol.rs`
//! mirrors `display::protocol`. Kept separate from `main.rs` so the wire-format logic is
//! unit-testable independent of stdin/SDL2.
//!
//! Unlike `display/src/protocol.rs`'s single fixed-size frame, messages here are tagged (spec
//! §3): a one-byte tag determines a fixed body length, with no length prefix anywhere. Callers
//! read the tag byte first, then [`body_len`] tells them how many more bytes to read before
//! calling [`decode_message`].

use emma65::emulator::device::led_matrix::PIXELS_PER_MATRIX;
use emma65::emulator::device::led_matrix::compositing::Rgb565;

const MAGIC: [u8; 4] = *b"E65M";
const SUPPORTED_VERSION: u8 = 1;

/// Tag identifying a block message (spec §5.1).
pub const MSG_BLOCK: u8 = 1;
/// Tag identifying a palette message (spec §5.2).
pub const MSG_PALETTE: u8 = 2;

/// Fixed size of the one-time header (spec §4): magic + version + matrix_count + frame_rate_hz.
pub const HEADER_LEN: usize = 4 + 1 + 1 + 4;

/// Size of a block message's body, *after* the already-consumed tag byte (spec §5.1):
/// matrix_index + raw pixels.
const BLOCK_BODY_LEN: usize = 1 + PIXELS_PER_MATRIX;

/// Size of a palette message's body, *after* the already-consumed tag byte (spec §5.2):
/// index + packed RGB565.
const PALETTE_BODY_LEN: usize = 1 + 2;

/// The one-time header, decoded (spec §4). No palette or per-matrix dimensions — both sides
/// already know matrix dimensions are a fixed 32x32 constant, and the palette is never
/// transferred at connection time (spec §7).
#[derive(Debug)]
pub struct Header {
    pub matrix_count: u8,
    pub frame_rate_hz: u32,
}

/// Decodes the header from exactly [`HEADER_LEN`] bytes. Refuses to proceed on a magic mismatch
/// or an unrecognized version, rather than trying to synthesize a fallback — same policy as
/// `display/src/protocol.rs::decode_header`.
pub fn decode_header(bytes: &[u8]) -> Result<Header, String> {
    if bytes.len() != HEADER_LEN {
        return Err(format!("header must be exactly {HEADER_LEN} bytes, got {}", bytes.len()));
    }
    if bytes[0..4] != MAGIC {
        return Err(format!("bad magic {:?}, expected {:?}", &bytes[0..4], MAGIC));
    }
    let version = bytes[4];
    if version != SUPPORTED_VERSION {
        return Err(format!("unsupported protocol version {version}, expected {SUPPORTED_VERSION}"));
    }
    let matrix_count = bytes[5];
    let frame_rate_hz = u32::from_le_bytes(bytes[6..10].try_into().unwrap());
    Ok(Header { matrix_count, frame_rate_hz })
}

/// A decoded post-header message (spec §5).
#[derive(Debug)]
pub enum Message {
    /// One matrix's raw palette-index pixels, sent on every swap (spec §5.1).
    Block { matrix_index: u8, pixels: Vec<u8> },
    /// One palette entry's new value, sent only on an actual `CMD_PALETTE_WRITE` (spec §5.2).
    Palette { index: u8, color: Rgb565 },
}

/// Returns the number of body bytes that must follow `tag` before [`decode_message`] can be
/// called, or `None` for an unrecognized tag — a protocol desync the caller should treat as
/// fatal (spec §3: there is no way to resynchronize the stream).
pub fn body_len(tag: u8) -> Option<usize> {
    match tag {
        MSG_BLOCK => Some(BLOCK_BODY_LEN),
        MSG_PALETTE => Some(PALETTE_BODY_LEN),
        _ => None,
    }
}

/// Decodes one message from its tag and exactly `body_len(tag)` further bytes.
pub fn decode_message(tag: u8, body: &[u8]) -> Result<Message, String> {
    match tag {
        MSG_BLOCK => {
            if body.len() != BLOCK_BODY_LEN {
                return Err(format!("block body must be exactly {BLOCK_BODY_LEN} bytes, got {}", body.len()));
            }
            Ok(Message::Block { matrix_index: body[0], pixels: body[1..].to_vec() })
        }
        MSG_PALETTE => {
            if body.len() != PALETTE_BODY_LEN {
                return Err(format!("palette body must be exactly {PALETTE_BODY_LEN} bytes, got {}", body.len()));
            }
            let index = body[0];
            let packed = u16::from_le_bytes(body[1..3].try_into().unwrap());
            let r5 = ((packed >> 11) & 0x1F) as u8;
            let g6 = ((packed >> 5) & 0x3F) as u8;
            let b5 = (packed & 0x1F) as u8;
            Ok(Message::Palette { index, color: Rgb565::new(r5, g6, b5) })
        }
        _ => Err(format!("unrecognized message tag {tag}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header_bytes(matrix_count: u8, frame_rate_hz: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_LEN);
        buf.extend_from_slice(&MAGIC);
        buf.push(SUPPORTED_VERSION);
        buf.push(matrix_count);
        buf.extend_from_slice(&frame_rate_hz.to_le_bytes());
        buf
    }

    #[test]
    fn decode_header_round_trips_fields() {
        let header = decode_header(&sample_header_bytes(4, 100)).unwrap();
        assert_eq!(header.matrix_count, 4);
        assert_eq!(header.frame_rate_hz, 100);
    }

    #[test]
    fn decode_header_rejects_wrong_length() {
        let err = decode_header(&sample_header_bytes(4, 100)[..HEADER_LEN - 1]).unwrap_err();
        assert!(err.contains("exactly"));
    }

    #[test]
    fn decode_header_rejects_bad_magic() {
        let mut bytes = sample_header_bytes(4, 100);
        bytes[0] = b'X';
        let err = decode_header(&bytes).unwrap_err();
        assert!(err.contains("magic"));
    }

    #[test]
    fn decode_header_rejects_unsupported_version() {
        let mut bytes = sample_header_bytes(4, 100);
        bytes[4] = 99;
        let err = decode_header(&bytes).unwrap_err();
        assert!(err.contains("version"));
    }

    #[test]
    fn body_len_matches_spec_for_known_tags() {
        assert_eq!(body_len(MSG_BLOCK), Some(1 + PIXELS_PER_MATRIX));
        assert_eq!(body_len(MSG_PALETTE), Some(3));
    }

    #[test]
    fn body_len_is_none_for_unknown_tag() {
        assert_eq!(body_len(0), None);
        assert_eq!(body_len(3), None);
    }

    #[test]
    fn decode_message_block_splits_index_and_pixels() {
        let mut body = vec![7u8]; // matrix_index
        body.extend(std::iter::repeat_n(9u8, PIXELS_PER_MATRIX));

        match decode_message(MSG_BLOCK, &body).unwrap() {
            Message::Block { matrix_index, pixels } => {
                assert_eq!(matrix_index, 7);
                assert_eq!(pixels.len(), PIXELS_PER_MATRIX);
                assert!(pixels.iter().all(|&p| p == 9));
            }
            _ => panic!("expected Block"),
        }
    }

    #[test]
    fn decode_message_block_rejects_wrong_body_length() {
        let err = decode_message(MSG_BLOCK, &[0u8; 5]).unwrap_err();
        assert!(err.contains("exactly"));
    }

    #[test]
    fn decode_message_palette_unpacks_rgb565_channels() {
        let color = Rgb565::new(0x1F, 0x3F, 0x00); // max red, max green, no blue
        let packed = color.to_packed565();
        let mut body = vec![9u8]; // index
        body.extend_from_slice(&packed.to_le_bytes());

        match decode_message(MSG_PALETTE, &body).unwrap() {
            Message::Palette { index, color: decoded } => {
                assert_eq!(index, 9);
                assert_eq!(decoded, color);
            }
            _ => panic!("expected Palette"),
        }
    }

    #[test]
    fn decode_message_palette_rejects_wrong_body_length() {
        let err = decode_message(MSG_PALETTE, &[0u8; 2]).unwrap_err();
        assert!(err.contains("exactly"));
    }

    #[test]
    fn decode_message_rejects_unknown_tag() {
        let err = decode_message(99, &[]).unwrap_err();
        assert!(err.contains("unrecognized"));
    }
}
