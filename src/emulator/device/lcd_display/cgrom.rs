//! The HD44780 character generator ROM (CGROM): a fixed table mapping a DDRAM byte value to a
//! 5×8 glyph's 8 row bytes (spec §8.1). See `compositing::glyph_rows` for how a DDRAM byte
//! (including the `0x00..=0x0F` CGRAM-alias range, which never indexes this table) resolves to
//! the rows actually drawn, and how a 5×10-font cell extends an 8-row CGROM glyph to 10 rows.

use std::fmt;

/// Rows stored per glyph in this table -- always 8, regardless of the active font (design doc
/// §5): CGROM characters are fixed 5×8 glyphs, and a 5×10-font cell pads them with two blank
/// rows rather than the table itself carrying a font-dependent shape.
pub const ROWS_PER_GLYPH: usize = 8;
/// Number of glyphs: one per possible DDRAM byte value.
pub const GLYPH_COUNT: usize = 256;
/// Total size in bytes of a table's raw data.
pub const CGROM_BYTES: usize = GLYPH_COUNT * ROWS_PER_GLYPH;

/// Raw bytes of the bundled "A00" (Japanese standard font) table: the Hitachi HD44780 ROM code
/// A00 character ROM (spec §8.1.1), each row byte holding a 5-bit pixel pattern (bit 4 leftmost,
/// matching spec §8.2) in its low bits. Sourced from the `_jp` font table in `char-lcd.js`
/// (<https://github.com/jazz-soft/char-lcd>, MIT licensed) -- the library's own default ROM,
/// assembled from the published HD44780U datasheet's character font table, the same kind of
/// real-hardware-derived, permissively licensed source `display::font`'s default font and
/// [`A02_CGROM_BYTES`] use. Unlike that source's `_eu` (A02) arrays, `_jp`'s rows are already
/// top-aligned with no leading blank row, so each glyph here is copied through unshifted: rows
/// 0-7 direct from the source array (padding a short array with blanks), confirmed by
/// cross-checking glyphs shared with `_eu` (e.g. `-`, `g`) landing on identical rows under each
/// source's own convention. Unlike [`A02_CGROM_BYTES`], row 7 is *not* forced blank here: eight
/// descender glyphs (`beta, mu, ro, g, j, p, q, y` at `0xE2,E4,E6,E7,EA,F0,F1,F9`) plus the solid
/// block at `0xFF` genuinely use it on real hardware (verified against the datasheet's printed
/// character table), so only each of those arrays' 9th/10th elements -- meant for 5x10-font mode
/// only -- are dropped, since this table -- like `A02_CGROM_BYTES` -- always stores the 5x8 shape
/// (see `ROWS_PER_GLYPH`). `0x00..=0x1F` and `0x80..=0x9F` are blank in the source already,
/// matching spec §8.1's documented gaps in the standard ROM table.
const A00_CGROM_BYTES: &[u8; CGROM_BYTES] = include_bytes!("default_cgrom_a00.bin");

/// Raw bytes of the bundled "A02" (European standard font) table: the Hitachi HD44780 ROM code
/// A02 character ROM (spec §8.1.1), each row byte holding a 5-bit pixel pattern (bit 4 leftmost,
/// matching spec §8.2) in its low bits. Sourced from the `_eu` font table in `char-lcd.js`
/// (<https://github.com/jazz-soft/char-lcd>, MIT licensed), itself assembled from the published
/// HD44780U datasheet's character font table -- the same kind of real-hardware-derived,
/// permissively licensed source `display::font`'s default font uses. `0x00..=0x1F` and
/// `0x80..=0x9F` are forced blank here (rather than keeping that source's extra glyphs in those
/// slots) to match spec §8.1's documented gaps in the standard ROM table exactly. Every glyph is
/// also shifted up by one row relative to `char-lcd.js`'s own arrays, whose row 0 is the blank
/// row: the HD44780U datasheet (Table 5) documents row 7 (the 8th line) as the reserved cursor
/// row instead, so each glyph here drops that source's row 0 and appends a blank row 7.
const A02_CGROM_BYTES: &[u8; CGROM_BYTES] = include_bytes!("default_cgrom_a02.bin");

/// An error indicating that raw CGROM data was not exactly [`CGROM_BYTES`] long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgRomError {
    pub actual_len: usize,
}

