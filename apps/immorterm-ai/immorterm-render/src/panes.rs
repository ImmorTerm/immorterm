//! Multi-pane layout engine for Agent Teams View.
//!
//! Computes how N agent panes should be arranged on a surface,
//! with auto-layout (1=full, 2=vertical split, 3=2+1, 4=2x2, etc.).
//! Each pane gets a pixel rect, terminal dimensions, label, and accent color.

/// A positioned pane within the team view surface.
#[derive(Debug, Clone)]
pub struct PaneRect {
    /// Pixel X offset from surface origin.
    pub x: f32,
    /// Pixel Y offset from surface origin.
    pub y: f32,
    /// Pixel width of the pane.
    pub width: f32,
    /// Pixel height of the pane (excluding header).
    pub height: f32,
    /// Terminal columns that fit in this pane.
    pub cols: usize,
    /// Terminal rows that fit in this pane.
    pub rows: usize,
    /// Display label (member name).
    pub label: String,
    /// Accent color for border + header (RGBA).
    pub accent_color: [f32; 4],
    /// Header height in pixels (for member name + status).
    pub header_height: f32,
}

impl PaneRect {
    /// Total height including header.
    pub fn total_height(&self) -> f32 {
        self.height + self.header_height
    }

    /// The pixel Y where terminal content starts (below header).
    pub fn content_y(&self) -> f32 {
        self.y + self.header_height
    }
}

/// Layout for multiple panes on a surface.
#[derive(Debug, Clone)]
pub struct PaneLayout {
    /// Arranged panes.
    pub panes: Vec<PaneRect>,
    /// Index of the currently focused pane.
    pub focused_index: usize,
}

/// Gap between panes in pixels.
const PANE_GAP: f32 = 2.0;
/// Height of the pane header in pixels.
const HEADER_HEIGHT: f32 = 20.0;
/// Border thickness around each pane.
pub const PANE_BORDER: f32 = 2.0;

impl PaneLayout {
    /// Auto-arrange N panes on a surface.
    ///
    /// Layout strategy:
    /// - 1 pane: full surface
    /// - 2 panes: vertical split (side by side)
    /// - 3 panes: top row 2, bottom row 1 (centered)
    /// - 4 panes: 2x2 grid
    /// - 5-6 panes: 3-column, 2 rows
    /// - 7-9 panes: 3x3 grid
    ///
    /// Each pane gets a header row for the agent name + status.
    pub fn auto_arrange(
        labels: &[(String, [f32; 4])], // (name, accent_color)
        surface_w: f32,
        surface_h: f32,
        cell_w: f32,
        cell_h: f32,
    ) -> Self {
        let n = labels.len();
        if n == 0 {
            return Self {
                panes: vec![],
                focused_index: 0,
            };
        }

        let (grid_cols, grid_rows) = grid_dimensions(n);
        let pane_w = (surface_w - PANE_GAP * (grid_cols as f32 + 1.0)) / grid_cols as f32;
        let pane_total_h =
            (surface_h - PANE_GAP * (grid_rows as f32 + 1.0)) / grid_rows as f32;
        let pane_content_h = pane_total_h - HEADER_HEIGHT;

        let terminal_cols = ((pane_w - PANE_BORDER * 2.0) / cell_w).floor() as usize;
        let terminal_rows = ((pane_content_h - PANE_BORDER * 2.0) / cell_h).floor() as usize;

        let mut panes = Vec::with_capacity(n);
        let mut idx = 0;

        for row in 0..grid_rows {
            // How many panes in this row?
            let panes_in_row = if row == grid_rows - 1 {
                // Last row gets remaining panes
                n - idx
            } else {
                grid_cols
            };

            // Center the row if it has fewer panes than grid_cols
            let row_width = panes_in_row as f32 * pane_w
                + (panes_in_row as f32 - 1.0).max(0.0) * PANE_GAP;
            let row_x_offset = (surface_w - row_width) / 2.0;

            for col in 0..panes_in_row {
                if idx >= n {
                    break;
                }

                let x = row_x_offset + col as f32 * (pane_w + PANE_GAP);
                let y = PANE_GAP + row as f32 * (pane_total_h + PANE_GAP);

                let (label, accent) = &labels[idx];

                panes.push(PaneRect {
                    x,
                    y,
                    width: pane_w,
                    height: pane_content_h,
                    cols: terminal_cols.max(1),
                    rows: terminal_rows.max(1),
                    label: label.clone(),
                    accent_color: *accent,
                    header_height: HEADER_HEIGHT,
                });

                idx += 1;
            }
        }

        Self {
            panes,
            focused_index: 0,
        }
    }

