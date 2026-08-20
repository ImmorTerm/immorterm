//! Per-agent TUI conventions.
//!
//! ImmorTerm reads structure out of the terminal grid — where the composer
//! starts and ends, what marks a turn, what a placeholder looks like. Every one
//! of those reads was originally written against Claude Code's Ink renderer.
//! Other agents draw the same concepts differently, and guessing wrong is not
//! cosmetic: select-all swallows the placeholder, arrow planning aims at the
//! wrong row, and hovers resolve against a cache that belongs to a different
//! tool.
//!
//! The Codex values here were transcribed from a live `codex-cli` 0.145
//! session, not inferred.

use crate::grid::Row;

/// Which agent's TUI conventions to assume when reading the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AiDialect {
    /// Claude Code (Ink): `❯ ` composer closed by a `─` rule row, `[Image #N]`
    /// paste pills, `⎿` tool-output indent, `●`/`⏺` turn markers, and a block
    /// cursor drawn as an INVERSE cell.
    #[default]
    Claude,
    /// Codex CLI: `› ` composer with NO rule row beneath it, `•` assistant
    /// marker, a dim placeholder with no leading inverse cell, and the real
    /// terminal cursor left visible.
    Codex,
}

impl AiDialect {
    /// Map the daemon's `AiTool::name()` string onto a dialect.
    ///
    /// Unknown tools keep Claude's behaviour — that is what shipped for years,
    /// and a wrong guess is worse than the status quo.
    pub fn from_tool(tool: &str) -> Self {
        match tool {
            "codex" => AiDialect::Codex,
            _ => AiDialect::Claude,
        }
    }

    /// Glyph that marks the start of the composer input row.
    pub fn prompt_sentinel(self) -> char {
        match self {
            AiDialect::Claude => '\u{276F}', // ❯
            AiDialect::Codex => '\u{203A}',  // ›
        }
    }

    /// Whether this agent closes its composer with a horizontal rule row.
    /// Codex does not — its input area ends at the blank row before the
    /// `<model> <effort> · <cwd>` footer.
    pub fn composer_has_rule_row(self) -> bool {
        matches!(self, AiDialect::Claude)
    }
}

/// True when a row holds nothing but blanks.
pub fn row_is_blank(row: &Row) -> bool {
    row.cells
        .iter()
        .all(|c| c.grapheme == ' ' || c.grapheme == '\0' || c.grapheme == '\u{a0}')
}

/// True when the text right after a sentinel at `sentinel_col` looks like a
/// menu option — `1. `, `2. `, … — rather than a typed prompt.
///
/// Codex reuses `›` to mark the highlighted row in its menus (for example
/// `› 1. Yes, continue` on the hook-trust prompt). A bottom-up sentinel scan
/// that latched onto one of those would aim every subsequent edit at a menu row
/// instead of the composer.
pub fn row_is_menu_option(row: &Row, sentinel_col: usize) -> bool {
    let mut saw_digit = false;
    for g in row
        .cells
        .iter()
        .skip(sentinel_col + 1)
        .map(|c| c.grapheme)
        .skip_while(|g| *g == ' ')
    {
        if g.is_ascii_digit() {
            saw_digit = true;
            continue;
        }
        // Digits only count as a menu marker when followed by a period.
        return saw_digit && g == '.';
    }
    false
}

/// Return a mask for rows that belong to a Codex user-prompt card.
///
/// Codex writes submitted prompts and its live composer as ordinary terminal
/// rows (`› ...`) with default backgrounds. ImmorTerm recreates the native
/// presentation from the grid structure. A card includes one blank padding
/// row on either side and any indented/wrapped continuation rows. Numbered
/// `› 1. ...` menu choices are deliberately excluded.
pub fn codex_prompt_highlight_mask(rows: &[&Row]) -> Vec<bool> {
    let mut highlighted = vec![false; rows.len()];

    for (idx, row) in rows.iter().enumerate() {
        let Some(sentinel_col) = row.cells.iter().position(|cell| {
            !matches!(cell.grapheme, ' ' | '\0' | '\u{a0}')
        }) else {
            continue;
        };
        if row.cells[sentinel_col].grapheme != AiDialect::Codex.prompt_sentinel()
            || row_is_menu_option(row, sentinel_col)
        {
            continue;
        }

        highlighted[idx] = true;
        if idx > 0 && row_is_blank(rows[idx - 1]) {
            highlighted[idx - 1] = true;
        }

        for next_idx in (idx + 1)..rows.len() {
            let next = rows[next_idx];
            if row_is_blank(next) {
                highlighted[next_idx] = true;
                // Submitted prompts may contain explicit blank lines. Keep
                // them inside the card until a structural boundary arrives.
                continue;
            }

            let first = next.cells.iter()
                .find(|cell| !matches!(cell.grapheme, ' ' | '\0' | '\u{a0}'))
                .map(|cell| cell.grapheme);
            // Explicit newlines in a submitted Codex prompt start at column 0,
            // so indentation cannot define the card boundary. Codex separates
            // the prompt from the next assistant turn with a blank padding row;
            // these sentinels are a defensive stop for transient redraws where
            // that row has not arrived yet.
            if matches!(first, Some('\u{2022}' | '\u{203A}'))
                || row_is_codex_footer(next)
                || row_is_rule(next)
            {
                break;
            }
            highlighted[next_idx] = true;
        }
    }

    highlighted
}

