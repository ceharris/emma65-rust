//! Pure decode side of the `CharDisplay` external protocol (`doc/char-display-external-protocol.md`).
//!
//! The encode side lives in `emma65::emulator::device::display` (private to that crate, since
//! only `CharDisplay` itself ever encodes); this binary only ever decodes, so it gets its own
//! small mirror rather than reaching into a private module. Kept separate from `main.rs` so the
//! wire-format logic is unit-testable independent of stdin/SDL2.

use emma65::emulator::device::display::compositing::Rgb24;
use emma65::emulator::device::display::font::{FONT_BYTES, Font};

const MAGIC: [u8; 4] = *b"E65D";
const SUPPORTED_VERSION: u8 = 1;

/// Fixed size of the one-time header (spec §4): magic + version + columns + rows +
/// frame_rate_hz + palette_len + the raw font bytes.
pub const HEADER_LEN: usize = 4 + 1 + 4 + 4 + 4 + 2 + FONT_BYTES;

/// The one-time header, decoded (spec §4).
#[derive(Debug)]
pub struct Header {
    pub columns: u32,
    pub rows: u32,
    pub frame_rate_hz: u32,
    pub palette_len: u16,
    pub font: Font,
}

impl Header {
    pub fn cells(&self) -> usize {
        self.columns as usize * self.rows as usize
    }

    /// Size in bytes of every subsequent frame message (spec §5), fixed for the life of the
    /// connection once the header is known.
    pub fn frame_len(&self) -> usize {
        2 * self.cells() + 3 * self.palette_len as usize
    }
}

/// Decodes the header from exactly [`HEADER_LEN`] bytes. Refuses to proceed on a magic
/// mismatch or an unrecognized version (spec §7: "a peripheral that doesn't recognize a
/// header's version should refuse to proceed rather than guess at a compatible framing"),
/// rather than trying to synthesize a fallback.
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
    let columns = u32::from_le_bytes(bytes[5..9].try_into().unwrap());
    let rows = u32::from_le_bytes(bytes[9..13].try_into().unwrap());
    let frame_rate_hz = u32::from_le_bytes(bytes[13..17].try_into().unwrap());
    let palette_len = u16::from_le_bytes(bytes[17..19].try_into().unwrap());
    let font = Font::from_bytes(&bytes[19..19 + FONT_BYTES]).map_err(|e| e.to_string())?;
    Ok(Header { columns, rows, frame_rate_hz, palette_len, font })
}

/// Decoded frame contents (spec §5): char RAM, color RAM, and the current palette.
pub struct Frame {
    pub char_ram: Vec<u8>,
    pub color_ram: Vec<u8>,
    pub palette: Vec<Rgb24>,
}

/// Decodes one frame message from exactly `2 * columns * rows + 3 * palette_len` bytes (spec
/// §5) — the palette length isn't needed explicitly since it falls out of `bytes`' remaining
/// length after the char/color RAM.
pub fn decode_frame(bytes: &[u8], columns: u32, rows: u32) -> Frame {
    let cells = columns as usize * rows as usize;
    let char_ram = bytes[0..cells].to_vec();
    let color_ram = bytes[cells..2 * cells].to_vec();
    let palette = bytes[2 * cells..]
        .chunks_exact(3)
        .map(|c| Rgb24::new(c[0], c[1], c[2]))
        .collect();
    Frame { char_ram, color_ram, palette }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header_bytes(columns: u32, rows: u32, frame_rate_hz: u32, palette_len: u16) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_LEN);
        buf.extend_from_slice(&MAGIC);
        buf.push(SUPPORTED_VERSION);
        buf.extend_from_slice(&columns.to_le_bytes());
        buf.extend_from_slice(&rows.to_le_bytes());
        buf.extend_from_slice(&frame_rate_hz.to_le_bytes());
        buf.extend_from_slice(&palette_len.to_le_bytes());
        buf.extend_from_slice(Font::default().as_bytes());
        buf
    }

    #[test]
    fn decode_header_round_trips_fields() {
        let bytes = sample_header_bytes(40, 25, 60, 16);
        let header = decode_header(&bytes).unwrap();
        assert_eq!(header.columns, 40);
        assert_eq!(header.rows, 25);
        assert_eq!(header.frame_rate_hz, 60);
        assert_eq!(header.palette_len, 16);
        assert_eq!(header.font.as_bytes(), Font::default().as_bytes());
    }

    #[test]
    fn decode_header_rejects_wrong_length() {
        let err = decode_header(&sample_header_bytes(40, 25, 60, 16)[..HEADER_LEN - 1]).unwrap_err();
        assert!(err.contains("exactly"));
    }

    #[test]
    fn decode_header_rejects_bad_magic() {
        let mut bytes = sample_header_bytes(40, 25, 60, 16);
        bytes[0] = b'X';
        let err = decode_header(&bytes).unwrap_err();
        assert!(err.contains("magic"));
    }

    #[test]
    fn decode_header_rejects_unsupported_version() {
        let mut bytes = sample_header_bytes(40, 25, 60, 16);
        bytes[4] = 99;
        let err = decode_header(&bytes).unwrap_err();
        assert!(err.contains("version"));
    }

    #[test]
    fn frame_len_matches_spec_formula() {
        let header = decode_header(&sample_header_bytes(2, 3, 60, 4)).unwrap();
        assert_eq!(header.frame_len(), 2 * (2 * 3) + 3 * 4);
    }

    #[test]
    fn decode_frame_splits_char_color_and_palette() {
        let frame_bytes = [0x41, 0x42, 1u8, 2u8, 10, 20, 30, 40, 50, 60];
        let frame = decode_frame(&frame_bytes, 2, 1);
        assert_eq!(frame.char_ram, vec![0x41, 0x42]);
        assert_eq!(frame.color_ram, vec![1, 2]);
        assert_eq!(frame.palette, vec![Rgb24::new(10, 20, 30), Rgb24::new(40, 50, 60)]);
    }
}