    /// Cycle focus to the next pane. Returns the new focused index.
    pub fn cycle_focus(&mut self) -> usize {
        if !self.panes.is_empty() {
            self.focused_index = (self.focused_index + 1) % self.panes.len();
        }
        self.focused_index
    }

    /// Set focus to a specific pane by index.
    pub fn set_focus(&mut self, index: usize) {
        if index < self.panes.len() {
            self.focused_index = index;
        }
    }

    /// Find which pane contains a pixel coordinate (for mouse clicks).
    pub fn pane_at(&self, px: f32, py: f32) -> Option<usize> {
        for (i, pane) in self.panes.iter().enumerate() {
            if px >= pane.x
                && px < pane.x + pane.width
                && py >= pane.y
                && py < pane.y + pane.total_height()
            {
                return Some(i);
            }
        }
        None
    }

    /// Get the focused pane, if any.
    pub fn focused_pane(&self) -> Option<&PaneRect> {
        self.panes.get(self.focused_index)
    }
}

/// Compute grid dimensions (cols, rows) for N panes.
fn grid_dimensions(n: usize) -> (usize, usize) {
    match n {
        0 => (0, 0),
        1 => (1, 1),
        2 => (2, 1),
        3 => (2, 2), // 2 top + 1 bottom (centered)
        4 => (2, 2),
        5 | 6 => (3, 2),
        7..=9 => (3, 3),
        _ => {
            // General case: ceil(sqrt(n)) columns
            let cols = (n as f64).sqrt().ceil() as usize;
            let rows = n.div_ceil(cols);
            (cols, rows)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_pane() {
        let labels = vec![("main".into(), [1.0, 1.0, 1.0, 1.0])];
        let layout = PaneLayout::auto_arrange(&labels, 1920.0, 1080.0, 8.0, 16.0);
        assert_eq!(layout.panes.len(), 1);
        assert!(layout.panes[0].cols > 200);
        assert!(layout.panes[0].rows > 50);
    }

    #[test]
    fn test_four_panes() {
        let labels: Vec<_> = ["lead", "researcher", "coder", "tester"]
            .iter()
            .map(|n| (n.to_string(), [0.5, 0.5, 1.0, 1.0]))
            .collect();
        let layout = PaneLayout::auto_arrange(&labels, 1920.0, 1080.0, 8.0, 16.0);
        assert_eq!(layout.panes.len(), 4);

        // All panes should have reasonable dimensions
        for pane in &layout.panes {
            assert!(pane.cols > 50, "cols={} too small", pane.cols);
            assert!(pane.rows > 20, "rows={} too small", pane.rows);
        }

        // No overlap
        for i in 0..4 {
            for j in (i + 1)..4 {
                let a = &layout.panes[i];
                let b = &layout.panes[j];
                let overlap_x = a.x < b.x + b.width && a.x + a.width > b.x;
                let overlap_y =
                    a.y < b.y + b.total_height() && a.y + a.total_height() > b.y;
                assert!(
                    !(overlap_x && overlap_y),
                    "panes {} and {} overlap",
                    i,
                    j
                );
            }
        }
    }

    #[test]
    fn test_focus_cycling() {
        let labels: Vec<_> = (0..3)
            .map(|i| (format!("agent-{}", i), [1.0, 0.0, 0.0, 1.0]))
            .collect();
        let mut layout = PaneLayout::auto_arrange(&labels, 800.0, 600.0, 8.0, 16.0);
        assert_eq!(layout.focused_index, 0);
        layout.cycle_focus();
        assert_eq!(layout.focused_index, 1);
        layout.cycle_focus();
        assert_eq!(layout.focused_index, 2);
        layout.cycle_focus();
        assert_eq!(layout.focused_index, 0); // wraps
    }

    #[test]
    fn test_pane_at_click() {
        let labels = vec![
            ("a".into(), [1.0, 0.0, 0.0, 1.0]),
            ("b".into(), [0.0, 1.0, 0.0, 1.0]),
        ];
        let layout = PaneLayout::auto_arrange(&labels, 800.0, 600.0, 8.0, 16.0);
        // Click in the left half should be pane 0
        let left = layout.pane_at(100.0, 100.0);
        assert_eq!(left, Some(0));
        // Click in the right half should be pane 1
        let right = layout.pane_at(600.0, 100.0);
        assert_eq!(right, Some(1));
    }

    #[test]
    fn test_grid_dimensions() {
        assert_eq!(grid_dimensions(1), (1, 1));
        assert_eq!(grid_dimensions(2), (2, 1));
        assert_eq!(grid_dimensions(3), (2, 2));
        assert_eq!(grid_dimensions(4), (2, 2));
        assert_eq!(grid_dimensions(5), (3, 2));
        assert_eq!(grid_dimensions(9), (3, 3));
    }
}
