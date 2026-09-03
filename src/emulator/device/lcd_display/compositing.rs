//! Pure frame compositing: DDRAM/CGRAM plus a CGROM table and colors, turned into RGBA pixels
//! (spec §8). No device state lives here -- just the render step, the same split
//! `display::compositing`/`led_matrix::compositing` use, so it is testable and reusable
//! independent of `LcdDisplay` or bus wiring.

use super::Geometry;
use super::cgrom::CgRom;

/// Reused directly rather than duplicated -- the HD44780 has no concept of color, so this
/// device's `background`/`foreground` are plain RGB24 triples exactly like `CharDisplay`'s
/// palette entries (spec §3, design doc §8).
pub use crate::emulator::device::display::compositing::Rgb24;

/// Pixel width of a glyph cell: 5 columns, fixed regardless of font (spec §8.2).
const CELL_WIDTH: usize = 5;
/// Pixel height of a glyph cell in 5×8 mode (`F`=0, spec §8.2).
const CELL_HEIGHT_5X8: usize = 8;
/// Pixel height of a glyph cell in 5×10 mode (`F`=1, spec §8.2).
const CELL_HEIGHT_5X10: usize = 10;

/// The address-counter-derived cursor state for one composite call, computed by the caller (a
/// later work unit's `LcdDisplay::compositing_cursor()`) from the current address counter and
/// `Geometry`'s segment table (spec §8.3): `None` when the address counter targets CGRAM, or a
/// DDRAM address outside every segment's currently visible window.
///
/// `visible` and `blinking` together select what (if anything) is drawn at `position` for *this*
/// call: `visible = false` draws nothing (cursor disabled, or -- when blinking -- this call
/// lands in the blink's off phase); `visible = true, blinking = false` draws an underline;
/// `visible = true, blinking = true` draws a solid block. Any time-based blink alternation is the
/// caller's responsibility -- this function has no clock of its own (spec §8.3's "some
/// implementation-chosen blink cadence").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorState {
    pub position: Option<(u8, u8)>,
    pub visible: bool,
    pub blinking: bool,
}

/// Resolves a DDRAM byte to the row bytes actually drawn for it, already sized to the active
/// font's cell height (8 or 10 rows) -- spec §8.1/§8.2.
///
/// `0x00..=0x0F` selects one of `cgram`'s custom characters (low 3 bits for `F`=0's 8 characters,
/// low 2 bits for `F`=1's 4 characters); every other byte indexes `cgrom` directly. A 5×10 CGROM
/// glyph is the same 8-row CGROM data padded with two blank rows at the bottom -- CGROM has no
/// separate 5×10 table (design doc §5) -- while a 5×10 CGRAM glyph reads its own 10 rows straight
/// out of that character's 16-byte group (spec §8.2); rows 10..16 of that group are real,
/// writable CGRAM (nothing rejects those addresses) but are never rendered, matching real
/// hardware's documented 16-byte-group-with-only-part-rendered layout.
fn glyph_rows(byte: u8, cgram: &[u8; 64], cgrom: &CgRom, font_5x10: bool) -> Vec<u8> {
    if byte <= 0x0F {
        if font_5x10 {
            let base = (byte & 0x03) as usize * 16;
            (0..CELL_HEIGHT_5X10).map(|row| cgram[base + row]).collect()
        } else {
            let base = (byte & 0x07) as usize * 8;
            (0..CELL_HEIGHT_5X8).map(|row| cgram[base + row]).collect()
        }
    } else {
        let glyph = cgrom.glyph(byte);
        if font_5x10 {
            glyph.iter().copied().chain([0, 0]).collect()
        } else {
            glyph.to_vec()
        }
    }
}

