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
}
