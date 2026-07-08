//! Terminal cell representation — Unicode-native with true color support.
//!
//! Replaces the C `struct mchar` (image+font+mbcs charset machinery).
//! No charset conversion needed — Rust is Unicode-native.

use serde::{Deserialize, Serialize};

use crate::expression::ExpressionMeta;

/// A single cell in the terminal grid.
///
/// Most cells are default spaces — `skip_serializing_if` annotations ensure
/// that default-valued fields are omitted from JSON, dramatically reducing
/// snapshot and scrollback payload sizes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    /// The character displayed in this cell.
    /// For wide characters (CJK/emoji), the first cell holds the char
    /// and continuation cells have `width = 0`.
    pub grapheme: char,

    /// Visual attributes (bold, italic, etc.)
    #[serde(default, skip_serializing_if = "CellAttrs::is_empty")]
    pub attrs: CellAttrs,

    /// Foreground color
    #[serde(default, skip_serializing_if = "Color::is_default")]
    pub fg: Color,

    /// Background color
    #[serde(default, skip_serializing_if = "Color::is_default")]
    pub bg: Color,

    /// Underline color (modern terminals support colored underlines)
    #[serde(default, skip_serializing_if = "Color::is_default")]
    pub underline_color: Color,

    /// Cell width: 1=normal, 2=wide (CJK/emoji), 0=continuation of wide char
    #[serde(default = "default_width", skip_serializing_if = "is_default_width")]
    pub width: u8,

    /// OSC 8 hyperlink ID (0 = no hyperlink)
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub hyperlink_id: u16,

    /// AI expression metadata — packed u16 with confidence, danger, mood, animation.
    /// Default (0) = no expression, render normally. Set by the Expression Protocol
    /// when AI output flows through the terminal.
    #[serde(default, skip_serializing_if = "ExpressionMeta::is_none")]
    pub expression: ExpressionMeta,
}

fn default_width() -> u8 { 1 }
fn is_default_width(v: &u8) -> bool { *v == 1 }
fn is_zero_u16(v: &u16) -> bool { *v == 0 }

impl Default for Cell {
    fn default() -> Self {
        Self {
            grapheme: ' ',
            attrs: CellAttrs::empty(),
            fg: Color::Default,
            bg: Color::Default,
            underline_color: Color::Default,
            width: 1,
            hyperlink_id: 0,
            expression: ExpressionMeta::NONE,
        }
    }
}

impl Cell {
    /// Create a cell with the given character and default attributes.
    pub fn with_char(c: char) -> Self {
        Self {
            grapheme: c,
            ..Default::default()
        }
    }

    /// Reset this cell to default (space, no attrs, default colors).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Returns true if this cell is in the default state (space, no attrs, default colors).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Returns true if this is a continuation cell of a wide character.
    pub fn is_wide_continuation(&self) -> bool {
        self.width == 0
    }
}

/// Terminal color — supports default, indexed (256), and true color (24-bit RGB).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Color {
    /// Terminal default color
    #[default]
    Default,
    /// 256-color palette index (0-255)
    Indexed(u8),
    /// True color RGB
    Rgb(u8, u8, u8),
}

impl Color {
    /// Returns true if this is the default color (for serde skip).
    pub fn is_default(&self) -> bool {
        matches!(self, Color::Default)
    }
}

bitflags::bitflags! {
    /// Cell visual attributes as a compact bitfield.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct CellAttrs: u16 {
        const BOLD          = 0b0000_0000_0001;
        const DIM           = 0b0000_0000_0010;
        const ITALIC        = 0b0000_0000_0100;
        const UNDERLINE     = 0b0000_0000_1000;
        const BLINK         = 0b0000_0001_0000;
        const INVERSE       = 0b0000_0010_0000;
        const HIDDEN        = 0b0000_0100_0000;
        const STRIKETHROUGH = 0b0000_1000_0000;
        const DOUBLE_UNDERLINE = 0b0001_0000_0000;
        const CURLY_UNDERLINE  = 0b0010_0000_0000;
        const DOTTED_UNDERLINE = 0b0100_0000_0000;
        const DASHED_UNDERLINE = 0b1000_0000_0000;
        /// This cell's digit/#/* is the base of an emoji keycap sequence
        /// (followed by U+20E3 in the stream, e.g. 1️⃣). Set at write time so
        /// the fact survives scrollback eviction — the combining-marks side
        /// table only covers live-grid rows. Renderers route these cells to
        /// the DOM emoji overlay instead of drawing the bare digit.
        const KEYCAP = 0b0001_0000_0000_0000;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cell_is_space() {
        let cell = Cell::default();
        assert_eq!(cell.grapheme, ' ');
        assert_eq!(cell.width, 1);
        assert_eq!(cell.fg, Color::Default);
        assert_eq!(cell.bg, Color::Default);
        assert!(cell.attrs.is_empty());
    }

    #[test]
    fn wide_continuation() {
        let mut cell = Cell::default();
        cell.width = 0;
        assert!(cell.is_wide_continuation());
    }

    #[test]
    fn cell_with_char() {
        let cell = Cell::with_char('A');
        assert_eq!(cell.grapheme, 'A');
        assert_eq!(cell.width, 1);
    }

    #[test]
    fn cell_reset() {
        let mut cell = Cell::with_char('X');
        cell.fg = Color::Rgb(255, 0, 0);
        cell.attrs = CellAttrs::BOLD | CellAttrs::ITALIC;
        cell.reset();
        assert_eq!(cell, Cell::default());
    }
}
