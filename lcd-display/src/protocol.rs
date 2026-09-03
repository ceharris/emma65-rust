//! Pure decode side of the `LcdDisplay` external protocol (`plan/lcd-display-external-protocol.md`).
//!
//! The encode side lives in `emma65::emulator::device::lcd_display` (private to that crate, since
//! only `LcdDisplay` itself ever encodes); this binary only ever decodes, so it gets its own small
//! mirror rather than reaching into a private module — same split as `display/src/protocol.rs`
//! mirrors `display::protocol` and `led-matrix/src/protocol.rs` mirrors `led_matrix`'s.
//!
//! Unlike `display/src/protocol.rs`'s single fixed-size frame, a frame here (spec §5) carries its
//! own `width_px`/`height_px` rather than a size fixed by the header, since the active font can
//! change a frame's pixel height at runtime — so decoding a frame is a two-step read (the 4-byte
//! dimension prefix, then the now-known-length pixel payload) rather than one fixed-size read.

use emma65::emulator::device::lcd_display::compositing::Rgb24;

const MAGIC: [u8; 4] = *b"E65L";
const SUPPORTED_VERSION: u8 = 1;

/// Fixed size of the one-time header (spec §4): magic + version + columns + rows + background +
/// foreground.
pub const HEADER_LEN: usize = 4 + 1 + 1 + 1 + 3 + 3;

/// Size of a frame message's dimension prefix (spec §5), read before the pixel payload's own
/// length is known.
pub const FRAME_DIMENSIONS_LEN: usize = 2 + 2;

/// The one-time header, decoded (spec §4).
#[derive(Debug)]
pub struct Header {
    pub columns: u8,
    pub rows: u8,
    pub background: Rgb24,
    pub foreground: Rgb24,
}

/// Decodes the header from exactly [`HEADER_LEN`] bytes. Refuses to proceed on a magic mismatch
/// or an unrecognized version (spec §8: "a peripheral that doesn't recognize a header's version
/// should refuse to proceed rather than guess at a compatible framing"), same policy as
/// `display/src/protocol.rs::decode_header` and `led-matrix/src/protocol.rs::decode_header`.
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
    let columns = bytes[5];
    let rows = bytes[6];
    let background = Rgb24::new(bytes[7], bytes[8], bytes[9]);
    let foreground = Rgb24::new(bytes[10], bytes[11], bytes[12]);
    Ok(Header { columns, rows, background, foreground })
}

/// A frame message's dimensions (spec §5), decoded from [`FRAME_DIMENSIONS_LEN`] bytes — read
/// first so the caller knows how many further bytes to read for the pixel payload.
pub fn decode_frame_dimensions(bytes: &[u8]) -> (u16, u16) {
    let width_px = u16::from_le_bytes(bytes[0..2].try_into().unwrap());
    let height_px = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
    (width_px, height_px)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header_bytes(columns: u8, rows: u8, background: Rgb24, foreground: Rgb24) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_LEN);
        buf.extend_from_slice(&MAGIC);
        buf.push(SUPPORTED_VERSION);
        buf.push(columns);
        buf.push(rows);
        buf.extend_from_slice(&[background.r, background.g, background.b]);
        buf.extend_from_slice(&[foreground.r, foreground.g, foreground.b]);
        buf
    }

    #[test]
    fn decode_header_round_trips_fields() {
        let bg = Rgb24::new(0, 0, 0xAA);
        let fg = Rgb24::new(0xFF, 0xFF, 0xFF);
        let header = decode_header(&sample_header_bytes(16, 2, bg, fg)).unwrap();
        assert_eq!(header.columns, 16);
        assert_eq!(header.rows, 2);
        assert_eq!(header.background, bg);
        assert_eq!(header.foreground, fg);
    }

    #[test]
    fn decode_header_rejects_wrong_length() {
        let bytes = sample_header_bytes(16, 2, Rgb24::new(0, 0, 0), Rgb24::new(255, 255, 255));
        let err = decode_header(&bytes[..HEADER_LEN - 1]).unwrap_err();
        assert!(err.contains("exactly"));
    }

    #[test]
    fn decode_header_rejects_bad_magic() {
        let mut bytes = sample_header_bytes(16, 2, Rgb24::new(0, 0, 0), Rgb24::new(255, 255, 255));
        bytes[0] = b'X';
        let err = decode_header(&bytes).unwrap_err();
        assert!(err.contains("magic"));
    }

    #[test]
    fn decode_header_rejects_unsupported_version() {
        let mut bytes = sample_header_bytes(16, 2, Rgb24::new(0, 0, 0), Rgb24::new(255, 255, 255));
        bytes[4] = 99;
        let err = decode_header(&bytes).unwrap_err();
        assert!(err.contains("version"));
    }

    #[test]
    fn decode_frame_dimensions_round_trips() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&80u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        assert_eq!(decode_frame_dimensions(&bytes), (80, 16));
    }
}