/// Resolves `raw` (a physical index into the 40- or 80-byte line `start` belongs to, before
/// `line_shift` is applied) plus a column offset `i` within a segment to the physical DDRAM
/// address (`0..80`) actually displayed there, per spec §7.4. Mirrors
/// `LcdDisplay::shift_display`'s own per-line bucketing (`Geometry::is_dual_line`) so scrolled
/// dual-line geometries shift each 40-byte line independently while single-line geometries treat
/// the whole 80-byte store as one shiftable line.
fn shifted_address(dual_line: bool, start: u8, column: u8, line_shift: &[u8; 2]) -> u8 {
    if dual_line {
        let line = ((start >> 6) & 1) as usize;
        let position_in_line = (start & 0x3F) as u16 + column as u16;
        let shifted = (position_in_line + line_shift[line] as u16) % 40;
        (line as u8) * 40 + shifted as u8
    } else {
        let position = start as u16 + column as u16;
        ((position + line_shift[0] as u16) % 80) as u8
    }
}

/// Draws one glyph's rows into `pixels` at the cell whose top-left pixel is
/// `(col * CELL_WIDTH, row * cell_height)`.
fn draw_glyph(pixels: &mut [u8], width_px: usize, col: usize, row: usize, cell_height: usize, rows: &[u8], color: Rgb24) {
    for (glyph_row, &row_bits) in rows.iter().enumerate() {
        let pixel_y = row * cell_height + glyph_row;
        for glyph_col in 0..CELL_WIDTH {
            // Bit 4 (of the low 5 bits) is the leftmost column (spec §8.2).
            if (row_bits >> (4 - glyph_col)) & 1 == 0 {
                continue;
            }
            let pixel_x = col * CELL_WIDTH + glyph_col;
            set_pixel(pixels, width_px, pixel_x, pixel_y, color);
        }
    }
}

/// Draws the cursor at the cell whose top-left pixel is `(col * CELL_WIDTH, row * cell_height)`:
/// a solid block (every pixel) when `blinking`, otherwise an underline (just the bottom row) --
/// spec §8.3.
fn draw_cursor(pixels: &mut [u8], width_px: usize, col: usize, row: usize, cell_height: usize, blinking: bool, color: Rgb24) {
    let rows = if blinking { 0..cell_height } else { cell_height - 1..cell_height };
    for pixel_y in row * cell_height + rows.start..row * cell_height + rows.end {
        for glyph_col in 0..CELL_WIDTH {
            let pixel_x = col * CELL_WIDTH + glyph_col;
            set_pixel(pixels, width_px, pixel_x, pixel_y, color);
        }
    }
}

fn set_pixel(pixels: &mut [u8], width_px: usize, x: usize, y: usize, color: Rgb24) {
    let offset = (y * width_px + x) * 4;
    pixels[offset] = color.r;
    pixels[offset + 1] = color.g;
    pixels[offset + 2] = color.b;
    pixels[offset + 3] = 0xFF;
}

