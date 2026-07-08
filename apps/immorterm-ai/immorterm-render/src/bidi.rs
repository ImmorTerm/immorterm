//! BiDi (Bidirectional) text support and alignment for the terminal renderer.
//!
//! Implements render-time UAX #9 reordering and text alignment (left/right/center/auto).
//! The terminal grid always stores text in logical order — this module computes
//! the visual order mapping applied only during rendering.
//!
//! # Design Principles
//! - **Render-only**: No grid mutations. All reordering is a pure index mapping.
//! - **Cached**: Recomputed only when a row is dirty. Reset on resize.
//! - **Zero-cost for LTR**: Pure-ASCII rows skip BiDi entirely.

use unicode_bidi::{BidiInfo, Level};
use unicode_script::{Script, UnicodeScript};

use immorterm_core::grid::Row;
use immorterm_core::CombiningMarks;

/// Text alignment mode — like a word processor toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum TextAlignment {
    /// Text starts from the left edge (default for LTR).
    /// Text starts from the left edge (default — correct for terminal semantics where
    /// content always starts at column 0).
    #[default]
    Left,
    /// Text starts from the right edge.
    Right,
    /// Text centered in the viewport.
    Center,
    /// Automatically determined: Left for LTR paragraphs, Right for RTL.
    /// Only activated explicitly via Phase 6 controls (MCP tool, OSC, toolbar).
    Auto,
}

/// Paragraph base direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ParagraphDirection {
    /// Left-to-right (Latin, CJK, etc.)
    Ltr,
    /// Right-to-left (Hebrew, Arabic, etc.)
    Rtl,
    /// Auto-detect from first strong character (with 30% majority fallback).
    #[default]
    Auto,
}

/// Cached BiDi reordering result for a single row.
///
/// Computed once per dirty row, reused across frames until the row changes.
#[derive(Debug, Clone)]
pub struct BidiRowCache {
    /// visual_col → logical_col mapping. `visual_to_logical[visual_pos] = logical_col`.
    pub visual_to_logical: Vec<usize>,
    /// logical_col → visual_col mapping. `logical_to_visual[logical_col] = visual_pos`.
    pub logical_to_visual: Vec<usize>,
    /// Resolved paragraph direction for this row.
    pub resolved_direction: ParagraphDirection,
    /// Pixel offset for alignment (0.0 for left-aligned rows).
    pub alignment_offset_px: f32,
    /// Whether this row contains any RTL content (optimization: skip BiDi for pure-LTR).
    pub has_rtl: bool,
    /// Contiguous RTL runs in visual order: `(visual_start_col, visual_end_col_exclusive)`.
    /// The renderer uses these to render whole runs as single wide glyphs with natural
    /// proportional spacing instead of per-character monospace placement.
    pub rtl_runs: Vec<(usize, usize)>,
}

impl Default for BidiRowCache {
    fn default() -> Self {
        Self {
            visual_to_logical: Vec::new(),
            logical_to_visual: Vec::new(),
            resolved_direction: ParagraphDirection::Ltr,
            alignment_offset_px: 0.0,
            has_rtl: false,
            rtl_runs: Vec::new(),
        }
    }
}

/// Detect whether a character belongs to an RTL script.
fn is_rtl_char(c: char) -> bool {
    matches!(
        c.script(),
        Script::Hebrew
            | Script::Arabic
            | Script::Syriac
            | Script::Thaana
            | Script::Mandaic
            | Script::Nko
            | Script::Samaritan
            | Script::Avestan
    )
}

/// Detect whether a character is a "strong" directional character
/// (not a space, digit, emoji, or punctuation).
fn is_strong_directional(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_ascii_digit()
        && !c.is_ascii_punctuation()
        && c.script() != Script::Common
        && c.script() != Script::Inherited
}

/// Auto-detect paragraph direction from row content.
///
/// Strategy (borrowed from rtl-for-vs-code-agents):
/// 1. Find the first "strong" character (skip spaces, digits, emojis, punctuation).
/// 2. If it's RTL → RTL paragraph.
/// 3. Fallback: if ≥30% of strong characters are RTL → RTL paragraph.
/// 4. Otherwise → LTR.
pub fn detect_paragraph_direction(row: &Row) -> ParagraphDirection {
    let mut first_strong: Option<bool> = None; // true = RTL
    let mut rtl_count = 0usize;
    let mut strong_count = 0usize;

    for cell in &row.cells {
        let c = cell.grapheme;
        if c == ' ' || cell.width == 0 {
            continue;
        }
        if is_strong_directional(c) {
            let is_rtl = is_rtl_char(c);
            if first_strong.is_none() {
                first_strong = Some(is_rtl);
            }
            strong_count += 1;
            if is_rtl {
                rtl_count += 1;
            }
        }
    }

    // First strong character is authoritative
    match first_strong {
        Some(true) => return ParagraphDirection::Rtl,
        Some(false) => return ParagraphDirection::Ltr,
        None => {}
    }

    // No strong first char (starts with digits/punctuation/etc.)
    // Fall back to 30% majority heuristic
    if strong_count > 0 && rtl_count * 100 / strong_count >= 30 {
        return ParagraphDirection::Rtl;
    }

    ParagraphDirection::Ltr
}

