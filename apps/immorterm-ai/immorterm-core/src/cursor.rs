//! Cursor state and position tracking.

use serde::{Deserialize, Serialize};

use crate::cell::{CellAttrs, Color};

/// Cursor position and visual state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    /// Column (0-indexed)
    pub col: usize,
    /// Row (0-indexed)
    pub row: usize,
    /// Whether the cursor is visible
    pub visible: bool,
    /// Cursor shape
    pub shape: CursorShape,
    /// Pending wrap — cursor is past the last column but hasn't wrapped yet.
    /// Next printable character will wrap to the next line.
    pub pending_wrap: bool,
    /// CR arrived while cursor was near the line edge (≥75% of terminal width).
    /// If the next byte is LF, the row is marked `soft_wrapped` (app word-wrap).
    /// Cleared on any non-LF input.
    #[serde(default, skip)]
    pub cr_near_edge: bool,
    /// Saved cursor state (ESC 7 / DECSC)
    pub saved: Option<SavedCursor>,
    /// Current SGR attributes applied to new characters
    pub attrs: CellAttrs,
    /// Current foreground color
    pub fg: Color,
    /// Current background color
    pub bg: Color,
    /// Current underline color
    pub underline_color: Color,
}

/// Cursor shape (DECSCUSR).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

/// State saved by DECSC (ESC 7) / restored by DECRC (ESC 8).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedCursor {
    pub col: usize,
    pub row: usize,
    pub attrs: CellAttrs,
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Color,
    pub origin_mode: bool,
    pub pending_wrap: bool,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            col: 0,
            row: 0,
            visible: true,
            shape: CursorShape::Block,
            pending_wrap: false,
            cr_near_edge: false,
            saved: None,
            attrs: CellAttrs::empty(),
            fg: Color::Default,
            bg: Color::Default,
            underline_color: Color::Default,
        }
    }
}

impl Cursor {
    /// Save current cursor state (DECSC).
    pub fn save(&mut self, origin_mode: bool) {
        self.saved = Some(SavedCursor {
            col: self.col,
            row: self.row,
            attrs: self.attrs,
            fg: self.fg,
            bg: self.bg,
            underline_color: self.underline_color,
            origin_mode,
            pending_wrap: self.pending_wrap,
        });
    }

    /// Restore cursor state (DECRC). Returns the saved origin_mode if restore happened.
    pub fn restore(&mut self) -> Option<bool> {
        if let Some(saved) = self.saved.take() {
            self.col = saved.col;
            self.row = saved.row;
            self.attrs = saved.attrs;
            self.fg = saved.fg;
            self.bg = saved.bg;
            self.underline_color = saved.underline_color;
            self.pending_wrap = saved.pending_wrap;
            // Re-store since restore doesn't consume the save point
            let origin_mode = saved.origin_mode;
            self.saved = Some(SavedCursor {
                col: self.col,
                row: self.row,
                attrs: self.attrs,
                fg: self.fg,
                bg: self.bg,
                underline_color: self.underline_color,
                origin_mode,
                pending_wrap: self.pending_wrap,
            });
            Some(origin_mode)
        } else {
            None
        }
    }

    /// Clamp cursor position to grid bounds.
    pub fn clamp(&mut self, cols: usize, rows: usize) {
        if cols > 0 {
            self.col = self.col.min(cols - 1);
        }
        if rows > 0 {
            self.row = self.row.min(rows - 1);
        }
        self.pending_wrap = false;
    }
}
