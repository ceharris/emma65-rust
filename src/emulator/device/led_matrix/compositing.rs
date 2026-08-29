//! RGB565 palette color storage, pixel compositing, and the fixed default palette.
//!
//! `Rgb565` and the masking/scaling conversions specified by `doc/memory-mapped-led-matrix-
//! device-spec.md` §4.2.1; [`composite_matrix`] (design doc §9); [`default_palette`], ported
//! verbatim from spec §2.1 (design doc §4).

/// A 16-bit RGB565 color: 5 bits red, 6 bits green, 5 bits blue -- the color depth real RGB LED
/// matrix driver hardware actually uses (spec §2), unlike `display::compositing::Rgb24`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb565 {
    r: u8,
    g: u8,
    b: u8,
}

impl Rgb565 {
    /// Packs already-5/6/5-bit components, masking each defensively (`& 0x1F`/`& 0x3F`/`& 0x1F`)
    /// against out-of-range callers. The building block both `from_rgb888` and the (later)
    /// default palette are built on.
    pub fn new(r5: u8, g6: u8, b5: u8) -> Self {
        Self { r: r5 & 0x1F, g: g6 & 0x3F, b: b5 & 0x1F }
    }

    /// Mask (spec §4.2.1): shifts each 8-bit component down to its native bit width, discarding
    /// low-order bits, then packs via `new`. Used by `CMD_PALETTE_WRITE`.
    pub fn from_rgb888(r: u8, g: u8, b: u8) -> Self {
        Self::new(r >> 3, g >> 2, b >> 3)
    }

    /// Scale (spec §4.2.1): expands each stored component back to 8 bits by bit-replication (not
    /// a bare left-shift, which would fall short of `0xFF` at the top of the range). Used by
    /// `CMD_PALETTE_READ` and by compositing.
    pub fn to_rgb888(self) -> (u8, u8, u8) {
        let r = (self.r << 3) | (self.r >> 2);
        let g = (self.g << 2) | (self.g >> 4);
        let b = (self.b << 3) | (self.b >> 2);
        (r, g, b)
    }
}

/// Composites one matrix's 1,024 palette-index bytes (spec §2, row-major) into
/// `PIXELS_PER_MATRIX * 4` RGBA bytes (design doc §9), via `palette[index].to_rgb888()` -- the
/// same quantized-then-expanded color a `CMD_PALETTE_READ` of that entry would report, not the
/// original pre-quantization `CMD_PALETTE_WRITE` bytes. Every pixel is fully opaque.
///
/// Index resolution is `index as usize % palette.len()`, matching the modulo rule
/// `display::compositing::resolve_palette_index` already established for `CharDisplay`.
pub fn composite_matrix(pixels: &[u8], palette: &[Rgb565]) -> Vec<u8> {
    debug_assert_eq!(pixels.len(), super::PIXELS_PER_MATRIX, "pixels must be exactly one matrix's worth");
    debug_assert!(!palette.is_empty(), "palette must be non-empty");

    let mut rgba = vec![0u8; pixels.len() * 4];
    for (i, &index) in pixels.iter().enumerate() {
        let (r, g, b) = palette[index as usize % palette.len()].to_rgb888();
        let offset = i * 4;
        rgba[offset] = r;
        rgba[offset + 1] = g;
        rgba[offset + 2] = b;
        rgba[offset + 3] = 0xFF;
    }
    rgba
}

/// Rounds `level * target_max / source_max` to the nearest integer (spec §2.1's `round`; none of
/// the scheme's divisions land on an exact half, so no tie-breaking rule is needed).
fn round_scale(level: u8, target_max: u8, source_max: u8) -> u8 {
    (level as f64 * target_max as f64 / source_max as f64).round() as u8
}

