//! Terminal screen grid — rows × cols of cells.

use serde::{Deserialize, Serialize};

use crate::cell::Cell;

/// Serde helper: skip serializing `content_end_col` when it's 0.
pub fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// A single row in the terminal grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Whether this row has been modified since last render
    pub dirty: bool,
    /// Whether this row soft-wraps to the next row (auto-wrap, not hard newline).
    /// `true` = line continues on next row; `false` = line ends here (hard break).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrapped: bool,
    /// Whether this row was app-wrapped: the application sent `\r\n` at a near-full
    /// line (≥75% of terminal width), indicating word-wrap rather than a hard break.
    /// Detected during `terminal.process()` by tracking cursor position at CR→LF.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub soft_wrapped: bool,
    /// Column *after* the rightmost character written to this row (including spaces).
    /// Updated on every `write_char`; **reset when writing before the current end**
    /// (catches Ink/TUI redraws that overwrite from an earlier position). This gives
    /// a reliable per-row content boundary including trailing spaces. Used by
    /// click-to-cursor to clamp phantom position. `0` means nothing was written.
    #[serde(default, skip_serializing_if = "crate::grid::is_zero")]
    pub content_end_col: usize,
    /// Per-row BiDi direction override: 0=LTR, 1=RTL, 2=Auto.
    /// `None` = inherit global default. Set via SCP escape sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<u8>,
    /// Per-row text alignment override: 0=Left, 1=Right, 2=Center, 3=Auto.
    /// `None` = inherit global default. Set via MCP/OSC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<u8>,
}

impl Row {
    /// Create a new row with `cols` default cells.
    pub fn new(cols: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols],
            dirty: true,
            wrapped: false,
            soft_wrapped: false,
            content_end_col: 0,
            direction: None,
            alignment: None,
        }
    }

    /// Resize this row to `cols` columns, filling new cells with defaults.
    pub fn resize(&mut self, cols: usize) {
        self.cells.resize_with(cols, Cell::default);
        self.dirty = true;
    }

    /// Clear all cells in this row to defaults.
    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            cell.reset();
        }
        self.dirty = true;
        self.wrapped = false;
        self.soft_wrapped = false;
        self.content_end_col = 0;
        self.direction = None;
        self.alignment = None;
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether the row has zero columns (shouldn't happen in practice).
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Whether this row appears to be soft-wrapped (content continues on the next row).
    /// Checks the terminal's auto-wrap flag first (reliable for regular terminals),
    /// then falls back to a fill-ratio heuristic for Ink/React renderers that
    /// pre-wrap text in JS and never trigger terminal auto-wrap.
    pub fn is_soft_wrapped(&self) -> bool {
        if self.wrapped || self.soft_wrapped {
            return true;
        }
        let width = self.cells.len();
        if width == 0 {
            return false;
        }
        let content_end = self.cells.iter()
            .rposition(|c| c.width > 0 && c.grapheme != ' ')
            .map(|p| p + 1)
            .unwrap_or(0);
        content_end * 4 >= width * 3 // content fills ≥75% of row
    }
}

/// The terminal screen buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grid {
    /// Rows of the visible screen area
    rows: Vec<Row>,
    /// Number of columns
    cols: usize,
    /// Number of rows
    num_rows: usize,
    /// Top of the scroll region (0-indexed, inclusive)
    pub scroll_top: usize,
    /// Bottom of the scroll region (0-indexed, inclusive)
    pub scroll_bottom: usize,
}

impl Grid {
    /// Create a new grid with the given dimensions.
    pub fn new(cols: usize, num_rows: usize) -> Self {
        let rows = (0..num_rows).map(|_| Row::new(cols)).collect();
        Self {
            rows,
            cols,
            num_rows,
            scroll_top: 0,
            scroll_bottom: num_rows.saturating_sub(1),
        }
    }

    /// Number of columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Number of rows.
    pub fn num_rows(&self) -> usize {
        self.num_rows
    }

