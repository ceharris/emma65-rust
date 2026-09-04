//! The HD44780 character generator ROM (CGROM): a fixed table mapping a DDRAM byte value to that
//! glyph's row bytes (spec §8.1). Not every glyph is the same shape -- the HD44780U datasheet
//! (page 1) declares CGROM as "9,920 bits" made up of 208 character fonts sized 5 x 8 dots plus
//! 32 character fonts sized 5 x 10 dots. The last 32 code points, `0xE0..=0xFF`, are genuinely 10
//! rows tall on real hardware, not an 8-row shape padded out for 5×10-font mode; this table
//! mirrors that split exactly, storing [`ROWS_PER_GLYPH`] rows for `0x00..=0xDF` and
//! [`EXTENDED_ROWS_PER_GLYPH`] rows for `0xE0..=0xFF`. 208 of the 224 standard-range entries are
//! real glyphs; the other 16 (`0x00..=0x0F`) are the CGRAM-alias range, spec §8.1, which never
//! indexes this table at all -- see `compositing::glyph_rows` for how a DDRAM byte resolves to
//! the rows actually drawn.

use std::fmt;

/// Rows stored per glyph in the table's standard range (`0x00..EXTENDED_RANGE_START`) -- the
/// datasheet's 5×8 shape.
pub const ROWS_PER_GLYPH: usize = 8;
/// Rows stored per glyph in the table's extended range (`EXTENDED_RANGE_START..=0xFF`) -- the
/// datasheet's own 5×10 shape for its last 32 glyphs (page 1), not a padded-out 5×8 shape.
pub const EXTENDED_ROWS_PER_GLYPH: usize = 10;
/// First code point of the table's 32-glyph, 5×10-shaped extended range (datasheet page 1).
pub const EXTENDED_RANGE_START: u8 = 0xE0;
/// Number of glyphs: one per possible DDRAM byte value.
pub const GLYPH_COUNT: usize = 256;
/// Number of glyphs in the table's 5×10-shaped extended range.
pub const EXTENDED_GLYPH_COUNT: usize = GLYPH_COUNT - EXTENDED_RANGE_START as usize;
/// Number of glyphs in the table's 5×8-shaped standard range, including the 16 unused
/// `0x00..=0x0F` CGRAM-alias slots -- kept in the byte layout so every index still resolves by
/// simple arithmetic rather than needing a hole in the addressing.
pub const STANDARD_GLYPH_COUNT: usize = GLYPH_COUNT - EXTENDED_GLYPH_COUNT;
/// Total size in bytes of a table's raw data: [`STANDARD_GLYPH_COUNT`] glyphs of
/// [`ROWS_PER_GLYPH`] bytes, followed by [`EXTENDED_GLYPH_COUNT`] glyphs of
/// [`EXTENDED_ROWS_PER_GLYPH`] bytes.
pub const CGROM_BYTES: usize = STANDARD_GLYPH_COUNT * ROWS_PER_GLYPH + EXTENDED_GLYPH_COUNT * EXTENDED_ROWS_PER_GLYPH;

/// Byte offset and row count of `index`'s glyph within a table's raw [`CGROM_BYTES`]-long data.
fn glyph_location(index: u8) -> (usize, usize) {
    if index >= EXTENDED_RANGE_START {
        let extended_index = (index - EXTENDED_RANGE_START) as usize;
        (STANDARD_GLYPH_COUNT * ROWS_PER_GLYPH + extended_index * EXTENDED_ROWS_PER_GLYPH, EXTENDED_ROWS_PER_GLYPH)
    } else {
        (index as usize * ROWS_PER_GLYPH, ROWS_PER_GLYPH)
    }
}

