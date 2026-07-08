//! Cursor navigation planner — Dijkstra over (row, col) state space.
//!
//! Pure logic, no WASM dependencies. Testable with `cargo test`.

use std::collections::BinaryHeap;
use std::cmp::Reverse;

/// Strategy for crossing between rows in the planner.
///
/// `Vertical` (legacy): Up/Down arrow keys move between rows at cost 1.
/// This works in terminals where each row is its own logical line (e.g.
/// a shell history list or a TUI list view).
///
/// `WrapEdge`: Up/Down are disabled; the planner crosses rows via
/// Right-at-end-of-row → start-of-next-row and Left-at-start-of-row →
/// end-of-prev-row. This models how Ink's TextInput handles wrapped
/// logical lines in Claude Code's input box: the buffer is one logical
/// line that spans multiple visual rows, so Down arrow is discarded
/// (no next logical line) but Right correctly crosses the wrap.
/// Works equally well for `\n`-separated input lines because Ink also
/// honors Right across `\n` boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowCrossingMode {
    Vertical,
    WrapEdge,
}

/// Dijkstra planner for cursor teleportation.
///
/// Finds the shortest key-event sequence to move from `(start_ri, start_col)`
/// to `(goal_ri, goal_col)` within a multi-row input area.
///
/// Per-row data:
/// - `row_start`: first valid column (after prompt indent on all rows)
/// - `content_end`: column after last non-space char (Ctrl+E destination)
/// - `row_width`: wrapped rows = full width; last row = content_end
/// - `row_text`: chars from `row_start..content_end` (for word-boundary scans)
/// - `image_spans`: per-row `[(start_col, end_col_exclusive)]` of Claude Code
///   `[Image #N]` placeholders. Ink/Claude renders these as 10-cell glyph blocks
///   but stores them as a single buffer character — one Right/Left arrow
///   traverses the entire block. Positions strictly inside a span are not
///   addressable by the cursor; the planner forbids them and emits atomic
///   Right/Left moves between span edges. Pass an empty slice for non-image
///   inputs (e.g. plain text deletion) to preserve cell-per-keystroke behavior.
/// - `mode`: see [`RowCrossingMode`]. Pass `WrapEdge` for Claude Code's
///   input box (the typical case); `Vertical` only for grid-style targets.
///
/// Returns raw escape bytes to send to the terminal.
#[allow(clippy::too_many_arguments)]
pub fn plan_dijkstra(
    start_ri: usize, start_col: usize,
    goal_ri: usize, goal_col: usize,
    n_rows: usize,
    row_start: &[usize], content_end: &[usize], row_width: &[usize], row_text: &[Vec<char>],
    image_spans: &[Vec<(usize, usize)>],
    mode: RowCrossingMode,
) -> Vec<u8> {
    if start_ri == goal_ri && start_col == goal_col {
        return Vec::new();
    }

    // SAFETY: Ensure content_end never exceeds row_width. If it does, the End
    // move would target a node outside the state space → OOB panic → in WASM
    // this permanently poisons the RefCell (no stack unwinding with panic=abort).
    let content_end_clamped: Vec<usize> = (0..n_rows)
        .map(|ri| content_end[ri].min(row_width[ri]))
        .collect();
    let content_end = &content_end_clamped[..];

    // Image-span helpers. A position is "interior" if strictly between the
    // span's edges — Ink doesn't render the cursor there because the entire
    // span is one buffer char. The planner refuses to reach interior positions.
    let spans_for = |ri: usize| -> &[(usize, usize)] {
        image_spans.get(ri).map(|v| v.as_slice()).unwrap_or(&[])
    };
    let is_interior = |ri: usize, col: usize| -> bool {
        spans_for(ri).iter().any(|&(s, e)| col > s && col < e)
    };
    // Snap an interior col outward in the requested direction. `dir > 0`
    // snaps to the span's end (right edge); `dir < 0` snaps to the start;
    // `dir == 0` snaps to whichever edge is closer.
    let snap_out = |ri: usize, col: usize, dir: i32| -> usize {
        for &(s, e) in spans_for(ri) {
            if col > s && col < e {
                return match dir.signum() {
                    1 => e,
                    -1 => s,
                    _ => if col - s <= e - col { s } else { e },
                };
            }
        }
        col
    };
    // If `col` is at the start of a span, return its end (the atomic Right
    // destination). Mirrors `jump_left` for Left moves at a span's end.
    let jump_right = |ri: usize, col: usize| -> Option<usize> {
        spans_for(ri).iter().find(|&&(s, _)| s == col).map(|&(_, e)| e)
    };
    let jump_left = |ri: usize, col: usize| -> Option<usize> {
        spans_for(ri).iter().find(|&&(_, e)| e == col).map(|&(s, _)| s)
    };

    // Clamp col when crossing rows — Ink preserves visual column but clamps
    // to the target row's content boundary (cursor can't go past content).
    // Also snap out of any image-span interior on the target row.
    let clamp_col = |ri: usize, col: usize| -> usize {
        let c = col.clamp(row_start[ri], row_width[ri]);
        snap_out(ri, c, 0)
    };

    // Word boundaries (readline-style, alphanumeric + '_' as word chars).
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';

    // Ink/Claude Code M-f lands on the FIRST CHAR of the next word.
    // If that lands inside an image span (image bytes look word-ish), advance
    // past the span — the cursor can't rest interior.
    let word_right = |ri: usize, col: usize| -> Option<usize> {
        let text = &row_text[ri];
        let rs = row_start[ri];
        let re = content_end[ri];
        if col >= re { return None; }
        let mut i = col - rs;
        while i < text.len() && is_word(text[i]) { i += 1; }
        while i < text.len() && !is_word(text[i]) { i += 1; }
        if i >= text.len() { return None; }
        let dst = snap_out(ri, rs + i, 1);
        if dst > col { Some(dst) } else { None }
    };

    let word_left = |ri: usize, col: usize| -> Option<usize> {
        let text = &row_text[ri];
        let rs = row_start[ri];
        if col <= rs || text.is_empty() { return None; }
        // Clamp to text length — cursor can be in trailing whitespace
        // past content_end, but text only covers [row_start..content_end].
        let mut i = (col - rs).min(text.len());
        i = i.saturating_sub(1);
        while i > 0 && !is_word(text[i]) { i -= 1; }
        while i > 0 && is_word(text[i - 1]) { i -= 1; }
        let dst = snap_out(ri, rs + i, -1);
        if dst < col { Some(dst) } else { None }
    };

    let max_col = *row_width.iter().max().unwrap_or(&0) + 1;
    let node_idx = |ri: usize, col: usize| -> usize { ri * max_col + col };
    let total_nodes = n_rows * max_col;

    // 0=Up 1=Down 2=Left 3=Right 4=Home 5=End 6=WordRight 7=WordLeft
    let mut best_cost: Vec<u32> = vec![u32::MAX; total_nodes];
    let mut parent: Vec<(u32, u8)> = vec![(u32::MAX, 0); total_nodes];
    let mut pq: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();

    // Defensive: caller may hand us a start/goal that happens to fall inside
    // an image span (stale visual cursor read, click landed inside the glyph).
    // Snap both to the nearest valid edge so the path search has a chance.
    let start_col = snap_out(start_ri, start_col, 0);
    let goal_col = snap_out(goal_ri, goal_col, 0);

    if start_ri == goal_ri && start_col == goal_col {
        return Vec::new();
    }

    let start_node = node_idx(start_ri, start_col);
    let goal_node = node_idx(goal_ri, goal_col);
    best_cost[start_node] = 0;
    pq.push(Reverse((0u32, start_node)));

    while let Some(Reverse((cost, node))) = pq.pop() {
        if node == goal_node { break; }
        if cost > best_cost[node] { continue; }
        let ri = node / max_col;
        let col = node % max_col;

        let mut moves: Vec<(usize, usize, u32, u8)> = Vec::with_capacity(10);
        // Cost = 1 per input event (Ink processes one escape sequence per
        // keystroke regardless of byte count, so minimize event count).
        // Vertical moves (Up/Down) are only added when the caller asked
        // for `Vertical` row-crossing. For Ink-style wrapped input
        // (`WrapEdge` mode) the planner crosses rows via Right-at-end /
        // Left-at-start instead, because Ink discards Up/Down inside a
        // single logical line.
        if mode == RowCrossingMode::Vertical && ri > 0 {
            let nri = ri - 1;
            let nc = clamp_col(nri, col);
            if !is_interior(nri, nc) {
                moves.push((nri, nc, 1, 0));
            }
        }
        if mode == RowCrossingMode::Vertical && ri + 1 < n_rows {
            let nri = ri + 1;
            let nc = clamp_col(nri, col);
            if !is_interior(nri, nc) {
                moves.push((nri, nc, 1, 1));
            }
        }
        // Left: either one cell or atomic image-jump if col sits at a span's end.
        if let Some(dst) = jump_left(ri, col) {
            moves.push((ri, dst, 1, 2));
        } else if col > row_start[ri] && !is_interior(ri, col - 1) {
            moves.push((ri, col - 1, 1, 2));
        } else if mode == RowCrossingMode::WrapEdge && col <= row_start[ri] && ri > 0 {
            // Wrap-edge Left: cursor at start of row → one Left key crosses
            // the wrap (or `\n`) into the end of the previous row in the
            // logical buffer. Ink honors this for both soft-wrap and
            // hard-newline boundaries.
            let dst = row_width[ri - 1];
            if !is_interior(ri - 1, dst) {
                moves.push((ri - 1, dst, 1, 2));
            }
        }
        // Right: either one cell or atomic image-jump if col sits at a span's start.
        if let Some(dst) = jump_right(ri, col) {
            moves.push((ri, dst, 1, 3));
        } else if col < row_width[ri] && !is_interior(ri, col + 1) {
            moves.push((ri, col + 1, 1, 3));
        } else if mode == RowCrossingMode::WrapEdge && col >= row_width[ri] && ri + 1 < n_rows {
            // Wrap-edge Right: mirror of wrap-edge Left.
            let dst = row_start[ri + 1];
            if !is_interior(ri + 1, dst) {
                moves.push((ri + 1, dst, 1, 3));
            }
        }
        // Ctrl+A = start of line. Ctrl+E = end of content.
        if col != row_start[ri] && !is_interior(ri, row_start[ri]) {
            moves.push((ri, row_start[ri], 1, 4));
        }
        if col != content_end[ri] && !is_interior(ri, content_end[ri]) {
            moves.push((ri, content_end[ri], 1, 5));
        }
        if let Some(dst) = word_right(ri, col)
            && !is_interior(ri, dst)
        {
            moves.push((ri, dst, 1, 6));
        }
        if let Some(dst) = word_left(ri, col)
            && !is_interior(ri, dst)
        {
            moves.push((ri, dst, 1, 7));
        }

        for (nri, ncol, mcost, mid) in moves {
            let ncost = cost + mcost;
            let nnode = node_idx(nri, ncol);
            if ncost < best_cost[nnode] {
                best_cost[nnode] = ncost;
                parent[nnode] = (node as u32, mid);
                pq.push(Reverse((ncost, nnode)));
            }
        }
    }

    if best_cost[goal_node] == u32::MAX {
        return Vec::new();
    }

    let mut moves: Vec<u8> = Vec::new();
    let mut node = goal_node;
    while node != start_node {
        let (prev, mid) = parent[node];
        moves.push(mid);
        node = prev as usize;
    }
    moves.reverse();

    let mut seq: Vec<u8> = Vec::with_capacity(moves.len() * 3);
    for mid in moves {
        match mid {
            0 => seq.extend_from_slice(b"\x1b[A"),   // Up
            1 => seq.extend_from_slice(b"\x1b[B"),   // Down
            2 => seq.extend_from_slice(b"\x1b[D"),   // Left
            3 => seq.extend_from_slice(b"\x1b[C"),   // Right
            4 => seq.push(0x01),                      // Ctrl+A = Home
            5 => seq.push(0x05),                      // Ctrl+E = End
            6 => seq.extend_from_slice(b"\x1bf"),     // ESC-f = WordRight
            7 => seq.extend_from_slice(b"\x1bb"),     // ESC-b = WordLeft
            _ => {}
        }
    }
    seq
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Build a random grid configuration for property-based testing.
    /// Generates 1-8 rows, each with random content and trailing whitespace.
    fn grid_strategy() -> impl Strategy<Value = (
        Vec<usize>,    // row_start
        Vec<usize>,    // content_end
        Vec<usize>,    // row_width
        Vec<Vec<char>>, // row_text
    )> {
        // 1-8 rows, terminal width 20-200
        (1..=8usize, 20..=200usize).prop_flat_map(|(n_rows, term_width)| {
            proptest::collection::vec(
                // Per row: prompt offset 0-10, content length 0-term_width
                (0..=10usize, 0..=term_width),
                n_rows,
            ).prop_map(move |row_specs| {
                let mut row_start = Vec::new();
                let mut content_end = Vec::new();
                let mut row_width = Vec::new();
                let mut row_text = Vec::new();

                for (i, (prompt_off, content_len)) in row_specs.iter().enumerate() {
                    let rs = if i == 0 { *prompt_off } else { 0 };
                    let ce = (rs + content_len).min(term_width);
                    row_start.push(rs);
                    content_end.push(ce);
                    row_width.push(term_width);
                    // Generate text with word-like content
                    let len = ce.saturating_sub(rs);
                    let chars: Vec<char> = (0..len).map(|j| {
                        if j % 7 == 3 || j % 7 == 4 { ' ' } else { 'a' }
                    }).collect();
                    row_text.push(chars);
                }
                (row_start, content_end, row_width, row_text)
            })
        })
    }

    proptest! {
        /// The planner must NEVER panic, regardless of cursor position.
        /// This is critical because in WASM, a panic leaks the RefCell borrow
        /// and permanently poisons the terminal object.
        #[test]
        fn plan_dijkstra_never_panics(
            grid in grid_strategy(),
            start_ri_raw in 0..8usize,
            start_col_raw in 0..200usize,
            goal_ri_raw in 0..8usize,
            goal_col_raw in 0..200usize,
        ) {
            let (row_start, content_end, row_width, row_text) = grid;
            let n_rows = row_start.len();
            if n_rows == 0 { return Ok(()); }

            // Clamp indices to valid range
            let start_ri = start_ri_raw % n_rows;
            let goal_ri = goal_ri_raw % n_rows;
            let start_col = start_col_raw.clamp(row_start[start_ri], row_width[start_ri]);
            let goal_col = goal_col_raw.clamp(row_start[goal_ri], row_width[goal_ri]);

            // This must not panic — that's the entire assertion.
            let empty: Vec<Vec<(usize, usize)>> = Vec::new();
            let _result = plan_dijkstra(
                start_ri, start_col,
                goal_ri, goal_col,
                n_rows,
                &row_start, &content_end, &row_width, &row_text,
                &empty,
                RowCrossingMode::Vertical,
            );
            let _result = plan_dijkstra(
                start_ri, start_col,
                goal_ri, goal_col,
                n_rows,
                &row_start, &content_end, &row_width, &row_text,
                &empty,
                RowCrossingMode::WrapEdge,
            );
        }

        /// When start == goal, result must be empty.
        #[test]
        fn same_position_returns_empty(
            grid in grid_strategy(),
            ri_raw in 0..8usize,
            col_raw in 0..200usize,
        ) {
            let (row_start, content_end, row_width, row_text) = grid;
            let n_rows = row_start.len();
            if n_rows == 0 { return Ok(()); }

            let ri = ri_raw % n_rows;
            let col = col_raw.clamp(row_start[ri], row_width[ri]);

            let empty: Vec<Vec<(usize, usize)>> = Vec::new();
            let result = plan_dijkstra(
                ri, col, ri, col,
                n_rows,
                &row_start, &content_end, &row_width, &row_text,
                &empty,
                RowCrossingMode::Vertical,
            );
            prop_assert!(result.is_empty());
            let result = plan_dijkstra(
                ri, col, ri, col,
                n_rows,
                &row_start, &content_end, &row_width, &row_text,
                &empty,
                RowCrossingMode::WrapEdge,
            );
            prop_assert!(result.is_empty());
        }

        /// Cursor in trailing whitespace (col > content_end) must not panic.
        /// This is the specific scenario that caused the RefCell poison bug.
        #[test]
        fn trailing_whitespace_positions_safe(
            grid in grid_strategy(),
            ri_raw in 0..8usize,
        ) {
            let (row_start, content_end, row_width, row_text) = grid;
            let n_rows = row_start.len();
            if n_rows == 0 { return Ok(()); }
            let ri = ri_raw % n_rows;

            // Place cursor 1 past content_end (trailing whitespace)
            let col = (content_end[ri] + 1).min(row_width[ri]);
            if col <= row_start[ri] { return Ok(()); }

            // Try navigating FROM trailing whitespace to row start
            let empty: Vec<Vec<(usize, usize)>> = Vec::new();
            let _result = plan_dijkstra(
                ri, col, ri, row_start[ri],
                n_rows,
                &row_start, &content_end, &row_width, &row_text,
                &empty,
                RowCrossingMode::Vertical,
            );

            // And TO trailing whitespace from row start
            let _result = plan_dijkstra(
                ri, row_start[ri], ri, col,
                n_rows,
                &row_start, &content_end, &row_width, &row_text,
                &empty,
                RowCrossingMode::Vertical,
            );
        }

        /// content_end > row_width must not panic (the clamp defense).
        /// This is the exact invariant violation that caused the RefCell
        /// poison: Ctrl+E targeted a node beyond the state space, causing
        /// an OOB index into best_cost[]. Without the clamp, this panics.
        #[test]
        fn content_end_exceeds_row_width_safe(
            n_rows in 1..=4usize,
            term_width in 20..=100usize,
            overflow in 1..=20usize,
            start_ri_raw in 0..4usize,
            goal_ri_raw in 0..4usize,
            start_col_raw in 0..100usize,
            goal_col_raw in 0..100usize,
        ) {
            let start_ri = start_ri_raw % n_rows;
            let goal_ri = goal_ri_raw % n_rows;

            let row_start: Vec<usize> = (0..n_rows).map(|i| if i == 0 { 2 } else { 0 }).collect();
            // Intentionally set content_end PAST row_width on every row
            let content_end: Vec<usize> = (0..n_rows).map(|_| term_width + overflow).collect();
            let row_width: Vec<usize> = vec![term_width; n_rows];
            let row_text: Vec<Vec<char>> = (0..n_rows).map(|i| {
                let len = term_width.saturating_sub(row_start[i]);
                (0..len).map(|j| if j % 5 == 2 { ' ' } else { 'x' }).collect()
            }).collect();

            let start_col = start_col_raw.clamp(row_start[start_ri], row_width[start_ri]);
            let goal_col = goal_col_raw.clamp(row_start[goal_ri], row_width[goal_ri]);

            // Without the clamp in plan_dijkstra, this panics with OOB.
            let empty: Vec<Vec<(usize, usize)>> = Vec::new();
            let _result = plan_dijkstra(
                start_ri, start_col,
                goal_ri, goal_col,
                n_rows,
                &row_start, &content_end, &row_width, &row_text,
                &empty,
                RowCrossingMode::Vertical,
            );
            let _result = plan_dijkstra(
                start_ri, start_col,
                goal_ri, goal_col,
                n_rows,
                &row_start, &content_end, &row_width, &row_text,
                &empty,
                RowCrossingMode::WrapEdge,
            );
        }
    }

    /// `[Image #1] hello` — one Right arrow must traverse the entire image
    /// glyph (10 cells) because Ink stores it as a single buffer char.
    /// Before the fix, the planner would emit 10 Rights and Claude would
    /// overshoot by 9 chars.
    #[test]
    fn image_span_is_one_keystroke_wide() {
        // Layout: "[Image #1] hello"
        // cols    0123456789012345
        //         ^         ^    ^
        //         span s=0  e=10
        let row_text: Vec<char> = "[Image #1] hello".chars().collect();
        let n_rows = 1;
        let row_start = vec![0usize];
        let content_end = vec![16usize];
        let row_width = vec![16usize];
        let row_text = vec![row_text];
        let image_spans = vec![vec![(0usize, 10usize)]];

        // Move from col 0 (start of image) to col 16 (end of "hello")
        let seq = plan_dijkstra(
            0, 0,
            0, 16,
            n_rows,
            &row_start, &content_end, &row_width, &row_text,
            &image_spans,
            RowCrossingMode::Vertical,
        );

        // 1 Right to traverse image + Ctrl+E (or 6 Rights) — planner picks
        // the cheapest. The crucial property: NEVER more than 7 events total
        // (image-jump=1 + chars-after-image=6). Pre-fix: 16 Rights.
        let event_count = count_events(&seq);
        assert!(
            event_count <= 7,
            "expected ≤7 events with image-aware planner, got {}: {:?}",
            event_count, seq
        );
    }

    /// Moving Left from after an image must also traverse it in one keystroke.
    #[test]
    fn image_span_left_jump() {
        // "[Image #1]" alone — cursor at col 10 (after `]`), goal col 0
        let row_text: Vec<char> = "[Image #1]".chars().collect();
        let image_spans = vec![vec![(0usize, 10usize)]];
        let seq = plan_dijkstra(
            0, 10,
            0, 0,
            1,
            &[0], &[10], &[10], &[row_text],
            &image_spans,
            RowCrossingMode::Vertical,
        );
        // Either Ctrl+A (1 event) or one Left (1 event). Either way: 1 event.
        assert_eq!(count_events(&seq), 1, "got: {:?}", seq);
    }

    /// The planner must never park the cursor at an interior column.
    /// (Path reconstruction visits only nodes whose `best_cost` was set,
    /// and only non-interior positions are reachable.)
    #[test]
    fn image_interior_unreachable() {
        let row_text: Vec<char> = "[Image #1] x".chars().collect();
        let image_spans = vec![vec![(0usize, 10usize)]];
        // Goal col 5 is interior — planner should snap to an edge.
        let seq = plan_dijkstra(
            0, 12,
            0, 5,
            1,
            &[0], &[12], &[12], &[row_text],
            &image_spans,
            RowCrossingMode::Vertical,
        );
        // Goal snapped to col 0 (nearer to 5 than 10? 5-0=5, 10-5=5 → tie,
        // either is fine). The path must be non-empty (we moved from 12).
        assert!(!seq.is_empty());
    }

    /// WrapEdge mode must never emit Up (`\x1b[A`) or Down (`\x1b[B`).
    /// Those keystrokes are discarded by Ink's TextInput when the cursor
    /// is inside a wrapped logical line — relying on them would make the
    /// planner emit "phantom" keystrokes that don't actually move the
    /// cursor, breaking click-to-cursor and the Up/Down-in-input fix.
    #[test]
    fn wrap_edge_mode_emits_no_vertical_arrows() {
        // 3 wrapped rows of an Ink input box, term_width=20, prompt at col 2.
        let row_start = vec![2usize, 0, 0];
        let content_end = vec![20usize, 20, 12];
        let row_width = vec![20usize, 20, 12];
        let row_text = vec![
            "this is the first ro".chars().collect::<Vec<_>>(),
            "w that wraps to row ".chars().collect::<Vec<_>>(),
            "two and three".chars().collect::<Vec<_>>(),
        ];
        let empty: Vec<Vec<(usize, usize)>> = Vec::new();

        // Walk row 0 col 5 → row 2 col 8 (down two rows, different col).
        let seq = plan_dijkstra(
            0, 5, 2, 8, 3,
            &row_start, &content_end, &row_width, &row_text,
            &empty,
            RowCrossingMode::WrapEdge,
        );
        assert!(!seq.is_empty(), "planner returned empty path");

        // Forbid Up/Down byte sequences anywhere in the output.
        let mut i = 0;
        while i + 2 < seq.len() {
            if seq[i] == 0x1b && seq[i + 1] == b'[' {
                let dir = seq[i + 2];
                assert!(
                    dir != b'A' && dir != b'B',
                    "WrapEdge planner emitted vertical arrow at byte {}: {:?}",
                    i, &seq[..]
                );
                i += 3;
            } else if seq[i] == 0x1b {
                i += 2;
            } else {
                i += 1;
            }
        }
    }

    /// WrapEdge planner can still cross rows — it just routes via Right at
    /// end-of-row (or Left at start) instead of Up/Down. Path must exist
    /// for any reachable goal in a contiguous wrapped buffer.
    #[test]
    fn wrap_edge_mode_crosses_rows_via_horizontal() {
        let row_start = vec![2usize, 0];
        let content_end = vec![20usize, 10];
        let row_width = vec![20usize, 10];
        let row_text = vec![
            "wwwwwwwwwwwwwwwwwwww".chars().collect::<Vec<_>>(),
            "second row".chars().collect::<Vec<_>>(),
        ];
        let empty: Vec<Vec<(usize, usize)>> = Vec::new();

        // From row 0 end → row 1 start: must be exactly 1 Right keystroke.
        let seq = plan_dijkstra(
            0, 20, 1, 0, 2,
            &row_start, &content_end, &row_width, &row_text,
            &empty,
            RowCrossingMode::WrapEdge,
        );
        assert_eq!(seq, b"\x1b[C", "expected single Right keystroke, got {:?}", seq);

        // Symmetric: from row 1 start → row 0 end: exactly 1 Left.
        let seq = plan_dijkstra(
            1, 0, 0, 20, 2,
            &row_start, &content_end, &row_width, &row_text,
            &empty,
            RowCrossingMode::WrapEdge,
        );
        assert_eq!(seq, b"\x1b[D", "expected single Left keystroke, got {:?}", seq);
    }

    /// Vertical mode still emits Down (`\x1b[B`) for same-col targets — the
    /// efficient path. Regression guard so the gate change doesn't accidentally
    /// affect non-input callers that legitimately need Up/Down.
    #[test]
    fn vertical_mode_still_emits_down() {
        let row_start = vec![0usize, 0];
        let content_end = vec![10usize, 10];
        let row_width = vec![10usize, 10];
        let row_text = vec![
            "abcdefghij".chars().collect::<Vec<_>>(),
            "klmnopqrst".chars().collect::<Vec<_>>(),
        ];
        let empty: Vec<Vec<(usize, usize)>> = Vec::new();

        let seq = plan_dijkstra(
            0, 5, 1, 5, 2,
            &row_start, &content_end, &row_width, &row_text,
            &empty,
            RowCrossingMode::Vertical,
        );
        // Cheapest path from (0,5) → (1,5) is one Down. WrapEdge would
        // take many more keystrokes (walk to end of row 0, wrap, walk back).
        assert_eq!(seq, b"\x1b[B", "expected single Down keystroke, got {:?}", seq);
    }

    /// Count terminal input events in a byte sequence. Each escape-prefixed
    /// arrow/word move counts as 1, as does each Ctrl-key byte.
    fn count_events(seq: &[u8]) -> usize {
        let mut i = 0;
        let mut n = 0;
        while i < seq.len() {
            if seq[i] == 0x1b {
                // ESC [ <C> = 3 bytes (arrow); ESC <c> = 2 bytes (word jump)
                if i + 1 < seq.len() && seq[i + 1] == b'[' {
                    i += 3;
                } else {
                    i += 2;
                }
            } else {
                i += 1;
            }
            n += 1;
        }
        n
    }
}