impl fmt::Display for CgRomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CGROM data must be exactly {CGROM_BYTES} bytes ({GLYPH_COUNT} glyphs of {ROWS_PER_GLYPH} bytes each), got {}",
            self.actual_len
        )
    }
}

impl std::error::Error for CgRomError {}

/// A fixed, 256-glyph HD44780 character generator ROM table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgRom {
    data: Box<[u8; CGROM_BYTES]>,
}

impl CgRom {
    /// Builds a table from raw bytes (the `cgrom=` config attribute's file format, spec §3).
    /// `data` must be exactly [`CGROM_BYTES`] long.
    pub fn from_bytes(data: &[u8]) -> Result<Self, CgRomError> {
        let array: [u8; CGROM_BYTES] = data.try_into().map_err(|_| CgRomError { actual_len: data.len() })?;
        Ok(Self { data: Box::new(array) })
    }

    /// Returns the 8 row bytes for `index` (the DDRAM byte value being resolved, spec §8.1).
    /// Meaningless for `0x00..=0x0F`, which never reach this table (they select a CGRAM
    /// character instead, per `compositing::glyph_rows`); callers still get *some* array back
    /// for that range rather than a panic, since the table itself has no way to reject an
    /// index.
    pub fn glyph(&self, index: u8) -> &[u8; ROWS_PER_GLYPH] {
        let start = index as usize * ROWS_PER_GLYPH;
        self.data[start..start + ROWS_PER_GLYPH].try_into().expect("glyph slice is always ROWS_PER_GLYPH long")
    }

    /// Returns the raw, `CGROM_BYTES`-long table data (all 256 glyphs, concatenated).
    pub fn as_bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// The bundled "A00" (Japanese standard font) table (see [`A00_CGROM_BYTES`]) -- the ROM
    /// code most HD44780 clones ship with, hence [`Default`] returning it.
    pub fn a00() -> Self {
        Self::from_bytes(A00_CGROM_BYTES).expect("bundled A00 CGROM must be well-formed")
    }

    /// The bundled "A02" (European standard font) table (see [`A02_CGROM_BYTES`]).
    pub fn a02() -> Self {
        Self::from_bytes(A02_CGROM_BYTES).expect("bundled A02 CGROM must be well-formed")
    }
}