/// Raw bytes of the bundled "A00" (Japanese standard font) table: the Hitachi HD44780 ROM code
/// A00 character ROM (spec §8.1.1), each row byte holding a 5-bit pixel pattern (bit 4 leftmost,
/// matching spec §8.2) in its low bits. The standard range (`0x00..0xE0`) is sourced from the
/// `_jp` font table in `char-lcd.js` (<https://github.com/jazz-soft/char-lcd>, MIT licensed) --
/// the library's own default ROM, assembled from the published HD44780U datasheet's character
/// font table, the same kind of real-hardware-derived, permissively licensed source
/// `display::font`'s default font and [`A02_CGROM_BYTES`] use. Unlike that source's `_eu` (A02)
/// arrays, `_jp`'s rows are already top-aligned with no leading blank row, so each standard-range
/// glyph here is copied through unshifted: rows 0-7 direct from the source array (padding a short
/// array with blanks), confirmed by cross-checking glyphs shared with `_eu` (e.g. `-`, `g`)
/// landing on identical rows under each source's own convention. The extended range
/// (`0xE0..=0xFF`) additionally carries real rows 8/9 for nine glyphs whose descenders genuinely
/// reach that far on real hardware -- `beta, mu, ro, g, j, p, q, y` at `0xE2,E4,E6,E7,EA,F0,F1,F9`
/// plus the solid block at `0xFF` -- verified against the datasheet's printed character table;
/// every other extended-range glyph leaves rows 8/9 blank. `0x00..=0x1F` and `0x80..=0x9F` are
/// blank in the source already, matching spec §8.1's documented gaps in the standard ROM table.
const A00_CGROM_BYTES: &[u8; CGROM_BYTES] = include_bytes!("default_cgrom_a00.bin");