/// Compute the number of content columns in a row (trailing spaces stripped).
fn content_width(row: &Row) -> usize {
    let mut last_non_space = 0;
    for (i, cell) in row.cells.iter().enumerate() {
        if cell.grapheme != ' ' || cell.width == 0 {
            last_non_space = i + 1;
        }
    }
    last_non_space
}

/// Quick check: does this row contain any RTL characters at all?
/// Used to skip the full BiDi algorithm for pure-LTR rows.
fn row_has_rtl(row: &Row) -> bool {
    row.cells.iter().any(|c| c.grapheme != ' ' && is_rtl_char(c.grapheme))
}

/// Compute BiDi reordering for a single row.
///
/// Returns a `BidiRowCache` with visual↔logical column mappings and alignment offset.
pub fn reorder_row(
    row: &Row,
    _combining_marks: &CombiningMarks,
    _row_index: usize,
    direction: ParagraphDirection,
    alignment: TextAlignment,
    visible_cols: usize,
    cell_width_px: f32,
) -> BidiRowCache {
    let cols = row.cells.len().min(visible_cols);

    // Resolve paragraph direction
    let resolved_dir = match direction {
        ParagraphDirection::Ltr => ParagraphDirection::Ltr,
        ParagraphDirection::Rtl => ParagraphDirection::Rtl,
        ParagraphDirection::Auto => detect_paragraph_direction(row),
    };

    // Fast path: no RTL content → identity mapping
    let has_rtl = row_has_rtl(row);
    if !has_rtl {
        let identity: Vec<usize> = (0..cols).collect();
        let alignment_offset = compute_alignment_offset(
            alignment,
            resolved_dir,
            content_width(row),
            visible_cols,
            cell_width_px,
        );
        return BidiRowCache {
            logical_to_visual: identity.clone(),
            visual_to_logical: identity,
            resolved_direction: resolved_dir,
            alignment_offset_px: alignment_offset,
            has_rtl: false,
            rtl_runs: Vec::new(),
        };
    }

    // Build logical text string from row cells (one char per cell)
    let text: String = row.cells.iter().take(cols).map(|c| c.grapheme).collect();

    // Determine paragraph level: 0 = LTR, 1 = RTL
    let para_level = match resolved_dir {
        ParagraphDirection::Rtl => Level::rtl(),
        _ => Level::ltr(),
    };

    // Run the Unicode BiDi Algorithm (UAX #9)
    let bidi_info = BidiInfo::new(&text, Some(para_level));

    // We have one paragraph (single row), get the first paragraph's range
    if bidi_info.paragraphs.is_empty() {
        // Empty row — return identity
        let identity: Vec<usize> = (0..cols).collect();
        return BidiRowCache {
            logical_to_visual: identity.clone(),
            visual_to_logical: identity,
            resolved_direction: resolved_dir,
            alignment_offset_px: 0.0,
            has_rtl: false,
            rtl_runs: Vec::new(),
        };
    }

    let paragraph = &bidi_info.paragraphs[0];
    let line_range = paragraph.range.clone();

    // Get the visual reordering of the line
    let _reordered = bidi_info.reorder_line(paragraph, line_range);

    // Build the visual → logical and logical → visual mappings.
    // `reordered` is a slice of (char_index, &str) in visual order.
    // We need to map this back to cell/column indices.
    //
    // Since we built the text as one char per cell, char index == column index.
    let mut visual_to_logical = Vec::with_capacity(cols);
    let mut logical_to_visual = vec![0usize; cols];

    // Map each character (cell column) to its BiDi level.
    // IMPORTANT: bidi_info.levels is byte-indexed (one entry per byte), not per char.
    // Multi-byte characters (Hebrew=2, emoji=4, em dash=3) have their level repeated
    // for each byte. We must iterate by char_indices() to get the correct level per cell.
    let para_start = paragraph.range.start;
    let indexed_levels: Vec<(usize, Level)> = text
        .char_indices()
        .enumerate()
        .take(cols)
        .map(|(char_idx, (byte_offset, _ch))| {
            let idx = para_start + byte_offset;
            // Default to LTR if the index is out of bounds (can happen with
            // paragraph separators in cell text that split the BiDi paragraph).
            let level = bidi_info.levels.get(idx).copied().unwrap_or(Level::ltr());
            (char_idx, level)
        })
        .collect();

    // Reorder using the BiDi algorithm's visual ordering rules:
    // Characters are reordered by their embedding level.
    // We use unicode_bidi's reorder_line indirectly via the reordered output.
    //
    // Simpler approach: rebuild the visual order from the reordered text.
    // Since each char maps 1:1 to a column, we can use char offsets.
    visual_to_logical.clear();

    // For each visual position, find which logical column it came from.
    // With 1-char-per-cell this is a permutation derived from the levels.
    // Use the standard level-based reordering to build the map.
    let visual_order = compute_visual_order(&indexed_levels);

    for &logical_col in &visual_order {
        visual_to_logical.push(logical_col);
    }

    // Pad if needed (row may be shorter than visual_order)
    while visual_to_logical.len() < cols {
        visual_to_logical.push(visual_to_logical.len());
    }

    // Build inverse mapping
    logical_to_visual.resize(cols, 0);
    for (visual_pos, &logical_col) in visual_to_logical.iter().enumerate() {
        if logical_col < cols {
            logical_to_visual[logical_col] = visual_pos;
        }
    }

    let alignment_offset = compute_alignment_offset(
        alignment,
        resolved_dir,
        content_width(row),
        visible_cols,
        cell_width_px,
    );

    // Detect contiguous RTL runs in visual order.
    // An RTL run is a maximal sequence of consecutive visual columns whose logical
    // characters have odd (RTL) BiDi levels. The renderer uses these runs to batch-
    // render Hebrew/Arabic text with natural proportional spacing.
    let mut rtl_runs = Vec::new();
    {
        let mut run_start: Option<usize> = None;
        for (visual_col, &logical_col) in visual_to_logical.iter().enumerate() {
            // Check if this logical column has an RTL level
            let is_rtl = indexed_levels.get(logical_col)
                .map(|(_, level)| level.is_rtl())
                .unwrap_or(false);
            match (is_rtl, run_start) {
                (true, None) => run_start = Some(visual_col),
                (false, Some(start)) => {
                    rtl_runs.push((start, visual_col));
                    run_start = None;
                }
                _ => {}
            }
        }
        if let Some(start) = run_start {
            rtl_runs.push((start, visual_to_logical.len()));
        }
    }

    BidiRowCache {
        visual_to_logical,
        logical_to_visual,
        resolved_direction: resolved_dir,
        alignment_offset_px: alignment_offset,
        has_rtl: true,
        rtl_runs,
    }
}

