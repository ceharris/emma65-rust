//! Pure frame compositing: character/color RAM plus a palette and font, turned into RGBA
//! pixels (spec §6). No device or configuration state lives here -- just the render step,
//! so it is testable and reusable independent of `CharDisplay` or bus wiring.

use super::font::Font;

/// An RGB24 color, as configured in a device's palette (spec §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb24 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb24 {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Resolves a color-RAM byte to a palette entry using the modulo rule (design doc §2):
/// `index % palette.len()`, unconditionally -- bit-identical to power-of-two masking for
/// every power-of-two palette length, and well-defined for every other length too.
fn resolve_color(index: u8, palette: &[Rgb24]) -> Rgb24 {
    debug_assert!(!palette.is_empty(), "palette must be non-empty (spec §3 validates this at configuration time)");
    palette[index as usize % palette.len()]
}

/// Composites one frame of `columns * 8` by `rows * 8` RGBA pixels (4 bytes per pixel,
/// row-major, top row first) from a `(char_ram, color_ram)` pair as returned by
/// `CharDisplay::frame_source()` (spec §6), a `palette`, and a `font`.
///
/// A cell's color applies only to the glyph's set pixels; unset pixels render as opaque
/// black -- the spec defines no separate background register, so every composited frame is
/// fully opaque (alpha 255 throughout) rather than mixing in a transparent background that
/// nothing else in the spec accounts for.
///
/// `char_ram` and `color_ram` must each have exactly `columns * rows` entries, matching the
/// invariant `CharDisplay` already upholds internally.
pub fn composite(char_ram: &[u8], color_ram: &[u8], columns: u32, rows: u32, palette: &[Rgb24], font: &Font) -> Vec<u8> {
    let columns = columns as usize;
    let rows = rows as usize;
    let cells = columns * rows;
    debug_assert_eq!(char_ram.len(), cells, "char_ram length must equal columns * rows");
    debug_assert_eq!(color_ram.len(), cells, "color_ram length must equal columns * rows");

    let width_px = columns * 8;
    let height_px = rows * 8;
    // Every pixel is opaque black by default; the loop below only needs to overwrite the
    // pixels a glyph actually sets.
    let mut pixels = vec![0u8; width_px * height_px * 4];
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 0xFF;
    }

    for cell_row in 0..rows {
        for cell_col in 0..columns {
            let cell = cell_row * columns + cell_col;
            let glyph = font.glyph(char_ram[cell]);
            let color = resolve_color(color_ram[cell], palette);

            for (glyph_row, row_bits) in glyph.iter().enumerate() {
                let pixel_y = cell_row * 8 + glyph_row;
                for glyph_col in 0..8usize {
                    if (row_bits >> glyph_col) & 1 == 0 {
                        continue;
                    }
                    let pixel_x = cell_col * 8 + glyph_col;
                    let offset = (pixel_y * width_px + pixel_x) * 4;
                    pixels[offset] = color.r;
                    pixels[offset + 1] = color.g;
                    pixels[offset + 2] = color.b;
                    pixels[offset + 3] = 0xFF;
                }
            }
        }
    }

    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_cell_font(set_pixels: &[(usize, usize)]) -> Font {
        let mut data = vec![0u8; super::super::font::FONT_BYTES];
        // Glyph 0x01 has the given (row, col) pixels set; every other glyph stays blank.
        for &(row, col) in set_pixels {
            data[8_usize + row] |= 1 << col;
        }
        Font::from_bytes(&data).unwrap()
    }

    #[test]
    fn single_cell_composites_expected_pixels() {
        // Glyph 0x01: top-left pixel (row 0, col 0) and bottom-right pixel (row 7, col 7) set.
        let font = one_cell_font(&[(0, 0), (7, 7)]);
        let palette = [Rgb24::new(0, 0, 0), Rgb24::new(255, 0, 0)];
        let char_ram = [0x01];
        let color_ram = [1u8];

        let pixels = composite(&char_ram, &color_ram, 1, 1, &palette, &font);

        assert_eq!(pixels.len(), 8 * 8 * 4);
        // Top-left pixel: red, opaque.
        assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
        // Bottom-right pixel: same red.
        let br_offset = (7 * 8 + 7) * 4;
        assert_eq!(&pixels[br_offset..br_offset + 4], &[255, 0, 0, 255]);
        // An unset pixel (row 0, col 1): opaque black.
        assert_eq!(&pixels[4..8], &[0, 0, 0, 255]);
    }

    #[test]
    fn palette_index_wraps_via_modulo() {
        let font = one_cell_font(&[(0, 0)]);
        let palette = [Rgb24::new(10, 20, 30), Rgb24::new(40, 50, 60), Rgb24::new(70, 80, 90)];
        let char_ram = [0x01];
        // 5 % 3 == 2 -> third palette entry.
        let color_ram = [5u8];

        let pixels = composite(&char_ram, &color_ram, 1, 1, &palette, &font);

        assert_eq!(&pixels[0..4], &[70, 80, 90, 255]);
    }

    #[test]
    fn multi_cell_grid_places_glyphs_at_correct_offsets() {
        let font = one_cell_font(&[(0, 0)]);
        let palette = [Rgb24::new(0, 0, 0), Rgb24::new(200, 200, 200)];
        // 2x1 grid: cell 0 blank (glyph 0x00), cell 1 uses glyph 0x01 with color index 1.
        let char_ram = [0x00, 0x01];
        let color_ram = [0u8, 1u8];

        let pixels = composite(&char_ram, &color_ram, 2, 1, &palette, &font);

        // Cell 0's top-left pixel: opaque black (blank glyph).
        assert_eq!(&pixels[0..4], &[0, 0, 0, 255]);
        // Cell 1's top-left pixel (pixel_x = 8, pixel_y = 0): light gray.
        let cell1_offset = 8 * 4;
        assert_eq!(&pixels[cell1_offset..cell1_offset + 4], &[200, 200, 200, 255]);
    }
}