fn row_text(row: &Row) -> String {
    row.cells.iter().filter(|cell| cell.width > 0).map(|cell| cell.grapheme).collect()
}

fn row_is_codex_footer(row: &Row) -> bool {
    let text = row_text(row);
    let trimmed = text.trim();
    trimmed.contains(" · ") && (trimmed.contains("~/") || trimmed.contains(" context left"))
}

fn row_is_rule(row: &Row) -> bool {
    let text = row_text(row);
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed.chars().all(|c| matches!(c, '\u{2500}' | '\u{2501}' | '\u{2504}' | '\u{2505}'))
}

fn codex_prompt_text_from_start(rows: &[&Row], prompt_rows: &[bool], start: usize) -> String {
    let mut lines = Vec::new();
    for (idx, row) in rows.iter().enumerate().skip(start) {
        if !prompt_rows[idx] { break; }
        let mut text = row_text(row).trim_end().to_owned();
        if idx == start {
            text = text.trim_start().strip_prefix(AiDialect::Codex.prompt_sentinel())
                .unwrap_or(&text).trim_start().to_owned();
        }
        lines.push(text);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) { lines.pop(); }
    lines.join("\n")
}

fn codex_prompt_start(row: &Row) -> bool {
    let Some(first) = row.cells.iter().position(|cell| {
        !matches!(cell.grapheme, ' ' | '\0' | '\u{a0}')
    }) else { return false; };
    row.cells[first].grapheme == AiDialect::Codex.prompt_sentinel()
        && !row_is_menu_option(row, first)
}

