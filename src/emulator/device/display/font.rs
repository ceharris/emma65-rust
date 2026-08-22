//! An 8×8, 1bpp, 256-glyph bitmap font used by the display's compositing path
//! (design doc §7).
//!
//! Each glyph is 8 bytes, one byte per row, indexed top-to-bottom; within a row byte, bit `n`
//! (from the least-significant bit) is the pixel at column `n` (leftmost column is bit 0). The
//! font is indexed by the same byte value stored in a device's character RAM, so glyph `0x41`
//! is whatever byte `0x41` in char RAM should render as -- there is no other indirection.

use std::fmt;

/// Number of glyphs in a font (one per possible character-RAM byte value).
pub const GLYPH_COUNT: usize = 256;
/// Bytes per glyph: one row per byte, 8 rows.
pub const GLYPH_BYTES: usize = 8;
/// Total size in bytes of a font's raw bitmap data.
pub const FONT_BYTES: usize = GLYPH_COUNT * GLYPH_BYTES;

/// Raw bytes of the bundled default font, assembled from `font8x8` by Daniel Hepper
/// (<https://github.com/dhepper/font8x8>, public domain; in turn derived from Marcel
/// Sondaar's `font8_8.asm`, itself derived from IBM's public-domain VGA font set -- see
/// design doc §7 for why this source was chosen over the C64 chargen ROM).
///
/// `font8x8` splits glyph coverage across several headers rather than shipping one 256-entry
/// table. This default combines three of them into a single indexed-by-byte-value table:
///
/// - `0x00..=0x7F` -- `font8x8_basic` (`U+0000..=U+007F`), unchanged: byte value already equals
///   the Unicode code point for this range.
/// - `0x80..=0x9F` -- the first 32 entries of `font8x8_box` (`U+2500..`, line/box-drawing
///   glyphs). `font8x8_ext_latin` leaves this range uncovered (it starts at `U+00A0`), and
///   Latin-1 reserves `0x80..=0x9F` for C1 control codes that have no meaningful glyph anyway,
///   so it is repurposed here for a small set of box-drawing characters.
/// - `0xA0..=0xFF` -- `font8x8_ext_latin` (`U+00A0..=U+00FF`), unchanged: byte value already
///   equals the Unicode code point for this range too.
///
/// `font8x8_box`, `font8x8_greek`, `font8x8_hiragana`, etc. beyond that first 32-glyph slice are
/// not included -- there is no room left in a single byte-indexed table.
const DEFAULT_FONT_BYTES: &[u8; FONT_BYTES] = include_bytes!("default_font.bin");

/// An error indicating that raw font data was not exactly [`FONT_BYTES`] long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontError {
    pub actual_len: usize,
}

impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "font data must be exactly {FONT_BYTES} bytes ({GLYPH_COUNT} glyphs of {GLYPH_BYTES} bytes each), got {}", self.actual_len)
    }
}

impl std::error::Error for FontError {}

/// An 8×8, 1bpp, 256-glyph bitmap font.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Font {
    data: Box<[u8; FONT_BYTES]>,
}

impl Font {
    /// Builds a font from raw bytes. `data` must be exactly [`FONT_BYTES`] long.
    pub fn from_bytes(data: &[u8]) -> Result<Self, FontError> {
        let array: [u8; FONT_BYTES] = data.try_into().map_err(|_| FontError { actual_len: data.len() })?;
        Ok(Self { data: Box::new(array) })
    }

    /// Returns the 8 row bytes for `glyph_index` (one of the 256 possible character-RAM byte
    /// values).
    pub fn glyph(&self, glyph_index: u8) -> &[u8; GLYPH_BYTES] {
        let start = glyph_index as usize * GLYPH_BYTES;
        self.data[start..start + GLYPH_BYTES].try_into().expect("glyph slice is always GLYPH_BYTES long")
    }
}

impl Default for Font {
    /// The bundled default font (see [`DEFAULT_FONT_BYTES`]).
    fn default() -> Self {
        Self::from_bytes(DEFAULT_FONT_BYTES).expect("bundled default font must be well-formed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_rejects_wrong_length() {
        let err = Font::from_bytes(&[0u8; FONT_BYTES - 1]).unwrap_err();
        assert_eq!(err.actual_len, FONT_BYTES - 1);
    }

    #[test]
    fn from_bytes_accepts_exact_length() {
        let data = vec![0u8; FONT_BYTES];
        assert!(Font::from_bytes(&data).is_ok());
    }

    #[test]
    fn glyph_reads_correct_slice() {
        let mut data = vec![0u8; FONT_BYTES];
        data[8 * 0x41..8 * 0x41 + 8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let font = Font::from_bytes(&data).unwrap();
        assert_eq!(font.glyph(0x41), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(font.glyph(0x00), &[0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn default_font_is_well_formed_and_ascii_covers_printable_range() {
        let font = Font::default();
        // 'A' (0x41) in font8x8_basic is a well-known non-blank glyph; a blank glyph here
        // would indicate the wrong header (or wrong slice of it) was embedded.
        assert_ne!(font.glyph(0x41), &[0u8; 8]);
        // NUL (0x00) is blank in font8x8_basic.
        assert_eq!(font.glyph(0x00), &[0u8; 8]);
    }

    #[test]
    fn default_font_box_drawing_range_is_populated() {
        let font = Font::default();
        // 0x80 is the first glyph borrowed from font8x8_box (a thin horizontal line);
        // it must not be blank, unlike the Latin-1 C1 control codes it replaces.
        assert_ne!(font.glyph(0x80), &[0u8; 8]);
    }
}