/// Compute visual column order from BiDi embedding levels.
///
/// This implements the L2 rule of UAX #9: reverse contiguous runs of
/// characters at each level greater than or equal to the maximum level,
/// working down to the paragraph level.
fn compute_visual_order(indexed_levels: &[(usize, Level)]) -> Vec<usize> {
    let len = indexed_levels.len();
    if len == 0 {
        return vec![];
    }

    // Start with logical order
    let mut result: Vec<usize> = indexed_levels.iter().map(|&(i, _)| i).collect();
    let levels: Vec<Level> = indexed_levels.iter().map(|&(_, l)| l).collect();

    // Find max level and lowest odd level (UAX #9 L2 rule)
    let max_level = levels.iter().copied().max().unwrap_or(Level::ltr());

    // Lowest odd level present in the text (at least 1 for any RTL content)
    let lowest_odd = levels
        .iter()
        .filter(|l| l.number() % 2 == 1)
        .copied()
        .min()
        .unwrap_or(Level::rtl()); // default to 1 if somehow no odd levels

    // L2: From highest level to lowest odd level, reverse contiguous runs
    // at that level or higher. This correctly reverses RTL runs.
    let mut level_num = max_level.number();
    while level_num >= lowest_odd.number() {
        let target = Level::new(level_num).unwrap_or(Level::ltr());
        let mut i = 0;
        while i < len {
            if levels[result[i]] >= target {
                let start = i;
                while i < len && levels[result[i]] >= target {
                    i += 1;
                }
                result[start..i].reverse();
            } else {
                i += 1;
            }
        }
        if level_num == 0 {
            break;
        }
        level_num -= 1;
    }

    result
}

