//! Wire codec for `LcdDisplay`'s external protocol.
//!
//! See `plan/lcd-display-external-protocol.md` for the full specification. Summary: a one-time
//! header ([`encode_header`]) sent immediately when an external transport is attached, followed by
//! one self-describing frame message ([`encode_frame`]) every time the device pushes a frame (no
//! periodic cadence at all -- design doc §7). Unlike `CharDisplay`'s/`LedMatrix`'s fixed-size
//! frames, each frame here carries its own `width`/`height` (spec §5) since the active font
//! (`Function Set`'s `F` bit) can change a frame's pixel height at runtime.

use super::compositing::Rgb24;

/// Magic bytes identifying this protocol (spec §4) -- distinct from `"E65D"` (display) and
/// `"E65M"` (LED matrix).
const MAGIC: [u8; 4] = *b"E65L";
/// Protocol version (spec §4), incremented on any wire-incompatible change.
const VERSION: u8 = 1;

/// Builds the one-time header sent immediately on attach (spec §4): magic, version, the character
/// grid's dimensions, and the configuration-time-fixed background/foreground colors.
pub fn encode_header(columns: u8, rows: u8, background: Rgb24, foreground: Rgb24) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + 1 + 1 + 1 + 3 + 3);
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.push(columns);
    buf.push(rows);
    buf.extend_from_slice(&[background.r, background.g, background.b]);
    buf.extend_from_slice(&[foreground.r, foreground.g, foreground.b]);
    buf
}

/// Builds one frame message (spec §5): pixel width, pixel height, then the raw composited RGBA
/// payload -- `pixels.len()` must equal `width_px as usize * height_px as usize * 4`.
pub fn encode_frame(width_px: u16, height_px: u16, pixels: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(2 + 2 + pixels.len());
    buf.extend_from_slice(&width_px.to_le_bytes());
    buf.extend_from_slice(&height_px.to_le_bytes());
    buf.extend_from_slice(pixels);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_matches_spec() {
        let header = encode_header(16, 2, Rgb24::new(0, 0, 0xAA), Rgb24::new(0xFF, 0xFF, 0xFF));

        assert_eq!(&header[0..4], b"E65L");
        assert_eq!(header[4], 1);
        assert_eq!(header[5], 16);
        assert_eq!(header[6], 2);
        assert_eq!(&header[7..10], &[0, 0, 0xAA]);
        assert_eq!(&header[10..13], &[0xFF, 0xFF, 0xFF]);
        assert_eq!(header.len(), 13);
    }

    #[test]
    fn frame_layout_matches_spec() {
        let pixels = vec![1u8, 2, 3, 255, 4, 5, 6, 255];

        let frame = encode_frame(2, 1, &pixels);

        assert_eq!(&frame[0..2], &2u16.to_le_bytes());
        assert_eq!(&frame[2..4], &1u16.to_le_bytes());
        assert_eq!(&frame[4..], pixels.as_slice());
        assert_eq!(frame.len(), 4 + pixels.len());
    }

    #[test]
    fn frame_size_varies_with_dimensions_not_a_fixed_constant() {
        let short = encode_frame(10, 8, &vec![0u8; 10 * 8 * 4]);
        let tall = encode_frame(10, 10, &vec![0u8; 10 * 10 * 4]);

        assert_ne!(short.len(), tall.len());
    }
}