impl Default for CgRom {
    /// The bundled "A00" table (see [`CgRom::a00`]).
    fn default() -> Self {
        Self::a00()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_rejects_wrong_length() {
        let err = CgRom::from_bytes(&[0u8; CGROM_BYTES - 1]).unwrap_err();
        assert_eq!(err.actual_len, CGROM_BYTES - 1);
    }

    #[test]
    fn from_bytes_accepts_exact_length() {
        let data = vec![0u8; CGROM_BYTES];
        assert!(CgRom::from_bytes(&data).is_ok());
    }

    #[test]
    fn glyph_reads_correct_slice() {
        let mut data = vec![0u8; CGROM_BYTES];
        data[8 * 0x41..8 * 0x41 + 8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let table = CgRom::from_bytes(&data).unwrap();
        assert_eq!(table.glyph(0x41), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(table.glyph(0x00), &[0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn default_table_is_a00() {
        // A00 is the ROM code most HD44780 clones ship with.
        assert_eq!(CgRom::default(), CgRom::a00());
    }

    #[test]
    fn a00_table_renders_a_as_a_recognizable_letter_a() {
        // 0x41 ('A'): flat top spanning three columns, widening into two vertical strokes all the
        // way down, blank cursor row at the bottom -- spot-checked against the sourced A00 table
        // rather than merely asserting non-blank, so a transposed or shifted table would fail
        // this test. A00's 'A' is flat-topped rather than A02's pointed-apex 'A' -- both are
        // correct, since the two ROM codes are genuinely different pixel masks even for glyphs
        // they share, per the real HD44780U datasheet.
        let table = CgRom::a00();
        assert_eq!(table.glyph(0x41), &[14, 17, 17, 31, 17, 17, 17, 0]);
    }

    #[test]
    fn a00_table_yen_sign_replaces_backslash() {
        // 0x5C: A00 substitutes a Yen sign for ASCII backslash -- a ROM-code-identifying
        // difference from A02, which keeps a literal backslash there.
        let table = CgRom::a00();
        assert_eq!(table.glyph(0x5C), &[17, 10, 31, 4, 31, 4, 4, 0]);
    }

    #[test]
    fn a00_table_descender_glyphs_populate_the_cursor_row() {
        // Unlike A02, A00 has 8 glyphs (plus the solid block) whose descender genuinely reaches
        // row 7 on real hardware, verified against the datasheet's printed character table --
        // row 7 must carry that pixel data rather than being forced blank like every other glyph.
        let table = CgRom::a00();
        assert_eq!(table.glyph(0xE2)[7], 0b10000, "beta"); // column 0
        assert_eq!(table.glyph(0xE4)[7], 0b10000, "mu"); // column 0
        assert_eq!(table.glyph(0xE6)[7], 0b10000, "ro"); // column 0
        assert_eq!(table.glyph(0xE7)[7], 0b00001, "g"); // column 4
        assert_eq!(table.glyph(0xEA)[7], 0b00010, "j"); // column 3
        assert_eq!(table.glyph(0xF0)[7], 0b10000, "p"); // column 0
        assert_eq!(table.glyph(0xF1)[7], 0b00001, "q"); // column 4
        assert_eq!(table.glyph(0xF9)[7], 0b00001, "y"); // column 4
        assert_eq!(table.glyph(0xFF)[7], 0b11111, "solid block"); // all columns
    }

    #[test]
    fn a00_table_ascii_space_is_blank() {
        let table = CgRom::a00();
        assert_eq!(table.glyph(0x20), &[0u8; ROWS_PER_GLYPH]);
    }

    #[test]
    fn a00_table_undefined_ranges_are_blank() {
        // spec §8.1: 0x10..=0x1F and 0x80..=0x9F are gaps in the standard Hitachi ROM table.
        // A00 also leaves 0xA0 itself blank -- its populated extended range starts at 0xA1.
        let table = CgRom::a00();
        for index in 0x10..=0x1F {
            assert_eq!(table.glyph(index), &[0u8; ROWS_PER_GLYPH], "0x{index:02x} should be blank");
        }
        for index in 0x80..=0xA0 {
            assert_eq!(table.glyph(index), &[0u8; ROWS_PER_GLYPH], "0x{index:02x} should be blank");
        }
    }

    #[test]
    fn a00_table_extended_range_is_populated() {
        // spec §8.1: 0xA1..=0xFF carries the A00 variant's Katakana/extended glyphs -- not blank.
        let table = CgRom::a00();
        assert_ne!(table.glyph(0xA1), &[0u8; ROWS_PER_GLYPH]);
        assert_ne!(table.glyph(0xFF), &[0u8; ROWS_PER_GLYPH]);
    }

    #[test]
    fn a02_table_renders_a_as_a_recognizable_letter_a() {
        // 0x41 ('A'): apex at the top-center column, widening into two vertical strokes, blank
        // cursor row at the bottom -- spot-checked against the sourced A02 table rather than
        // merely asserting non-blank, so a transposed or shifted table would fail this test.
        let table = CgRom::a02();
        assert_eq!(table.glyph(0x41), &[4, 10, 17, 17, 31, 17, 17, 0]);
    }

    #[test]
    fn a02_table_ascii_space_is_blank() {
        let table = CgRom::a02();
        assert_eq!(table.glyph(0x20), &[0u8; ROWS_PER_GLYPH]);
    }

    #[test]
    fn a02_table_undefined_ranges_are_blank() {
        // spec §8.1: 0x10..=0x1F and 0x80..=0x9F are gaps in the standard Hitachi ROM table.
        let table = CgRom::a02();
        for index in 0x10..=0x1F {
            assert_eq!(table.glyph(index), &[0u8; ROWS_PER_GLYPH], "0x{index:02x} should be blank");
        }
        for index in 0x80..=0x9F {
            assert_eq!(table.glyph(index), &[0u8; ROWS_PER_GLYPH], "0x{index:02x} should be blank");
        }
    }

    #[test]
    fn a02_table_extended_range_is_populated() {
        // spec §8.1: 0xA0..=0xFF carries the A02 variant's extended glyphs -- not blank.
        let table = CgRom::a02();
        assert_ne!(table.glyph(0xA0), &[0u8; ROWS_PER_GLYPH]);
        assert_ne!(table.glyph(0xFF), &[0u8; ROWS_PER_GLYPH]);
    }
}
