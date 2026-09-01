//! Wire codec for `LedMatrix`'s external protocol.
//!
//! See `doc/led-matrix-external-protocol.md` for the full specification. Summary: a one-time
//! header ([`encode_header`]) sent immediately when an external transport is attached, followed
//! by a tagged message per matrix swap ([`encode_block`]), per actual palette write
//! ([`encode_palette`]), and per actual power/brightness change ([`encode_power`],
//! [`encode_brightness`]) -- unlike `CharDisplay`'s protocol, there is no single fixed-size
//! per-tick frame, since swaps happen per-matrix rather than in lockstep across the whole device.
//! There is no length prefix on any message -- each tag has exactly one fixed following length --
//! which is safe only because the attached transport's
//! [`Transport::send_bytes`](crate::emulator::transport::Transport::send_bytes) is required to be
//! all-or-nothing.

use super::PIXELS_PER_MATRIX;
use super::compositing::Rgb565;

/// Magic bytes identifying this protocol (spec §4) -- distinct from the CPU trace format's
/// `"E65T"` and `CharDisplay`'s `"E65D"`.
const MAGIC: [u8; 4] = *b"E65M";
/// Protocol version (spec §4), incremented on any wire-incompatible change. Bumped to 2 when the
/// header grew a `columns` field (arrangement-coupling follow-up), replacing the peripheral's own
/// `--arrangement` flag with the device's actual configured arrangement.
const VERSION: u8 = 2;

/// Tag identifying a block message (spec §5.1).
pub const MSG_BLOCK: u8 = 1;
/// Tag identifying a palette message (spec §5.2).
pub const MSG_PALETTE: u8 = 2;
/// Tag identifying a power message (spec §5.3).
pub const MSG_POWER: u8 = 3;
/// Tag identifying a brightness message (spec §5.4).
pub const MSG_BRIGHTNESS: u8 = 4;

/// Builds the one-time header sent immediately on attach (spec §4): magic, version, matrix
/// count, arrangement column count, then the auto-refresh cadence. `columns` is the device's own
/// configured arrangement (design doc §2.2) -- the peripheral derives row count as
/// `matrix_count / columns`, which always divides evenly since the config module validates that
/// invariant at instantiation time.
pub fn encode_header(matrix_count: u8, columns: u8, frame_rate_hz: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + 1 + 1 + 1 + 4);
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.push(matrix_count);
    buf.push(columns);
    buf.extend_from_slice(&frame_rate_hz.to_le_bytes());
    buf
}

/// Builds one block message (spec §5.1): tag, matrix index, then that matrix's raw
/// palette-index pixel bytes -- sent once per swap of that matrix.
pub fn encode_block(matrix_index: u8, pixels: &[u8]) -> Vec<u8> {
    debug_assert_eq!(pixels.len(), PIXELS_PER_MATRIX, "pixels must be exactly one matrix's worth");
    let mut buf = Vec::with_capacity(1 + 1 + pixels.len());
    buf.push(MSG_BLOCK);
    buf.push(matrix_index);
    buf.extend_from_slice(pixels);
    buf
}

/// Builds one palette message (spec §5.2): tag, entry index, then the entry's new value packed
/// as RGB565 -- sent only when `CMD_PALETTE_WRITE`'s effect is actually applied.
pub fn encode_palette(index: u8, color: Rgb565) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 1 + 2);
    buf.push(MSG_PALETTE);
    buf.push(index);
    buf.extend_from_slice(&color.to_packed565().to_le_bytes());
    buf
}

/// Builds one power message (spec §5.3): tag, then the new power-state bitmask -- sent only when
/// `CMD_SET_POWER`'s effect is actually applied.
pub fn encode_power(mask: u8) -> Vec<u8> {
    vec![MSG_POWER, mask]
}

/// Builds one brightness message (spec §5.4): tag, then the new global brightness level -- sent
/// only when `CMD_SET_BRIGHTNESS`'s effect is actually applied.
pub fn encode_brightness(level: u8) -> Vec<u8> {
    vec![MSG_BRIGHTNESS, level]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_matches_spec() {
        let header = encode_header(4, 2, 100);

        assert_eq!(&header[0..4], b"E65M");
        assert_eq!(header[4], 2); // version
        assert_eq!(header[5], 4); // matrix_count
        assert_eq!(header[6], 2); // columns
        assert_eq!(&header[7..11], &100u32.to_le_bytes());
        assert_eq!(header.len(), 11);
    }

    #[test]
    fn block_layout_matches_spec() {
        let pixels = vec![7u8; PIXELS_PER_MATRIX];
        let block = encode_block(3, &pixels);

        assert_eq!(block[0], MSG_BLOCK);
        assert_eq!(block[1], 3);
        assert_eq!(&block[2..], pixels.as_slice());
        assert_eq!(block.len(), 1026);
    }

    #[test]
    fn palette_layout_matches_spec() {
        let color = Rgb565::new(0x1F, 0x3F, 0x00); // max red, max green, no blue
        let message = encode_palette(9, color);

        assert_eq!(message[0], MSG_PALETTE);
        assert_eq!(message[1], 9);
        assert_eq!(&message[2..4], &color.to_packed565().to_le_bytes());
        assert_eq!(message.len(), 4);
    }

    #[test]
    fn palette_packing_places_channels_at_spec_bit_positions() {
        let color = Rgb565::new(0b10101, 0b110011, 0b01010);
        let packed = color.to_packed565();

        assert_eq!(packed, 0b1010_1110_0110_1010);
    }

    #[test]
    fn power_layout_matches_spec() {
        let message = encode_power(0b0110);

        assert_eq!(message[0], MSG_POWER);
        assert_eq!(message[1], 0b0110);
        assert_eq!(message.len(), 2);
    }

    #[test]
    fn brightness_layout_matches_spec() {
        let message = encode_brightness(0x7F);

        assert_eq!(message[0], MSG_BRIGHTNESS);
        assert_eq!(message[1], 0x7F);
        assert_eq!(message.len(), 2);
    }
}
