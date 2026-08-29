//! RGB565 palette color storage.
//!
//! `Rgb565` and the masking/scaling conversions specified by `doc/memory-mapped-led-matrix-
//! device-spec.md` §4.2.1. Pixel compositing and the fixed default palette (design doc §9, §4)
//! land in a later work unit.

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
}
