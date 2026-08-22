//! Parses the display device's palette configuration data (spec §3): a list of RGB24 color
//! triples supplied at configuration time, independent of the device's bus-addressable memory
//! -- color RAM stores indices into this list, resolved during compositing (see
//! `compositing::composite`).
//!
//! Palette files are plain text, one color per line: six hex digits (`RRGGBB`,
//! case-insensitive), with an optional leading `#`. Blank lines are skipped. Text, rather than
//! `font`'s raw bitmap bytes, is the natural format here: a flat list of colors reads and edits
//! by hand the way a glyph bitmap does not.

use super::compositing::Rgb24;
use std::fmt;

/// Minimum number of palette entries (spec §3: "must be non-empty").
pub const MIN_ENTRIES: usize = 1;
/// Maximum number of palette entries (spec §3: "no more than 256 entries", since color RAM
/// indices are a full byte).
pub const MAX_ENTRIES: usize = 256;

/// Text of the bundled default palette: the classic 16-color IBM CGA/VGA text-mode palette --
/// plain RGB triples, not derived from any copyrighted asset.
const DEFAULT_PALETTE_TEXT: &str = include_str!("default_palette.txt");

/// An error parsing or validating palette text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteError {
    /// No non-blank lines were present.
    Empty,
    /// More than [`MAX_ENTRIES`] entries were present.
    TooManyEntries { actual: usize },
    /// A non-blank line was not a valid `RRGGBB` (optionally `#`-prefixed) color.
    InvalidEntry { line: usize, text: String },
}

impl fmt::Display for PaletteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaletteError::Empty =>
                write!(f, "palette must contain at least {MIN_ENTRIES} color"),
            PaletteError::TooManyEntries { actual } =>
                write!(f, "palette must contain at most {MAX_ENTRIES} colors, got {actual}"),
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

/// Parses palette text into a list of colors, validating the count against spec §3's 1-256
/// entry range.
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
    if colors.is_empty() {
        return Err(PaletteError::Empty);
    }
    if colors.len() > MAX_ENTRIES {
        return Err(PaletteError::TooManyEntries { actual: colors.len() });
    }
    Ok(colors)
}

/// Returns the bundled default palette (the classic 16-color IBM CGA/VGA text-mode palette).
pub fn default_palette() -> Vec<Rgb24> {
    parse(DEFAULT_PALETTE_TEXT).expect("bundled default palette must be well-formed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors_with_and_without_hash_prefix() {
        let colors = parse("#FF0000\n00ff00\n0000FF\n").unwrap();
        assert_eq!(colors, vec![Rgb24::new(0xFF, 0, 0), Rgb24::new(0, 0xFF, 0), Rgb24::new(0, 0, 0xFF)]);
    }

    #[test]
    fn skips_blank_lines() {
        let colors = parse("#FF0000\n\n  \n00FF00\n").unwrap();
        assert_eq!(colors.len(), 2);
    }

    #[test]
    fn rejects_empty_input() {
        assert_eq!(parse("").unwrap_err(), PaletteError::Empty);
        assert_eq!(parse("\n\n  \n").unwrap_err(), PaletteError::Empty);
    }

    #[test]
    fn rejects_more_than_256_entries() {
        let text = "000000\n".repeat(257);
        assert!(matches!(parse(&text).unwrap_err(), PaletteError::TooManyEntries { actual: 257 }));
    }

    #[test]
    fn accepts_exactly_256_entries() {
        let text = "000000\n".repeat(256);
        assert_eq!(parse(&text).unwrap().len(), 256);
    }

    #[test]
    fn rejects_malformed_entry() {
        let err = parse("FF0000\nnotacolor\n").unwrap_err();
        assert!(matches!(err, PaletteError::InvalidEntry { line: 2, .. }));
    }

    #[test]
    fn default_palette_has_16_entries() {
        assert_eq!(default_palette().len(), 16);
    }

    #[test]
    fn default_palette_first_entry_is_black() {
        assert_eq!(default_palette()[0], Rgb24::new(0, 0, 0));
    }
}
