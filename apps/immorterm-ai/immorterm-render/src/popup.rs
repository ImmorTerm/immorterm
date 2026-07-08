//! Popup menu data structures for interactive status bar menus.
//!
//! Popup menus are rendered above the status bar using existing GPU primitives
//! (BgInstance for backgrounds, GlyphInstance for text, DecorInstance for borders).
//! No new shaders or render pipelines are needed.

/// A single item in a popup menu.
#[derive(Debug, Clone)]
pub struct PopupRenderItem {
    pub label: String,
    pub checked: bool,
    pub separator_after: bool,
    pub enabled: bool,
}

/// Popup menu state, ready for rendering.
#[derive(Debug, Clone)]
pub struct PopupRenderData {
    pub items: Vec<PopupRenderItem>,
    pub selected_index: usize,
    pub anchor_col: usize,
    pub width_cols: usize,
    pub visible: bool,
}

impl PopupRenderData {
    /// Compute the display row range this popup occupies (row_start..row_end exclusive).
    /// The popup is positioned above the status bar row.
    pub fn row_range(&self, visible_rows: usize) -> (usize, usize) {
        let padding = 1; // top + bottom
        let total_rows = self.items.len() + padding * 2;
        let row_start = visible_rows.saturating_sub(total_rows);
        (row_start, visible_rows)
    }

    /// Check if a display (col, row) falls within this popup.
    pub fn contains(&self, col: usize, row: usize, visible_rows: usize) -> bool {
        let (row_start, row_end) = self.row_range(visible_rows);
        row >= row_start
            && row < row_end
            && col >= self.anchor_col
            && col < self.anchor_col + self.width_cols
    }

    /// Return which item index a display row maps to, if any.
    pub fn item_at_row(&self, row: usize, visible_rows: usize) -> Option<usize> {
        let (row_start, _) = self.row_range(visible_rows);
        let padding = 1;
        let item_row = row.checked_sub(row_start + padding)?;
        if item_row < self.items.len() {
            Some(item_row)
        } else {
            None
        }
    }
}

/// Actions resulting from popup menu interaction.
#[derive(Debug, Clone)]
pub enum PopupAction {
    SwitchSession(String),
    KillSession(String),
    NewSession,
    SetTheme(usize), // index into THEME_PRESETS
    ToggleAiStats,
    Dismiss,
}