/// Composites one frame of `geometry.columns * 5` by `geometry.rows * (8 or 10)` RGBA pixels
/// (spec §8.3), fully opaque throughout (the HD44780 has no separate background register, same
/// rationale as `display::compositing::composite`).
///
/// Walks each of `geometry`'s rows and each row's segments (spec §7.1), applying `line_shift`'s
/// modulo-40-or-80 offset (spec §7.4) to find each visible cell's actual DDRAM address, resolves
/// that byte to a glyph's rows ([`glyph_rows`]), and draws it in `foreground` against
/// `background`. `display_on = false` renders every cell blank instead, leaving DDRAM, CGRAM,
/// and the address counter untouched by definition since this function never mutates them (spec
/// §8.3).
#[allow(clippy::too_many_arguments)]
pub fn composite(
    ddram: &[u8; 80],
    cgram: &[u8; 64],
    geometry: &Geometry,
    line_shift: &[u8; 2],
    cursor: CursorState,
    display_on: bool,
    font_5x10: bool,
    cgrom: &CgRom,
    background: Rgb24,
    foreground: Rgb24,
) -> Vec<u8> {
    let cell_height = if font_5x10 { CELL_HEIGHT_5X10 } else { CELL_HEIGHT_5X8 };
    let columns = geometry.columns as usize;
    let rows = geometry.rows as usize;
    let width_px = columns * CELL_WIDTH;
    let height_px = rows * cell_height;

    let mut pixels = vec![0u8; width_px * height_px * 4];
    for cell in 0..width_px * height_px {
        let offset = cell * 4;
        pixels[offset] = background.r;
        pixels[offset + 1] = background.g;
        pixels[offset + 2] = background.b;
        pixels[offset + 3] = 0xFF;
    }

    if !display_on {
        return pixels;
    }

    let dual_line = geometry.is_dual_line();

    for (row_index, row_segments) in geometry.segments.iter().enumerate() {
        let mut col = 0usize;
        for &(start, count) in row_segments.iter() {
            for offset in 0..count {
                let addr = shifted_address(dual_line, start, offset, line_shift);
                let rows = glyph_rows(ddram[addr as usize], cgram, cgrom, font_5x10);
                draw_glyph(&mut pixels, width_px, col, row_index, cell_height, &rows, foreground);

                if cursor.visible && cursor.position == Some((row_index as u8, col as u8)) {
                    draw_cursor(&mut pixels, width_px, col, row_index, cell_height, cursor.blinking, foreground);
                }

                col += 1;
            }
        }
    }

    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    const BG: Rgb24 = Rgb24::new(0, 0, 0);
    const FG: Rgb24 = Rgb24::new(255, 255, 255);

    const SINGLE_ROW: Geometry = Geometry { rows: 1, columns: 2, segments: &[&[(0x00, 2)]] };
    const DUAL_ROW: Geometry = Geometry { rows: 2, columns: 2, segments: &[&[(0x00, 2)], &[(0x40, 2)]] };

    fn empty_ddram() -> [u8; 80] {
        [0x20; 80]
    }

    fn blank_cgram() -> [u8; 64] {
        [0; 64]
    }

    fn no_cursor() -> CursorState {
        CursorState::default()
    }

    /// Reads the 4-byte pixel at `(x, y)` out of a composited frame with the given row stride.
    fn pixel_at(pixels: &[u8], width_px: usize, x: usize, y: usize) -> [u8; 4] {
        let offset = (y * width_px + x) * 4;
        pixels[offset..offset + 4].try_into().unwrap()
    }

    fn all_pixels_are(pixels: &[u8], color: Rgb24) -> bool {
        let expected = [color.r, color.g, color.b, 0xFF];
        let mut i = 0;
        while i < pixels.len() {
            if pixels[i..i + 4] != expected {
                return false;
            }
            i += 4;
        }
        true
    }

    #[test]
    fn display_off_renders_all_background() {
        let mut ddram = empty_ddram();
        ddram[0] = 0x41; // 'A' -- must not show through
        let pixels = composite(&ddram, &blank_cgram(), &SINGLE_ROW, &[0, 0], no_cursor(), false, false, &CgRom::default(), BG, FG);

        assert_eq!(pixels.len(), SINGLE_ROW.columns as usize * CELL_WIDTH * 8 * 4);
        assert!(all_pixels_are(&pixels, BG));
    }

    #[test]
    fn ascii_glyph_composites_expected_pixels() {
        let mut ddram = empty_ddram();
        ddram[0] = 0x41; // 'A'
        let pixels = composite(&ddram, &blank_cgram(), &SINGLE_ROW, &[0, 0], no_cursor(), true, false, &CgRom::default(), BG, FG);
        let width_px = SINGLE_ROW.columns as usize * CELL_WIDTH;

        // Row 1 of 'A' is [0, 4, 10, ...]: row 0 blank, row 1 has only the middle (col 2) pixel
        // set.
        assert_eq!(pixel_at(&pixels, width_px, 2, 1), [FG.r, FG.g, FG.b, 0xFF]);
        // Row 0 is blank -- top-left pixel stays background.
        assert_eq!(pixel_at(&pixels, width_px, 0, 0), [BG.r, BG.g, BG.b, 0xFF]);
    }

    #[test]
    fn second_cell_composites_at_correct_column_offset() {
        let mut ddram = empty_ddram();
        ddram[1] = 0x41; // 'A' in the second visible column
        let pixels = composite(&ddram, &blank_cgram(), &SINGLE_ROW, &[0, 0], no_cursor(), true, false, &CgRom::default(), BG, FG);
        let width_px = SINGLE_ROW.columns as usize * CELL_WIDTH;

        assert_eq!(pixel_at(&pixels, width_px, CELL_WIDTH + 2, 1), [FG.r, FG.g, FG.b, 0xFF]);
    }

    #[test]
    fn dual_line_second_row_reads_from_second_ddram_line() {
        let mut ddram = empty_ddram();
        ddram[40] = 0x41; // 'A' at the start of the second physical line (raw address 0x40 folds to 40)
        let pixels = composite(&ddram, &blank_cgram(), &DUAL_ROW, &[0, 0], no_cursor(), true, false, &CgRom::default(), BG, FG);
        let width_px = DUAL_ROW.columns as usize * CELL_WIDTH;
        let second_display_row = 1;

        assert_eq!(pixel_at(&pixels, width_px, 2, second_display_row * 8 + 1), [FG.r, FG.g, FG.b, 0xFF]);
    }

    // Mirrors the real `40x2` geometry (spec §7.1): wide dual-line segments whose column offsets
    // push well past the raw `0x40` second-segment start, exercising the fold from raw HD44780
    // address to physical `ddram` index (regression for the `shifted_address` addressing bug
    // found while reasoning about Work Unit 3's real geometry table).
    const WIDE_DUAL_ROW: Geometry =
        Geometry { rows: 2, columns: 40, segments: &[&[(0x00, 40)], &[(0x40, 40)]] };

    #[test]
    fn wide_dual_line_last_column_stays_in_bounds_and_reads_correct_cell() {
        let mut ddram = empty_ddram();
        ddram[79] = 0x41; // 'A' at the last physical byte of the second line
        let pixels = composite(&ddram, &blank_cgram(), &WIDE_DUAL_ROW, &[0, 0], no_cursor(), true, false, &CgRom::default(), BG, FG);
        let width_px = WIDE_DUAL_ROW.columns as usize * CELL_WIDTH;
        let last_col = WIDE_DUAL_ROW.columns as usize - 1;
        let second_display_row = 1;

        assert_eq!(
            pixel_at(&pixels, width_px, last_col * CELL_WIDTH + 2, second_display_row * 8 + 1),
            [FG.r, FG.g, FG.b, 0xFF]
        );
    }

    // Mirrors the real `20x4` geometry (spec §7.1): rows 3 and 4 start mid-line (raw `0x14`,
    // `0x54`) rather than at a line boundary, exercising the fold for non-boundary segment
    // starts.
    const PAIRED_ROW_GEOMETRY: Geometry = Geometry {
        rows: 4,
        columns: 20,
        segments: &[&[(0x00, 20)], &[(0x40, 20)], &[(0x14, 20)], &[(0x54, 20)]],
    };

    #[test]
    fn paired_row_geometry_folds_mid_line_segment_starts_correctly() {
        let mut ddram = empty_ddram();
        ddram[20] = 0x41; // row 3's segment starts at raw 0x14, which folds to physical line 0, index 20
        ddram[60] = 0x41; // row 4's segment starts at raw 0x54, which folds to physical line 1, index 60
        let pixels = composite(&ddram, &blank_cgram(), &PAIRED_ROW_GEOMETRY, &[0, 0], no_cursor(), true, false, &CgRom::default(), BG, FG);
        let width_px = PAIRED_ROW_GEOMETRY.columns as usize * CELL_WIDTH;

        // 'A' row 1 of the glyph has only the middle column set (see ascii_glyph_composites_expected_pixels).
        assert_eq!(pixel_at(&pixels, width_px, 2, 2 * 8 + 1), [FG.r, FG.g, FG.b, 0xFF], "row 3 (index 2)");
        assert_eq!(pixel_at(&pixels, width_px, 2, 3 * 8 + 1), [FG.r, FG.g, FG.b, 0xFF], "row 4 (index 3)");
    }

    #[test]
    fn line_shift_scrolls_visible_window() {
        let mut ddram = empty_ddram();
        ddram[1] = 0x41; // 'A' one position to the right of the unshifted window
        // Shifting line 0 left by one brings ddram[1] into the first visible column.
        let pixels = composite(&ddram, &blank_cgram(), &SINGLE_ROW, &[1, 0], no_cursor(), true, false, &CgRom::default(), BG, FG);
        let width_px = SINGLE_ROW.columns as usize * CELL_WIDTH;

        assert_eq!(pixel_at(&pixels, width_px, 2, 1), [FG.r, FG.g, FG.b, 0xFF]);
    }

    #[test]
    fn cgram_custom_character_5x8_composites_from_cgram() {
        let ddram = {
            let mut d = empty_ddram();
            d[0] = 0x02; // custom character 2 (F=0: low 3 bits)
            d
        };
        let mut cgram = blank_cgram();
        let character_row = 3;
        cgram[2 * CELL_HEIGHT_5X8 + character_row] = 0b10101; // alternating pixels
        let pixels = composite(&ddram, &cgram, &SINGLE_ROW, &[0, 0], no_cursor(), true, false, &CgRom::default(), BG, FG);
        let width_px = SINGLE_ROW.columns as usize * CELL_WIDTH;

        for (col, expect_set) in [(0, true), (1, false), (2, true), (3, false), (4, true)] {
            let expected = if expect_set { [FG.r, FG.g, FG.b, 0xFF] } else { [BG.r, BG.g, BG.b, 0xFF] };
            assert_eq!(pixel_at(&pixels, width_px, col, character_row), expected, "column {col}");
        }
    }

    #[test]
    fn cgram_custom_character_5x10_uses_sixteen_byte_group() {
        let ddram = {
            let mut d = empty_ddram();
            d[0] = 0x01; // custom character 1 (F=1: low 2 bits)
            d
        };
        let mut cgram = blank_cgram();
        let character_group = 1;
        let last_rendered_row = 9;
        cgram[character_group * 16 + last_rendered_row] = 0b11111;
        let pixels = composite(&ddram, &cgram, &SINGLE_ROW, &[0, 0], no_cursor(), true, true, &CgRom::default(), BG, FG);
        let width_px = SINGLE_ROW.columns as usize * CELL_WIDTH;

        for col in 0..CELL_WIDTH {
            assert_eq!(pixel_at(&pixels, width_px, col, last_rendered_row), [FG.r, FG.g, FG.b, 0xFF]);
        }
    }

    #[test]
    fn underline_cursor_draws_only_bottom_row() {
        let ddram = empty_ddram();
        let cursor = CursorState { position: Some((0, 0)), visible: true, blinking: false };
        let pixels = composite(&ddram, &blank_cgram(), &SINGLE_ROW, &[0, 0], cursor, true, false, &CgRom::default(), BG, FG);
        let width_px = SINGLE_ROW.columns as usize * CELL_WIDTH;
        let bottom_row = CELL_HEIGHT_5X8 - 1;

        for col in 0..CELL_WIDTH {
            assert_eq!(pixel_at(&pixels, width_px, col, bottom_row), [FG.r, FG.g, FG.b, 0xFF]);
        }
        // Any other row stays background (glyph is blank space).
        assert_eq!(pixel_at(&pixels, width_px, 0, 0), [BG.r, BG.g, BG.b, 0xFF]);
    }

    #[test]
    fn blinking_cursor_draws_solid_block() {
        let ddram = empty_ddram();
        let cursor = CursorState { position: Some((0, 1)), visible: true, blinking: true };
        let pixels = composite(&ddram, &blank_cgram(), &SINGLE_ROW, &[0, 0], cursor, true, false, &CgRom::default(), BG, FG);
        let width_px = SINGLE_ROW.columns as usize * CELL_WIDTH;

        for row in 0..CELL_HEIGHT_5X8 {
            for col in CELL_WIDTH..2 * CELL_WIDTH {
                assert_eq!(pixel_at(&pixels, width_px, col, row), [FG.r, FG.g, FG.b, 0xFF], "row {row} col {col}");
            }
        }
    }

    #[test]
    fn cursor_not_visible_leaves_glyph_untouched() {
        let ddram = empty_ddram();
        let cursor = CursorState { position: Some((0, 0)), visible: false, blinking: true };
        let pixels = composite(&ddram, &blank_cgram(), &SINGLE_ROW, &[0, 0], cursor, true, false, &CgRom::default(), BG, FG);

        assert!(all_pixels_are(&pixels, BG));
    }
}