    /// Access a specific cell. Returns None if out of bounds.
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        self.rows.get(row)?.cells.get(col)
    }

    /// Mutably access a specific cell. Returns None if out of bounds.
    pub fn cell_mut(&mut self, row: usize, col: usize) -> Option<&mut Cell> {
        self.rows.get_mut(row)?.cells.get_mut(col)
    }

    /// Access an entire row.
    pub fn row(&self, idx: usize) -> Option<&Row> {
        self.rows.get(idx)
    }

    /// Mutably access an entire row.
    pub fn row_mut(&mut self, idx: usize) -> Option<&mut Row> {
        self.rows.get_mut(idx)
    }

    /// Iterator over all rows.
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter()
    }

    /// Scroll the scroll region up by one line.
    /// Returns the evicted top row (for scrollback).
    pub fn scroll_up(&mut self) -> Row {
        let evicted = self.rows.remove(self.scroll_top);
        self.rows
            .insert(self.scroll_bottom, Row::new(self.cols));
        // Mark all rows in scroll region as dirty
        for r in self.scroll_top..=self.scroll_bottom {
            if let Some(row) = self.rows.get_mut(r) {
                row.dirty = true;
            }
        }
        evicted
    }

    /// Scroll the scroll region down by one line.
    pub fn scroll_down(&mut self) {
        if self.scroll_bottom < self.rows.len() {
            self.rows.remove(self.scroll_bottom);
            self.rows.insert(self.scroll_top, Row::new(self.cols));
            for r in self.scroll_top..=self.scroll_bottom {
                if let Some(row) = self.rows.get_mut(r) {
                    row.dirty = true;
                }
            }
        }
    }

    /// Insert `n` blank lines at `row`, scrolling content down within scroll region.
    pub fn insert_lines(&mut self, at_row: usize, n: usize) {
        for _ in 0..n {
            if self.scroll_bottom < self.rows.len() {
                self.rows.remove(self.scroll_bottom);
            }
            self.rows.insert(at_row, Row::new(self.cols));
        }
        for r in at_row..=self.scroll_bottom {
            if let Some(row) = self.rows.get_mut(r) {
                row.dirty = true;
            }
        }
    }

    /// Delete `n` lines at `row`, scrolling content up within scroll region.
    pub fn delete_lines(&mut self, at_row: usize, n: usize) {
        for _ in 0..n {
            if at_row < self.rows.len() {
                self.rows.remove(at_row);
            }
            if self.scroll_bottom < self.num_rows {
                self.rows
                    .insert(self.scroll_bottom, Row::new(self.cols));
            }
        }
        for r in at_row..=self.scroll_bottom {
            if let Some(row) = self.rows.get_mut(r) {
                row.dirty = true;
            }
        }
    }

    /// Clear all cells in the grid.
    pub fn clear(&mut self) {
        for row in &mut self.rows {
            row.clear();
        }
    }

    /// Erase from cursor to end of screen.
    pub fn erase_below(&mut self, cursor_row: usize, cursor_col: usize) {
        // Erase from cursor to end of current row
        if let Some(row) = self.rows.get_mut(cursor_row) {
            for col in cursor_col..self.cols {
                if let Some(cell) = row.cells.get_mut(col) {
                    cell.reset();
                }
            }
            // Content can't extend past the erase point
            if row.content_end_col > cursor_col {
                row.content_end_col = cursor_col;
            }
            row.dirty = true;
        }
        // Erase all rows below
        for r in (cursor_row + 1)..self.num_rows {
            if let Some(row) = self.rows.get_mut(r) {
                row.clear();
            }
        }
    }

    /// Erase from start of screen to cursor.
    pub fn erase_above(&mut self, cursor_row: usize, cursor_col: usize) {
        // Erase all rows above
        for r in 0..cursor_row {
            if let Some(row) = self.rows.get_mut(r) {
                row.clear();
            }
        }
        // Erase from start of current row to cursor
        if let Some(row) = self.rows.get_mut(cursor_row) {
            for col in 0..=cursor_col.min(self.cols.saturating_sub(1)) {
                if let Some(cell) = row.cells.get_mut(col) {
                    cell.reset();
                }
            }
            // If all content was before the erase point, reset
            if row.content_end_col <= cursor_col + 1 {
                row.content_end_col = 0;
            }
            row.dirty = true;
        }
    }

    /// Erase from cursor to end of line.
    pub fn erase_line_right(&mut self, row: usize, from_col: usize) {
        if let Some(r) = self.rows.get_mut(row) {
            for col in from_col..self.cols {
                if let Some(cell) = r.cells.get_mut(col) {
                    cell.reset();
                }
            }
            // Content can't extend past the erase point
            if r.content_end_col > from_col {
                r.content_end_col = from_col;
            }
            r.dirty = true;
        }
    }

    /// Erase from start of line to cursor.
    pub fn erase_line_left(&mut self, row: usize, to_col: usize) {
        if let Some(r) = self.rows.get_mut(row) {
            for col in 0..=to_col.min(self.cols.saturating_sub(1)) {
                if let Some(cell) = r.cells.get_mut(col) {
                    cell.reset();
                }
            }
            r.dirty = true;
        }
    }

    /// Erase entire line.
    pub fn erase_line(&mut self, row: usize) {
        if let Some(r) = self.rows.get_mut(row) {
            r.clear();
        }
    }

    /// Simple resize without content-aware reflow.
    /// Used for the alternate screen grid and internal grid setup.
    /// For the primary grid, Terminal::resize() performs reflow instead.
    pub fn resize(&mut self, new_cols: usize, new_rows: usize) {
        // Resize existing rows
        for row in &mut self.rows {
            row.resize(new_cols);
        }
        // Add/remove rows
        while self.rows.len() < new_rows {
            self.rows.push(Row::new(new_cols));
        }
        self.rows.truncate(new_rows);

        self.cols = new_cols;
        self.num_rows = new_rows;
        self.scroll_top = 0;
        self.scroll_bottom = new_rows.saturating_sub(1);
    }

    /// Drain all rows out of the grid, leaving it empty.
    pub fn take_rows(&mut self) -> Vec<Row> {
        self.num_rows = 0;
        std::mem::take(&mut self.rows)
    }

    /// Replace grid rows and update dimensions.
    pub fn replace_rows(&mut self, rows: Vec<Row>, cols: usize) {
        self.num_rows = rows.len();
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = self.num_rows.saturating_sub(1);
    }

    /// Current number of physical rows in the grid.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Remove and return the first (top) row.
    pub fn remove_first(&mut self) -> Row {
        self.num_rows = self.num_rows.saturating_sub(1);
        self.rows.remove(0)
    }

    /// Insert a row at the top (position 0).
    pub fn insert_first(&mut self, row: Row) {
        self.rows.insert(0, row);
        self.num_rows = self.rows.len();
    }

    /// Append a row at the bottom.
    pub fn push_row(&mut self, row: Row) {
        self.rows.push(row);
        self.num_rows = self.rows.len();
    }

    /// Remove and return the last (bottom) row.
    pub fn pop_last(&mut self) -> Option<Row> {
        let row = self.rows.pop();
        self.num_rows = self.rows.len();
        row
    }

    /// Finalize grid dimensions and scroll region after external row manipulation.
    pub fn finalize(&mut self, cols: usize, num_rows: usize) {
        // Ensure row count matches target
        while self.rows.len() < num_rows {
            self.rows.push(Row::new(cols));
        }
        self.rows.truncate(num_rows);
        self.cols = cols;
        self.num_rows = num_rows;
        self.scroll_top = 0;
        self.scroll_bottom = num_rows.saturating_sub(1);
    }

    /// Insert blank characters at position, shifting existing chars right.
    pub fn insert_chars(&mut self, row: usize, col: usize, n: usize) {
        if let Some(r) = self.rows.get_mut(row) {
            for _ in 0..n {
                if col < r.cells.len() {
                    r.cells.pop(); // Remove last cell
                    r.cells.insert(col, Cell::default());
                }
            }
            r.dirty = true;
        }
    }

    /// Delete characters at position, shifting remaining chars left.
    pub fn delete_chars(&mut self, row: usize, col: usize, n: usize) {
        if let Some(r) = self.rows.get_mut(row) {
            for _ in 0..n {
                if col < r.cells.len() {
                    r.cells.remove(col);
                    r.cells.push(Cell::default());
                }
            }
            r.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_grid_dimensions() {
        let grid = Grid::new(80, 24);
        assert_eq!(grid.cols(), 80);
        assert_eq!(grid.num_rows(), 24);
        assert_eq!(grid.scroll_top, 0);
        assert_eq!(grid.scroll_bottom, 23);
    }

    #[test]
    fn cell_access() {
        let mut grid = Grid::new(80, 24);
        let cell = grid.cell_mut(0, 0).unwrap();
        cell.grapheme = 'A';
        assert_eq!(grid.cell(0, 0).unwrap().grapheme, 'A');
    }

    #[test]
    fn scroll_up_evicts_top_row() {
        let mut grid = Grid::new(80, 3);
        grid.cell_mut(0, 0).unwrap().grapheme = 'A';
        grid.cell_mut(1, 0).unwrap().grapheme = 'B';
        grid.cell_mut(2, 0).unwrap().grapheme = 'C';

        let evicted = grid.scroll_up();
        assert_eq!(evicted.cells[0].grapheme, 'A');
        assert_eq!(grid.cell(0, 0).unwrap().grapheme, 'B');
        assert_eq!(grid.cell(1, 0).unwrap().grapheme, 'C');
        assert_eq!(grid.cell(2, 0).unwrap().grapheme, ' '); // New blank row
    }

    #[test]
    fn erase_below() {
        let mut grid = Grid::new(10, 3);
        for r in 0..3 {
            for c in 0..10 {
                grid.cell_mut(r, c).unwrap().grapheme = 'X';
            }
        }
        grid.erase_below(1, 5);
        // Row 0 untouched
        assert_eq!(grid.cell(0, 0).unwrap().grapheme, 'X');
        // Row 1, cols 0-4 untouched
        assert_eq!(grid.cell(1, 4).unwrap().grapheme, 'X');
        // Row 1, col 5+ erased
        assert_eq!(grid.cell(1, 5).unwrap().grapheme, ' ');
        // Row 2 fully erased
        assert_eq!(grid.cell(2, 0).unwrap().grapheme, ' ');
    }

    #[test]
    fn resize_grid() {
        let mut grid = Grid::new(80, 24);
        grid.cell_mut(0, 0).unwrap().grapheme = 'Z';
        grid.resize(120, 40);
        assert_eq!(grid.cols(), 120);
        assert_eq!(grid.num_rows(), 40);
        assert_eq!(grid.cell(0, 0).unwrap().grapheme, 'Z');
        assert_eq!(grid.scroll_bottom, 39);
    }
}
