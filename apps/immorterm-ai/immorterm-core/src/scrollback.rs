//! Scrollback buffer — ring buffer of evicted rows.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::grid::Row;

/// Ring buffer for scrollback history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scrollback {
    /// The stored rows (newest at back)
    rows: VecDeque<Row>,
    /// Maximum number of rows to retain
    max_lines: usize,
    /// Total rows evicted from the front over the lifetime of this buffer.
    /// Monotonically increases.
    #[serde(default)]
    total_evicted: u64,
    /// Net index shift applied to all *existing* rows over time:
    ///   +1 per row prepended via `push_front` (existing rows shift DOWN
    ///      one index to make room at the front — idx += 1)
    ///   -1 per row evicted via `push` when the buffer is at capacity
    ///      (existing rows shift UP one index as pop_front removes idx 0
    ///      and everything slides — idx -= 1)
    ///   +0 for regular `push` below capacity (new row appended, no shift)
    /// Used by consumers that need a stable absolute line identifier:
    ///   line_id = scrollback_idx_at_creation - net_shift_at_creation
    /// Later: current_idx = line_id + net_shift_now.
    /// Handles BOTH eviction (where `total_evicted` also grows) and daemon
    /// scrollback prepends (where eviction never happens but existing
    /// indices still shift up).
    #[serde(default)]
    net_shift: i64,
}

impl Scrollback {
    /// Create a new scrollback buffer with the given capacity.
    pub fn new(max_lines: usize) -> Self {
        Self {
            rows: VecDeque::with_capacity(max_lines.min(8192)),
            max_lines,
            total_evicted: 0,
            net_shift: 0,
        }
    }

    /// Push a row into the scrollback. Evicts oldest if at capacity.
    pub fn push(&mut self, row: Row) {
        if self.rows.len() >= self.max_lines {
            self.rows.pop_front();
            self.total_evicted = self.total_evicted.saturating_add(1);
            self.net_shift = self.net_shift.saturating_sub(1);
        }
        self.rows.push_back(row);
    }

    /// Total rows evicted from this buffer over its lifetime.
    pub fn total_evicted(&self) -> u64 {
        self.total_evicted
    }

    /// Net index shift applied to all *existing* rows since buffer creation.
    /// See field doc for the line_id formula consumers use.
    pub fn net_shift(&self) -> i64 {
        self.net_shift
    }

    /// Number of rows in scrollback.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the scrollback is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Access a row by index (0 = oldest).
    pub fn get(&self, idx: usize) -> Option<&Row> {
        self.rows.get(idx)
    }

    /// Maximum capacity.
    pub fn max_lines(&self) -> usize {
        self.max_lines
    }

    /// Set new max capacity. Evicts oldest rows if shrinking.
    pub fn set_max_lines(&mut self, max_lines: usize) {
        self.max_lines = max_lines;
        while self.rows.len() > self.max_lines {
            self.rows.pop_front();
            self.total_evicted = self.total_evicted.saturating_add(1);
            self.net_shift = self.net_shift.saturating_sub(1);
        }
    }

    /// Remove and return the most recent row (back of the deque).
    /// Used during resize to pull scrollback rows back into the grid.
    pub fn pop_back(&mut self) -> Option<Row> {
        self.rows.pop_back()
    }

    /// Drain all rows out as a Vec (oldest first), leaving the buffer empty.
    pub fn take_all(&mut self) -> Vec<Row> {
        self.rows.drain(..).collect()
    }

    /// Clear all scrollback. Treats every retained row as evicted so that
    /// absolute line identifiers built from the counters remain monotonic
    /// across clears. Orphans any existing comment anchors (their line_ids
    /// will resolve to negative current_idx after this).
    pub fn clear(&mut self) {
        let len = self.rows.len() as u64;
        self.total_evicted = self.total_evicted.saturating_add(len);
        self.net_shift = self.net_shift.saturating_sub(len as i64);
        self.rows.clear();
    }

    /// Iterator over rows (oldest first).
    pub fn iter(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter()
    }

    /// Return a slice of rows `[offset..offset+count]` (0 = oldest).
    /// Returns fewer rows if the range extends past the buffer.
    pub fn range(&self, offset: usize, count: usize) -> Vec<&Row> {
        let start = offset.min(self.rows.len());
        let end = (offset + count).min(self.rows.len());
        (start..end).filter_map(|i| self.rows.get(i)).collect()
    }