/// Calculate alignment pixel offset.
fn compute_alignment_offset(
    alignment: TextAlignment,
    resolved_dir: ParagraphDirection,
    content_cols: usize,
    visible_cols: usize,
    cell_width_px: f32,
) -> f32 {
    if content_cols >= visible_cols {
        return 0.0; // Row is full — no room to align
    }

    let free_space = (visible_cols - content_cols) as f32 * cell_width_px;

    match alignment {
        TextAlignment::Left => 0.0,
        TextAlignment::Right => free_space,
        TextAlignment::Center => free_space / 2.0,
        TextAlignment::Auto => {
            if resolved_dir == ParagraphDirection::Rtl {
                free_space // Right-align for RTL
            } else {
                0.0 // Left-align for LTR
            }
        }
    }
}

/// BiDi mirroring (UAX #9 Rule L4): swap paired characters in RTL context.
///
/// When a character with the Bidi_Mirrored property is resolved as RTL,
/// its glyph should be replaced with its mirror. E.g. `(` → `)`, `[` → `]`.
pub fn bidi_mirror(ch: char) -> char {
    match ch {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '\u{00AB}' => '\u{00BB}', // « → »
        '\u{00BB}' => '\u{00AB}', // » → «
        '\u{2039}' => '\u{203A}', // ‹ → ›
        '\u{203A}' => '\u{2039}', // › → ‹
        '\u{2045}' => '\u{2046}', // ⁅ → ⁆
        '\u{2046}' => '\u{2045}', // ⁆ → ⁅
        '\u{207D}' => '\u{207E}', // ⁽ → ⁾
        '\u{207E}' => '\u{207D}', // ⁾ → ⁽
        '\u{208D}' => '\u{208E}', // ₍ → ₎
        '\u{208E}' => '\u{208D}', // ₎ → ₍
        _ => ch,
    }
}

