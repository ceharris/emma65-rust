//! Parses a display device's `palette=` configuration file (spec §3): a list of RGB24 color
//! triples supplied at configuration time, independent of the device's bus-addressable memory
//! -- color RAM stores indices into this list, resolved during compositing.
//!
//! Palette files are plain text, one color per line: six hex digits (`RRGGBB`,
//! case-insensitive), with an optional leading `#`. Blank lines are skipped.
//!
//! The entry count must be exactly [`SMALL_PALETTE_ENTRIES`] (16) or [`LARGE_PALETTE_ENTRIES`]
//! (256) -- not merely non-empty and no more than 256 as the device itself would tolerate (spec
//! §3, §4.1: `CharDisplay`'s modulo index resolution works for any 1-256 length). Any other
//! count is rejected here as almost certainly a user mistake rather than an intentional
//! custom-size palette; there's no real use case for, say, a hand-authored 200-entry list.
//!
//! Other formats were considered and rejected:
//! - **Inline TOML/JSON array** (`palette = ["#000000", ...]`) directly in the device's
//!   attribute table, avoiding a separate file -- rejected because up to 256 entries would bloat
//!   a single device line/table, and every other file-shaped attribute this device (and `rom`)
//!   accepts (`image=`, `labels=`, `font=`) is already a path to a separate file, not inline
//!   structured data; a palette should follow that same convention rather than being a special
//!   case.
//! - **CSV** (`R,G,B` per line) -- rejected as more verbose than hex triples for no real benefit;
//!   `#RRGGBB` is already the idiomatic way colors are written by hand (CSS/web convention).
//! - **Binary** (raw RGB triples, mirroring `font`'s raw bitmap bytes) -- rejected because a
//!   palette is far more likely to be hand-authored or tweaked than a font bitmap is; plain text
//!   is directly editable without a hex editor, and parsing overhead is irrelevant at up to 256
//!   entries.
//! - **An existing palette-tool format** (e.g. GIMP `.gpl`) -- rejected to avoid pulling in a
//!   parser for a foreign spec just to support hand-authoring, which a flat text file already
//!   serves well enough.

use crate::emulator::device::display::compositing::Rgb24;
use std::fmt;

/// A "low-color" palette entry count -- one of the two counts [`parse`] accepts.
pub const SMALL_PALETTE_ENTRIES: usize = 16;
/// A "full-range" palette entry count, using every value an 8-bit color-RAM index can hold --
/// the other count [`parse`] accepts.
pub const LARGE_PALETTE_ENTRIES: usize = 256;
/// Entry counts [`parse`] accepts.
pub const ALLOWED_ENTRY_COUNTS: [usize; 2] = [SMALL_PALETTE_ENTRIES, LARGE_PALETTE_ENTRIES];

/// An error parsing or validating palette text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteError {
    /// Entry count was not one of [`ALLOWED_ENTRY_COUNTS`].
    InvalidCount { actual: usize },
    /// A non-blank line was not a valid `RRGGBB` (optionally `#`-prefixed) color.
    InvalidEntry { line: usize, text: String },
}

impl fmt::Display for PaletteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaletteError::InvalidCount { actual } =>
                write!(f, "palette must contain exactly {SMALL_PALETTE_ENTRIES} or {LARGE_PALETTE_ENTRIES} colors, got {actual}"),
            PaletteError::InvalidEntry { line, text } =>
                write!(f, "invalid color on line {line}: {text:?} (expected 6 hex digits, optionally prefixed with '#')"),
        }
    }
}

impl std::error::Error for PaletteError {}

fn parse_color(text: &str) -> Option<Rgb24> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb24::new(r, g, b))
}

/// Parses palette text into a list of colors, requiring the entry count to be one of
/// [`ALLOWED_ENTRY_COUNTS`].
pub fn parse(text: &str) -> Result<Vec<Rgb24>, PaletteError> {
    let mut colors = Vec::new();
    for (i, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let color = parse_color(line)
            .ok_or_else(|| PaletteError::InvalidEntry { line: i + 1, text: line.to_string() })?;
        colors.push(color);
    }
    if !ALLOWED_ENTRY_COUNTS.contains(&colors.len()) {
        return Err(PaletteError::InvalidCount { actual: colors.len() });
    }
    Ok(colors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sixteen_entry_text() -> String {
        // First two entries exercise both prefixed and unprefixed, mixed-case hex; the rest are
        // filler so the count lands on the accepted 16.
        let mut text = String::from("#FF0000\n00ff00\n");
        text.push_str(&"0000FF\n".repeat(14));
        text
    }

    #[test]
    fn parses_hex_colors_with_and_without_hash_prefix() {
        let colors = parse(&sixteen_entry_text()).unwrap();
        assert_eq!(colors.len(), 16);
        assert_eq!(colors[0], Rgb24::new(0xFF, 0, 0));
        assert_eq!(colors[1], Rgb24::new(0, 0xFF, 0));
        assert_eq!(colors[2], Rgb24::new(0, 0, 0xFF));
    }

    #[test]
    fn skips_blank_lines() {
        let mut text = sixteen_entry_text();
        text.push_str("\n  \n");
        let colors = parse(&text).unwrap();
        assert_eq!(colors.len(), 16);
    }

    #[test]
    fn rejects_empty_input() {
        assert_eq!(parse("").unwrap_err(), PaletteError::InvalidCount { actual: 0 });
        assert_eq!(parse("\n\n  \n").unwrap_err(), PaletteError::InvalidCount { actual: 0 });
    }

    #[test]
    fn rejects_counts_other_than_16_or_256() {
        for count in [1, 3, 15, 17, 200, 255, 257] {
            let text = "000000\n".repeat(count);
            assert_eq!(parse(&text).unwrap_err(), PaletteError::InvalidCount { actual: count });
        }
    }

    #[test]
    fn accepts_exactly_16_entries() {
        let text = "000000\n".repeat(16);
        assert_eq!(parse(&text).unwrap().len(), 16);
    }

    #[test]
    fn accepts_exactly_256_entries() {
        let text = "000000\n".repeat(256);
        assert_eq!(parse(&text).unwrap().len(), 256);
    }

    #[test]
    fn rejects_malformed_entry() {
        let mut text = String::from("FF0000\nnotacolor\n");
        text.push_str(&"000000\n".repeat(14));
        let err = parse(&text).unwrap_err();
        assert!(matches!(err, PaletteError::InvalidEntry { line: 2, .. }));
    }
}