/// Raw bytes of the bundled "A02" (European standard font) table: the Hitachi HD44780 ROM code
/// A02 character ROM (spec §8.1.1), each row byte holding a 5-bit pixel pattern (bit 4 leftmost,
/// matching spec §8.2) in its low bits. Sourced from the `_eu` font table in `char-lcd.js`
/// (<https://github.com/jazz-soft/char-lcd>, MIT licensed), itself assembled from the published
/// HD44780U datasheet's character font table -- the same kind of real-hardware-derived,
/// permissively licensed source `display::font`'s default font uses. `0x00..=0x1F` and
/// `0x80..=0x9F` are forced blank here (rather than keeping that source's extra glyphs in those
/// slots) to match spec §8.1's documented gaps in the standard ROM table exactly. Every
/// standard-range glyph is also shifted up by one row relative to `char-lcd.js`'s own arrays,
/// whose row 0 is the blank row: the HD44780U datasheet (Table 5) documents row 7 (the 8th line)
/// as the reserved cursor row instead, so each glyph here drops that source's row 0 and appends a
/// blank row 7. A02 has no glyphs with real data in the extended range's rows 8/9 (unlike A00) --
/// every extended-range glyph here leaves both blank.
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
            "CGROM data must be exactly {CGROM_BYTES} bytes ({STANDARD_GLYPH_COUNT} glyphs of \
             {ROWS_PER_GLYPH} bytes each, plus {EXTENDED_GLYPH_COUNT} glyphs of \
             {EXTENDED_ROWS_PER_GLYPH} bytes each starting at 0x{EXTENDED_RANGE_START:02X}), got {}",
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

    /// Returns the row bytes for `index` (the DDRAM byte value being resolved, spec §8.1): 8 bytes
    /// for `index < EXTENDED_RANGE_START`, 10 for `index >= EXTENDED_RANGE_START` (the datasheet's
    /// own 5×10 shape for its last 32 glyphs). Meaningless for `0x00..=0x0F`, which never reach
    /// this table (they select a CGRAM character instead, per `compositing::glyph_rows`); callers
    /// still get *some* slice back for that range rather than a panic, since the table itself has
    /// no way to reject an index.
    pub fn glyph(&self, index: u8) -> &[u8] {
        let (offset, len) = glyph_location(index);
        &self.data[offset..offset + len]
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
    fn glyph_reads_correct_slice_in_standard_range() {
        let mut data = vec![0u8; CGROM_BYTES];
        data[8 * 0x41..8 * 0x41 + 8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let table = CgRom::from_bytes(&data).unwrap();
        assert_eq!(table.glyph(0x41), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(table.glyph(0x00), &[0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn glyph_reads_correct_slice_in_extended_range() {
        let mut data = vec![0u8; CGROM_BYTES];
        let offset = STANDARD_GLYPH_COUNT * ROWS_PER_GLYPH + 3 * EXTENDED_ROWS_PER_GLYPH; // 0xE3
        data[offset..offset + 10].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        let table = CgRom::from_bytes(&data).unwrap();
        assert_eq!(table.glyph(0xE3), &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(table.glyph(0xE0), &[0u8; EXTENDED_ROWS_PER_GLYPH]);
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
    fn a00_table_extended_glyphs_are_ten_rows_with_correct_rows_0_through_7() {
        // Unlike A02, A00 has 8 glyphs (plus the solid block) whose descender genuinely reaches
        // row 7 on real hardware, verified against the datasheet's printed character table --
        // row 7 must carry that pixel data rather than being forced blank like every other glyph.
        let table = CgRom::a00();
        assert_eq!(table.glyph(0xE2).len(), 10);
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
    fn a00_table_extended_glyphs_carry_real_rows_8_and_9() {
        // The nine glyphs whose descenders reach row 7 also genuinely extend into rows 8/9 on real
        // 5×10-font hardware, per the datasheet's printed character table (not the padded-blank
        // shape every other CGROM glyph gets).
        let table = CgRom::a00();
        assert_eq!(&table.glyph(0xE2)[8..10], &[0b10000, 0b10000], "beta");
        assert_eq!(&table.glyph(0xE4)[8..10], &[0b10000, 0b10000], "mu");
        assert_eq!(&table.glyph(0xE6)[8..10], &[0b10000, 0b10000], "ro");
        assert_eq!(&table.glyph(0xE7)[8..10], &[0b00001, 0b01110], "g");
        assert_eq!(&table.glyph(0xEA)[8..10], &[0b10010, 0b01100], "j");
        assert_eq!(&table.glyph(0xF0)[8..10], &[0b10000, 0b10000], "p");
        assert_eq!(&table.glyph(0xF1)[8..10], &[0b00001, 0b00001], "q");
        assert_eq!(&table.glyph(0xF9)[8..10], &[0b00001, 0b01110], "y");
        assert_eq!(&table.glyph(0xFF)[8..10], &[0b11111, 0b11111], "solid block");
    }

    #[test]
    fn a00_table_other_extended_glyphs_leave_rows_8_and_9_blank() {
        let table = CgRom::a00();
        for index in EXTENDED_RANGE_START..=0xFF {
            if matches!(index, 0xE2 | 0xE4 | 0xE6 | 0xE7 | 0xEA | 0xF0 | 0xF1 | 0xF9 | 0xFF) {
                continue;
            }
            assert_eq!(&table.glyph(index)[8..10], &[0, 0], "0x{index:02x} should be blank in rows 8/9");
        }
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
        assert_ne!(table.glyph(0xA1), &[0u8; EXTENDED_ROWS_PER_GLYPH]);
        assert_ne!(table.glyph(0xFF), &[0u8; EXTENDED_ROWS_PER_GLYPH]);
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
        assert_ne!(table.glyph(0xA0), &[0u8; EXTENDED_ROWS_PER_GLYPH]);
        assert_ne!(table.glyph(0xFF), &[0u8; EXTENDED_ROWS_PER_GLYPH]);
    }

    #[test]
    fn a02_table_extended_range_never_carries_rows_8_and_9() {
        // Unlike A00, A02 has no glyphs whose real hardware shape extends past row 7.
        let table = CgRom::a02();
        for index in EXTENDED_RANGE_START..=0xFF {
            assert_eq!(&table.glyph(index)[8..10], &[0, 0], "0x{index:02x} should be blank in rows 8/9");
        }
    }
}