/// Check if a logical column is in an RTL run (odd BiDi level).
/// Used by the renderer to decide whether to apply mirroring.
pub fn is_in_rtl_run(cache: &BidiRowCache, logical_col: usize) -> bool {
    if !cache.has_rtl {
        return false;
    }
    let visual_col = cache.logical_to_visual.get(logical_col).copied().unwrap_or(logical_col);
    cache.rtl_runs.iter().any(|(start, end)| visual_col >= *start && visual_col < *end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use immorterm_core::cell::Cell;
    use immorterm_core::grid::Row;

    fn make_row(text: &str) -> Row {
        let cells: Vec<Cell> = text.chars().map(Cell::with_char).collect();
        Row {
            cells,
            dirty: true,
            wrapped: false,
            soft_wrapped: false,
            content_end_col: 0,
            direction: None,
            alignment: None,
        }
    }

    #[test]
    fn detect_ltr_paragraph() {
        let row = make_row("hello world");
        assert_eq!(detect_paragraph_direction(&row), ParagraphDirection::Ltr);
    }

    #[test]
    fn detect_rtl_paragraph() {
        let row = make_row("שלום עולם");
        assert_eq!(detect_paragraph_direction(&row), ParagraphDirection::Rtl);
    }

    #[test]
    fn detect_mixed_rtl_first() {
        let row = make_row("שלום hello");
        assert_eq!(detect_paragraph_direction(&row), ParagraphDirection::Rtl);
    }

    #[test]
    fn detect_mixed_ltr_first() {
        let row = make_row("hello שלום");
        assert_eq!(detect_paragraph_direction(&row), ParagraphDirection::Ltr);
    }

    #[test]
    fn pure_ltr_identity_mapping() {
        let row = make_row("hello");
        let marks = CombiningMarks::new();
        let cache = reorder_row(
            &row, &marks, 0,
            ParagraphDirection::Auto, TextAlignment::Left,
            80, 8.0,
        );
        assert!(!cache.has_rtl);
        assert_eq!(cache.logical_to_visual, vec![0, 1, 2, 3, 4]);
        assert_eq!(cache.visual_to_logical, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn rtl_reordering() {
        // Hebrew "שלום" should be visually reversed
        let row = make_row("שלום");
        let marks = CombiningMarks::new();
        let cache = reorder_row(
            &row, &marks, 0,
            ParagraphDirection::Rtl, TextAlignment::Left,
            80, 8.0,
        );
        assert!(cache.has_rtl);
        // In RTL paragraph, "שלום" (4 chars) should be reversed visually
        // Visual order: ם ו ל ש (right-to-left display)
        assert_eq!(cache.visual_to_logical, vec![3, 2, 1, 0]);
    }

    #[test]
    fn alignment_offset_right() {
        let row = make_row("שלום"); // 4 chars in 80 cols
        let marks = CombiningMarks::new();
        let cache = reorder_row(
            &row, &marks, 0,
            ParagraphDirection::Rtl, TextAlignment::Right,
            80, 8.0,
        );
        // free_space = (80 - 4) * 8.0 = 608.0
        assert_eq!(cache.alignment_offset_px, 608.0);
    }

    #[test]
    fn alignment_auto_rtl() {
        let row = make_row("שלום עולם");
        let marks = CombiningMarks::new();
        let cache = reorder_row(
            &row, &marks, 0,
            ParagraphDirection::Auto, TextAlignment::Auto,
            80, 8.0,
        );
        // Auto direction → RTL, Auto alignment → Right
        assert_eq!(cache.resolved_direction, ParagraphDirection::Rtl);
        assert!(cache.alignment_offset_px > 0.0);
    }

    #[test]
    fn alignment_center() {
        let row = make_row("test"); // 4 chars
        let marks = CombiningMarks::new();
        let cache = reorder_row(
            &row, &marks, 0,
            ParagraphDirection::Ltr, TextAlignment::Center,
            80, 10.0,
        );
        // free_space = (80 - 4) * 10.0 = 760.0, center = 380.0
        assert_eq!(cache.alignment_offset_px, 380.0);
    }

    /// Verify Hebrew chars in a full 80-col row produce contiguous visual positions.
    /// This is the suspected root cause of the "double-spacing" bug.
    #[test]
    fn hebrew_full_row_contiguous_visual() {
        // Simulate a terminal row: "שלום" + 76 spaces (80 cols total)
        let mut text = String::from("שלום");
        text.extend(std::iter::repeat(' ').take(76));
        let row = make_row(&text);
        let marks = CombiningMarks::new();
        let cache = reorder_row(
            &row, &marks, 0,
            ParagraphDirection::Auto, TextAlignment::Left,
            80, 8.0,
        );
        assert!(cache.has_rtl);
        assert_eq!(cache.resolved_direction, ParagraphDirection::Rtl);

        // Hebrew chars at logical [0,1,2,3] should map to contiguous visual positions
        let v0 = cache.logical_to_visual[0]; // ש
        let v1 = cache.logical_to_visual[1]; // ל
        let v2 = cache.logical_to_visual[2]; // ו
        let v3 = cache.logical_to_visual[3]; // ם

        // They should be consecutive (reversed: ם ו ל ש in visual order)
        let mut positions = vec![v0, v1, v2, v3];
        positions.sort();
        let min = *positions.first().unwrap();
        let max = *positions.last().unwrap();

        // Verify contiguous: max - min == 3 (4 consecutive positions)
        assert_eq!(
            max - min, 3,
            "Hebrew chars should be contiguous! positions={:?} (ש={}, ל={}, ו={}, ם={})",
            positions, v0, v1, v2, v3
        );
    }

    /// Verify mixed LTR+Hebrew after a prompt produces correct mapping.
    #[test]
    fn mixed_prompt_with_hebrew() {
        // "$ echo שלום" + trailing spaces to fill 80 cols
        let mut text = String::from("$ echo שלום");
        text.extend(std::iter::repeat(' ').take(80 - text.chars().count()));
        let row = make_row(&text);
        let marks = CombiningMarks::new();
        let cache = reorder_row(
            &row, &marks, 0,
            ParagraphDirection::Auto, TextAlignment::Left,
            80, 8.0,
        );

        // LTR paragraph (first strong char is 'e')
        assert_eq!(cache.resolved_direction, ParagraphDirection::Ltr);

        // Hebrew chars "שלום" are at logical indices 7,8,9,10
        let v7 = cache.logical_to_visual[7];
        let v8 = cache.logical_to_visual[8];
        let v9 = cache.logical_to_visual[9];
        let v10 = cache.logical_to_visual[10];

        let mut hebrew_positions = vec![v7, v8, v9, v10];
        hebrew_positions.sort();
        let min = *hebrew_positions.first().unwrap();
        let max = *hebrew_positions.last().unwrap();

        assert_eq!(
            max - min, 3,
            "Hebrew chars in mixed line should be contiguous! positions={:?}", hebrew_positions
        );

        // LTR prefix should be at identity positions
        for i in 0..7 {
            assert_eq!(
                cache.logical_to_visual[i], i,
                "LTR char at logical {} should be at visual {}", i, i
            );
        }
    }

    /// Verify that unicode-width gives width 1 for Hebrew characters.
    #[test]
    fn hebrew_char_width_is_1() {
        use ::unicode_width::UnicodeWidthChar;
        for ch in "שלום עולם הכל טוב".chars() {
            if ch == ' ' { continue; }
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            assert_eq!(w, 1, "Hebrew char '{}' (U+{:04X}) should have width 1, got {}", ch, ch as u32, w);
        }
    }
}