/// Builds the device's fixed 256-entry default palette (spec §2.1), directly in RGB565
/// component space -- not derived from an 8-bit truecolor palette quantized down through
/// [`Rgb565::from_rgb888`] (design doc §4). The device and the eventual companion process (out
/// of scope for this plan) must each reconstruct this exact table independently, since the
/// transport never transfers palette contents at startup (spec §7).
pub fn default_palette() -> Vec<Rgb565> {
    let mut palette = Vec::with_capacity(super::PALETTE_LEN);

    // [0..7]: 8 primary/secondary colors at half intensity.
    palette.push(Rgb565::new(0, 0, 0)); // black
    palette.push(Rgb565::new(15, 0, 0)); // red
    palette.push(Rgb565::new(0, 31, 0)); // green
    palette.push(Rgb565::new(15, 31, 0)); // yellow
    palette.push(Rgb565::new(0, 0, 15)); // blue
    palette.push(Rgb565::new(15, 0, 15)); // magenta
    palette.push(Rgb565::new(0, 31, 15)); // cyan
    palette.push(Rgb565::new(23, 47, 23)); // white

    // [8..15]: the same 8 colors at full intensity.
    palette.push(Rgb565::new(7, 15, 7)); // gray
    palette.push(Rgb565::new(31, 0, 0)); // bright red
    palette.push(Rgb565::new(0, 63, 0)); // bright green
    palette.push(Rgb565::new(31, 63, 0)); // bright yellow
    palette.push(Rgb565::new(0, 0, 31)); // bright blue
    palette.push(Rgb565::new(31, 0, 31)); // bright magenta
    palette.push(Rgb565::new(0, 63, 31)); // bright cyan
    palette.push(Rgb565::new(31, 63, 31)); // bright white

    // [16..231]: a 6x6x6 RGB color cube, r outermost, b innermost.
    for r in 0..6u8 {
        for g in 0..6u8 {
            for b in 0..6u8 {
                palette.push(Rgb565::new(round_scale(r, 31, 5), round_scale(g, 63, 5), round_scale(b, 31, 5)));
            }
        }
    }

    // [232..255]: a 24-step grayscale ramp.
    for level in 0..24u8 {
        let rb = round_scale(level, 31, 23);
        let g = round_scale(level, 63, 23);
        palette.push(Rgb565::new(rb, g, rb));
    }

    debug_assert_eq!(palette.len(), super::PALETTE_LEN);
    palette
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_masks_out_of_range_components_defensively() {
        let color = Rgb565::new(0xFF, 0xFF, 0xFF);
        assert_eq!(color, Rgb565::new(0x1F, 0x3F, 0x1F));
    }

    #[test]
    fn from_rgb888_masks_to_native_bit_width() {
        assert_eq!(Rgb565::from_rgb888(0x00, 0x00, 0x00), Rgb565::new(0, 0, 0));
        assert_eq!(Rgb565::from_rgb888(0xFF, 0xFF, 0xFF), Rgb565::new(0x1F, 0x3F, 0x1F));
        // A component not a multiple of 8 (red/blue) or 4 (green) is truncated, not rounded.
        assert_eq!(Rgb565::from_rgb888(0x0F, 0x0F, 0x0F), Rgb565::new(0x01, 0x03, 0x01));
    }

    #[test]
    fn to_rgb888_round_trips_zero_and_max_exactly() {
        assert_eq!(Rgb565::new(0, 0, 0).to_rgb888(), (0x00, 0x00, 0x00));
        assert_eq!(Rgb565::new(0x1F, 0x3F, 0x1F).to_rgb888(), (0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn to_rgb888_scales_by_bit_replication_not_left_shift() {
        // A bare left-shift (r << 3) would leave red at 0xF8, short of 0xFF; bit-replication
        // fills the low-order bits from the high-order bits instead.
        assert_eq!(Rgb565::new(0x1F, 0, 0).to_rgb888().0, 0xFF);
        assert_eq!(Rgb565::new(0, 0x3F, 0).to_rgb888().1, 0xFF);
        assert_eq!(Rgb565::new(0, 0, 0x1F).to_rgb888().2, 0xFF);
    }

    #[test]
    fn write_then_read_round_trip_is_not_guaranteed_exact() {
        // spec §4.2.1: a written byte that isn't a multiple of the channel's discarded bits
        // reads back changed.
        let stored = Rgb565::from_rgb888(0x0F, 0x0F, 0x0F);
        assert_ne!(stored.to_rgb888(), (0x0F, 0x0F, 0x0F));
    }

    #[test]
    fn zero_and_max_round_trip_exactly() {
        assert_eq!(Rgb565::from_rgb888(0x00, 0x00, 0x00).to_rgb888(), (0x00, 0x00, 0x00));
        assert_eq!(Rgb565::from_rgb888(0xFF, 0xFF, 0xFF).to_rgb888(), (0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn composite_matrix_maps_each_pixel_to_its_palette_color() {
        let palette = [Rgb565::new(0, 0, 0), Rgb565::new(0x1F, 0, 0), Rgb565::new(0, 0x3F, 0)];
        let mut pixels = vec![0u8; super::super::PIXELS_PER_MATRIX];
        pixels[0] = 1; // top-left: red
        pixels[super::super::PIXELS_PER_MATRIX - 1] = 2; // bottom-right: green

        let rgba = composite_matrix(&pixels, &palette);

        assert_eq!(rgba.len(), super::super::PIXELS_PER_MATRIX * 4);
        assert_eq!(&rgba[0..4], &[0xFF, 0x00, 0x00, 0xFF]);
        let last = rgba.len() - 4;
        assert_eq!(&rgba[last..], &[0x00, 0xFF, 0x00, 0xFF]);
        // An untouched pixel (index 0 in the palette) composites to black, opaque.
        assert_eq!(&rgba[4..8], &[0x00, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn composite_matrix_wraps_out_of_range_indices_via_modulo() {
        let palette = [Rgb565::new(10, 20, 5), Rgb565::new(0, 0, 0)];
        let mut pixels = vec![0u8; super::super::PIXELS_PER_MATRIX];
        pixels[0] = 4; // 4 % 2 == 0 -> first entry

        let rgba = composite_matrix(&pixels, &palette);

        assert_eq!(&rgba[0..4], &[palette[0].to_rgb888().0, palette[0].to_rgb888().1, palette[0].to_rgb888().2, 0xFF]);
    }

    #[test]
    fn default_palette_has_expected_length_and_worked_values() {
        let palette = default_palette();
        assert_eq!(palette.len(), super::super::PALETTE_LEN);
        // spec §2.1 worked values.
        assert_eq!(palette[0], Rgb565::new(0, 0, 0), "index 0 is black");
        assert_eq!(palette[9], Rgb565::new(31, 0, 0), "index 9 is bright red");
        assert_eq!(palette[255], Rgb565::new(31, 63, 31), "index 255 is the top of the grayscale ramp");
    }

    #[test]
    fn default_palette_half_intensity_block_matches_spec() {
        let palette = default_palette();
        assert_eq!(palette[1], Rgb565::new(15, 0, 0)); // red
        assert_eq!(palette[7], Rgb565::new(23, 47, 23)); // white
    }

    #[test]
    fn default_palette_color_cube_first_and_last_entries() {
        let palette = default_palette();
        // Cube starts at index 16 with (0, 0, 0) and ends at index 231 with (5, 5, 5) scaled up.
        assert_eq!(palette[16], Rgb565::new(0, 0, 0));
        assert_eq!(palette[231], Rgb565::new(31, 63, 31));
    }

    #[test]
    fn default_palette_grayscale_ramp_starts_black() {
        let palette = default_palette();
        assert_eq!(palette[232], Rgb565::new(0, 0, 0));
    }
}
