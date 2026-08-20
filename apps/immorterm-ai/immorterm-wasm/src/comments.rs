//! Inline comments feature.
//!
//! The user can select text in the terminal, right-click → "Add comment",
//! type a comment in a floating editor, and see a numbered sidebar pill
//! anchored to the selection. Multiple comments accumulate. When the user
//! sends their next prompt, all staged comments get serialized into a
//! beautiful citation block that is prepended to the prompt text and
//! pasted into the PTY as one message.
//!
//! This module owns the *data*: the list of comments, their stable line
//! identifiers, and the logic that resolves each comment to a visible
//! display row (or marks it orphaned when its anchor row has scrolled
//! off-screen or been evicted from scrollback).

use serde::{Deserialize, Serialize};

/// A single inline comment anchored to a selection in terminal output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    /// Monotonically assigned id — never reused within a session.
    pub id: u32,

    /// Stable absolute line identifier derived from
    ///   line_id = content_idx_at_creation - scrollback.net_shift_at_creation
    /// Survives BOTH scrollback eviction (net_shift decreases, `total_evicted`
    /// grows) and daemon scrollback prepends (net_shift increases). Can be
    /// negative after enough prepends — that's fine, it's just an identifier.
    pub line_id: i64,

    /// Column range on the anchor row the user originally selected.
    /// For multi-row selections, the pill anchors to the *first* row of
    /// the selection and `col_end` is clamped to that row's length.
    pub col_start: usize,
    pub col_end: usize,

    /// Full text of the anchor row at the moment of creation. Used as a
    /// fallback integrity check — if the stored line_id still resolves
    /// but the row content no longer matches this snapshot, the anchor
    /// is considered orphaned (reflow or massive overwrite).
    pub line_text: String,

    /// The exact text the user selected (may span multiple rows).
    pub selection_text: String,

    /// The user's comment body. Editable after creation via `update_text`.
    pub comment_text: String,

    /// Epoch milliseconds (from JS `Date.now()`) when the comment was
    /// created. Purely informational, used for UI ordering hints.
    pub created_at_ms: f64,
}

/// Collection of staged comments for one terminal session.
#[derive(Debug, Serialize, Deserialize)]
pub struct Comments {
    pub items: Vec<Comment>,
    pub next_id: u32,
}

/// `Default` delegates to `new()` so that `std::mem::take(&mut comments)`
/// (used during session save_active) leaves the outer wrapper with a
/// valid Comments — next_id starting at 1, not 0. The JS side treats
/// `add_comment_for_selection → 0` as "selection gone", so a zero id
/// from a default-initialized Comments would silently drop the comment.
impl Default for Comments {
    fn default() -> Self {
        Self::new()
    }
}

impl Comments {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            next_id: 1,
        }
    }

    /// Add a comment, returning the assigned id.
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        line_id: i64,
        col_start: usize,
        col_end: usize,
        line_text: String,
        selection_text: String,
        comment_text: String,
        created_at_ms: f64,
    ) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.items.push(Comment {
            id,
            line_id,
            col_start,
            col_end,
            line_text,
            selection_text,
            comment_text,
            created_at_ms,
        });
        id
    }

    /// Remove by id. Returns true if the id was found.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.items.len();
        self.items.retain(|c| c.id != id);
        self.items.len() != before
    }

    /// Drop every staged comment.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Replace the body of an existing comment. Returns true if found.
    pub fn update_text(&mut self, id: u32, new_text: String) -> bool {
        if let Some(c) = self.items.iter_mut().find(|c| c.id == id) {
            c.comment_text = new_text;
            true
        } else {
            false
        }
    }

    /// Relocate comments after an in-place TUI redraw. Stable numeric line ids
    /// handle ordinary scrollback growth and eviction, but Codex can replace
    /// rows above an anchor without changing either counter.
    pub fn reanchor_by_text(
        &mut self,
        row_count: usize,
        net_shift: i64,
        mut row_text: impl FnMut(usize) -> String,
    ) {
        for comment in &mut self.items {
            if comment.line_text.is_empty() {
                continue;
            }
            if let Some(found) = resolve_row_anchor(
                row_count,
                net_shift,
                comment.line_id,
                &comment.line_text,
                &mut row_text,
            ) {
                comment.line_id = found as i64 - net_shift;
            }
        }
    }
}

/// Resolve a stable line id and captured row text to its current content row.
/// The line id is the cheap path; nearby text matching repairs in-place redraws.
pub(crate) fn resolve_row_anchor(
    row_count: usize,
    net_shift: i64,
    line_id: i64,
    needle: &str,
    mut row_text: impl FnMut(usize) -> String,
) -> Option<usize> {
    let expected = line_id + net_shift;
    if expected >= 0 && (expected as usize) < row_count && row_text(expected as usize) == needle {
        return Some(expected as usize);
    }
    // A blank row has no useful identity. Searching would attach it to an
    // arbitrary nearby spacer, so leave its caller's numeric anchor unchanged.
    if needle.trim().is_empty() {
        return None;
    }
    closest_matching_row(row_count, expected.max(0) as usize, needle, &mut row_text)
}

pub(crate) fn closest_matching_row(
    row_count: usize,
    expected: usize,
    needle: &str,
    mut row_text: impl FnMut(usize) -> String,
) -> Option<usize> {
    const SEARCH_RADIUS: usize = 2_048;
    if row_count == 0 {
        return None;
    }
    let center = expected.min(row_count - 1);
    for distance in 0..=SEARCH_RADIUS.min(row_count - 1) {
        let before = center.saturating_sub(distance);
        if row_text(before) == needle {
            return Some(before);
        }
        let after = center.saturating_add(distance);
        if distance > 0 && after < row_count && row_text(after) == needle {
            return Some(after);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_redraw_reanchors_comment_to_same_text() {
        let mut comments = Comments::new();
        comments.add(1, 0, 4, "keep me".into(), "keep".into(), "note".into(), 0.0);
        let rows = ["new top", "different", "keep me", "footer"];
        comments.reanchor_by_text(rows.len(), 0, |idx| rows[idx].to_string());
        assert_eq!(comments.items[0].line_id, 2);
    }

    #[test]
    fn closest_duplicate_prefers_original_neighborhood() {
        let rows = ["same", "x", "x", "same"];
        assert_eq!(
            closest_matching_row(rows.len(), 2, "same", |i| rows[i].into()),
            Some(3)
        );
    }

    #[test]
    fn selection_endpoints_follow_inserted_codex_rows() {
        let rows = ["new status", "new progress", "selected start", "selected end"];
        assert_eq!(
            resolve_row_anchor(rows.len(), 0, 0, "selected start", |i| rows[i].into()),
            Some(2)
        );
        assert_eq!(
            resolve_row_anchor(rows.len(), 0, 1, "selected end", |i| rows[i].into()),
            Some(3)
        );
    }

    #[test]
    fn blank_rows_do_not_jump_to_an_arbitrary_spacer() {
        let rows = ["", "content", ""];
        assert_eq!(resolve_row_anchor(rows.len(), 0, 1, "", |i| rows[i].into()), None);
    }
}