    /// Push a row to the front (oldest end) of the scrollback.
    /// Used by the WASM client when prepending on-demand scrollback rows
    /// fetched from the daemon. No capacity eviction — daemon-fetched rows
    /// extend the buffer beyond `max_lines` because the daemon already
    /// bounds the total, and evicting the newest (back) rows would cause
    /// circular scrollback where the user sees the same content in a loop.
    pub fn push_front(&mut self, row: Row) {
        self.rows.push_front(row);
        // Every existing row's scrollback_idx just went up by 1.
        self.net_shift = self.net_shift.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_get() {
        let mut sb = Scrollback::new(100);
        let mut row = Row::new(10);
        row.cells[0].grapheme = 'A';
        sb.push(row);
        assert_eq!(sb.len(), 1);
        assert_eq!(sb.get(0).unwrap().cells[0].grapheme, 'A');
    }

    #[test]
    fn ring_buffer_eviction() {
        let mut sb = Scrollback::new(3);
        for i in 0..5u8 {
            let mut row = Row::new(1);
            row.cells[0].grapheme = (b'A' + i) as char;
            sb.push(row);
        }
        assert_eq!(sb.len(), 3);
        // Should have C, D, E (A and B evicted)
        assert_eq!(sb.get(0).unwrap().cells[0].grapheme, 'C');
        assert_eq!(sb.get(1).unwrap().cells[0].grapheme, 'D');
        assert_eq!(sb.get(2).unwrap().cells[0].grapheme, 'E');
    }

    #[test]
    fn range_returns_slice() {
        let mut sb = Scrollback::new(100);
        for i in 0..10u8 {
            let mut row = Row::new(1);
            row.cells[0].grapheme = (b'A' + i) as char;
            sb.push(row);
        }
        let slice = sb.range(3, 4);
        assert_eq!(slice.len(), 4);
        assert_eq!(slice[0].cells[0].grapheme, 'D');
        assert_eq!(slice[3].cells[0].grapheme, 'G');
    }

    #[test]
    fn range_clamps_to_bounds() {
        let mut sb = Scrollback::new(100);
        for i in 0..5u8 {
            let mut row = Row::new(1);
            row.cells[0].grapheme = (b'A' + i) as char;
            sb.push(row);
        }
        // Request beyond end
        let slice = sb.range(3, 100);
        assert_eq!(slice.len(), 2); // D, E
        // Request fully past end
        let slice = sb.range(100, 10);
        assert_eq!(slice.len(), 0);
    }

    #[test]
    fn push_front_prepends() {
        let mut sb = Scrollback::new(100);
        let mut row_a = Row::new(1);
        row_a.cells[0].grapheme = 'A';
        sb.push(row_a);

        let mut row_z = Row::new(1);
        row_z.cells[0].grapheme = 'Z';
        sb.push_front(row_z);

        assert_eq!(sb.len(), 2);
        assert_eq!(sb.get(0).unwrap().cells[0].grapheme, 'Z'); // prepended
        assert_eq!(sb.get(1).unwrap().cells[0].grapheme, 'A'); // original
    }

    /// Verify that the renderer formula `content_idx = (sb_len + display_row) - scroll_offset`
    /// stays correct after prepending rows WITHOUT adjusting scroll_offset.
    /// This proves the old `scroll_offset += count` was wrong (double-compensation).
    #[test]
    fn prepend_does_not_need_scroll_offset_adjustment() {
        let cols = 10;
        let mut sb = Scrollback::new(1000);

        // Build initial scrollback: rows labeled A..E (5 rows)
        for i in 0..5u8 {
            let mut row = Row::new(cols);
            row.cells[0].grapheme = (b'A' + i) as char;
            sb.push(row);
        }
        // sb = [A, B, C, D, E], len = 5

        // Simulate scroll_offset = 3 (viewing 3 rows above live)
        let scroll_offset: usize = 3;
        // Renderer: display_row 0 → content_idx = (5 + 0) - 3 = 2 → sb[2] = 'C'
        let content_idx = (sb.len() + 0).saturating_sub(scroll_offset);
        assert_eq!(sb.get(content_idx).unwrap().cells[0].grapheme, 'C');

        // Prepend 10 rows at the front (older history)
        for i in 0..10u8 {
            let mut row = Row::new(cols);
            row.cells[0].grapheme = (b'a' + i) as char;
            sb.push_front(row);
        }
        // sb = [j, i, h, g, f, e, d, c, b, a, A, B, C, D, E], len = 15
        // 'C' is now at index 12

        // With scroll_offset UNCHANGED (still 3), renderer formula:
        // content_idx = (15 + 0) - 3 = 12 → sb[12] = 'C' ✓ Same content!
        let content_idx_after = (sb.len() + 0).saturating_sub(scroll_offset);
        assert_eq!(sb.get(content_idx_after).unwrap().cells[0].grapheme, 'C',
            "viewport should show same content after prepend without scroll_offset adjustment");

        // Prove the OLD formula (scroll_offset += count) was wrong:
        let bad_offset = scroll_offset + 10; // old code: += count
        let bad_idx = (sb.len() + 0).saturating_sub(bad_offset);
        assert_ne!(sb.get(bad_idx).unwrap().cells[0].grapheme, 'C',
            "old formula should NOT show same content — it jumps to ancient history");
    }

    /// Verify: user at live view (offset=0), empty scrollback, first scroll triggers fetch.
    /// After prepend, user should still be near live view — NOT at the beginning.
    #[test]
    fn prepend_from_empty_scrollback_stays_near_live() {
        let cols = 10;
        let sb = Scrollback::new(1000);
        assert_eq!(sb.len(), 0);

        // User scrolls up 3 lines, but scrollback is empty → clamped to 0
        let scroll_offset: usize = 0;
        let scroll_deficit: usize = 3; // wanted 3, got 0

        // Daemon sends 100 rows
        let mut sb = sb;
        for i in 0..100u16 {
            let mut row = Row::new(cols);
            row.cells[0].grapheme = if i < 26 { (b'A' + i as u8) as char } else { '.' };
            sb.push_front(row);
        }

        // Apply deficit (the new logic in prepend_scrollback):
        let new_offset = (scroll_offset + scroll_deficit).min(sb.len());
        assert_eq!(new_offset, 3, "user should be 3 rows above live, not 100");

        // Old bug: scroll_offset += count would give 100 (= sb.len()) → oldest row
        let bad_offset = scroll_offset + 100;
        assert_eq!(bad_offset, 100);
        let bad_idx = (sb.len() + 0).saturating_sub(bad_offset);
        assert_eq!(bad_idx, 0, "old code would show scrollback[0] = beginning");
    }
}