/// Return the owning prompt and its 1-based occurrence counted from the end.
/// The reverse occurrence disambiguates identical image-only prompts while
/// remaining stable when older scrollback is pruned.
pub fn codex_prompt_identity_at(rows: &[&Row], target_row: usize) -> Option<(String, usize)> {
    if target_row >= rows.len() { return None; }
    let prompt_rows = codex_prompt_highlight_mask(rows);
    if !prompt_rows[target_row] { return None; }
    let start = (0..=target_row).rev().find(|idx| {
        let row = rows[*idx];
        codex_prompt_start(row)
    })?;
    let text = codex_prompt_text_from_start(rows, &prompt_rows, start);
    if text.trim().is_empty() { return None; }
    let newer_matches = ((start + 1)..rows.len())
        .filter(|idx| codex_prompt_start(rows[*idx]))
        .filter(|idx| codex_prompt_text_from_start(rows, &prompt_rows, *idx) == text)
        .count();
    Some((text, newer_matches + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Row {
        let mut r = Row::new(text.chars().count());
        for (i, g) in text.chars().enumerate() {
            r.cells[i].grapheme = g;
        }
        r.content_end_col = r.cells.len();
        r
    }

    #[test]
    fn maps_from_the_daemon_tool_string() {
        assert_eq!(AiDialect::from_tool("codex"), AiDialect::Codex);
        assert_eq!(AiDialect::from_tool("claude"), AiDialect::Claude);
        // Unknown or absent keeps the behaviour that shipped for years.
        assert_eq!(AiDialect::from_tool(""), AiDialect::Claude);
        assert_eq!(AiDialect::from_tool("cursor"), AiDialect::Claude);
        assert_eq!(AiDialect::default(), AiDialect::Claude);
    }

    #[test]
    fn each_dialect_has_its_own_composer_sentinel() {
        assert_eq!(AiDialect::Claude.prompt_sentinel(), '\u{276F}'); // ❯
        assert_eq!(AiDialect::Codex.prompt_sentinel(), '\u{203A}'); // ›
        assert!(AiDialect::Claude.composer_has_rule_row());
        assert!(!AiDialect::Codex.composer_has_rule_row());
    }

    #[test]
    fn codex_menu_rows_are_not_the_composer() {
        // Real rows from Codex's hook-trust prompt.
        assert!(row_is_menu_option(&row("\u{203A} 1. Yes, continue"), 0));
        assert!(row_is_menu_option(&row("\u{203A} 2. Trust all and continue"), 0));

        // A real prompt is not a menu, even when it opens with a digit.
        assert!(!row_is_menu_option(&row("\u{203A} 2 spaces or 4?"), 0));
        assert!(!row_is_menu_option(&row("\u{203A} fix the parser"), 0));
        // Empty composer.
        assert!(!row_is_menu_option(&row("\u{203A}"), 0));
    }

    #[test]
    fn blank_row_detection_ignores_nbsp_and_nul() {
        assert!(row_is_blank(&row("   ")));
        assert!(row_is_blank(&row("\u{a0}\u{a0}")));
        assert!(!row_is_blank(&row("  x  ")));
    }

    #[test]
    fn codex_prompt_cards_include_padding_and_continuations() {
        let top = row("");
        let prompt = row("› explain this screenshot");
        let image = row("  [Image #1]");
        let bottom = row("");
        let assistant = row("• I can help");
        let rows = [&top, &prompt, &image, &bottom, &assistant];

        assert_eq!(codex_prompt_highlight_mask(&rows), vec![true, true, true, true, false]);
    }

    #[test]
    fn codex_prompt_cards_include_unindented_explicit_newlines() {
        let top = row("");
        let prompt = row("› first line");
        let second = row("second line begins at column zero");
        let third = row("third line too");
        let bottom = row("");
        let assistant = row("• response");
        let rows = [&top, &prompt, &second, &third, &bottom, &assistant];

        assert_eq!(
            codex_prompt_highlight_mask(&rows),
            vec![true, true, true, true, true, false]
        );
    }

    #[test]
    fn codex_prompt_cards_keep_explicit_blank_lines_until_the_assistant_turn() {
        let rows = [row(""), row("› first paragraph"), row(""), row("second paragraph"), row(""), row("• response")];
        let refs: Vec<&Row> = rows.iter().collect();
        assert_eq!(codex_prompt_highlight_mask(&refs), vec![true, true, true, true, true, false]);
    }

    #[test]
    fn codex_live_composer_stops_before_model_footer() {
        let rows = [row("› draft prompt"), row(""), row("  gpt-5.6-sol medium · ~/Development/immorterm-org")];
        let refs: Vec<&Row> = rows.iter().collect();
        assert_eq!(codex_prompt_highlight_mask(&refs), vec![true, true, false]);
    }

    #[test]
    fn codex_menu_choices_are_not_prompt_cards() {
        let top = row("");
        let choice = row("› 1. Yes, continue");
        let bottom = row("");
        let rows = [&top, &choice, &bottom];

        assert_eq!(codex_prompt_highlight_mask(&rows), vec![false; 3]);
    }

    #[test]
    fn codex_image_prompt_text_is_scoped_to_owning_card() {
        let rows = [
            row("› first [Image #1]"), row(""), row("• response"), row(""),
            row("› second [Image #1] and [Image #2]"), row(""),
        ];
        let refs: Vec<&Row> = rows.iter().collect();

        assert_eq!(codex_prompt_identity_at(&refs, 0), Some(("first [Image #1]".into(), 1)));
        assert_eq!(
            codex_prompt_identity_at(&refs, 4),
            Some(("second [Image #1] and [Image #2]".into(), 1)),
        );
        assert_eq!(codex_prompt_identity_at(&refs, 2), None);
    }

    #[test]
    fn identical_codex_image_prompts_are_counted_from_the_end() {
        let rows = [
            row("› [Image #1]"), row(""), row("• response"), row(""),
            row("› [Image #1]"), row(""),
        ];
        let refs: Vec<&Row> = rows.iter().collect();
        assert_eq!(codex_prompt_identity_at(&refs, 0), Some(("[Image #1]".into(), 2)));
        assert_eq!(codex_prompt_identity_at(&refs, 4), Some(("[Image #1]".into(), 1)));
    }
}
