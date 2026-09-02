//! Wire codec for `CharDisplay`'s external display protocol.
//!
//! See `plan/char-display-external-protocol.md` for the full specification. Summary: a one-time
//! header ([`encode_header`]) sent immediately when an external transport is attached, followed
//! by one fixed-size frame message ([`encode_frame`]) per vsync. There is no per-frame length
//! prefix or delimiter -- the header alone fixes every subsequent frame's size, which is safe
//! only because the attached transport's [`Transport::send_bytes`](crate::emulator::transport::Transport::send_bytes)
//! is required to be all-or-nothing (never a partial write that would desync the stream).

use super::compositing::Rgb24;
use super::font::Font;

/// Magic bytes identifying this protocol (spec §4) -- distinct from the CPU trace format's
/// `"E65T"`.
const MAGIC: [u8; 4] = *b"E65D";
/// Protocol version (spec §4), incremented on any wire-incompatible change.
const VERSION: u8 = 1;

/// Builds the one-time header sent immediately on attach (spec §4): magic, version, grid
/// dimensions, vsync cadence, palette length, then the raw font bytes.
pub fn encode_header(columns: u32, rows: u32, frame_rate_hz: u32, palette_len: u16, font: &Font) -> Vec<u8> {
    let font_bytes = font.as_bytes();
    let mut buf = Vec::with_capacity(4 + 1 + 4 + 4 + 4 + 2 + font_bytes.len());
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.extend_from_slice(&columns.to_le_bytes());
    buf.extend_from_slice(&rows.to_le_bytes());
    buf.extend_from_slice(&frame_rate_hz.to_le_bytes());
    buf.extend_from_slice(&palette_len.to_le_bytes());
    buf.extend_from_slice(font_bytes);
    buf
}

/// Builds one frame message (spec §5): char RAM, then color RAM, then the current palette as
/// RGB24 triples -- sent once per vsync, fixed size for the life of the connection.
pub fn encode_frame(char_ram: &[u8], color_ram: &[u8], palette: &[Rgb24]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(char_ram.len() + color_ram.len() + palette.len() * 3);
    buf.extend_from_slice(char_ram);
    buf.extend_from_slice(color_ram);
    for color in palette {
        buf.push(color.r);
        buf.push(color.g);
        buf.push(color.b);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::font::FONT_BYTES;

    #[test]
    fn header_layout_matches_spec() {
        let font = Font::default();
        let header = encode_header(40, 25, 60, 16, &font);

        assert_eq!(&header[0..4], b"E65D");
        assert_eq!(header[4], 1);
        assert_eq!(&header[5..9], &40u32.to_le_bytes());
        assert_eq!(&header[9..13], &25u32.to_le_bytes());
        assert_eq!(&header[13..17], &60u32.to_le_bytes());
        assert_eq!(&header[17..19], &16u16.to_le_bytes());
        assert_eq!(&header[19..19 + FONT_BYTES], font.as_bytes());
        assert_eq!(header.len(), 19 + FONT_BYTES);
    }

    #[test]
    fn frame_layout_matches_spec() {
        let char_ram = vec![0x41, 0x42, 0x43, 0x44];
        let color_ram = vec![1, 2, 3, 4];
        let palette = vec![Rgb24::new(1, 2, 3), Rgb24::new(4, 5, 6)];

        let frame = encode_frame(&char_ram, &color_ram, &palette);

        assert_eq!(&frame[0..4], char_ram.as_slice());
        assert_eq!(&frame[4..8], color_ram.as_slice());
        assert_eq!(&frame[8..11], &[1, 2, 3]);
        assert_eq!(&frame[11..14], &[4, 5, 6]);
        assert_eq!(frame.len(), 14);
    }

    #[test]
    fn frame_size_is_constant_regardless_of_ram_contents() {
        let char_ram = vec![0u8; 8];
        let color_ram = vec![0u8; 8];
        let palette = vec![Rgb24::new(0, 0, 0); 4];

        let frame_a = encode_frame(&char_ram, &color_ram, &palette);
        let frame_b = encode_frame(&[0xFF; 8], &[0xAA; 8], &[Rgb24::new(255, 255, 255); 4]);

        assert_eq!(frame_a.len(), frame_b.len());
    }
}
