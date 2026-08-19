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
                break;
            }

            let leading_blanks = next.cells.iter()
                .take_while(|cell| matches!(cell.grapheme, ' ' | '\0' | '\u{a0}'))
                .count();
            if leading_blanks >= 2 || rows[next_idx - 1].wrapped {
                highlighted[next_idx] = true;
                continue;
            }
            break;
        }
    }

    highlighted
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
    fn codex_menu_choices_are_not_prompt_cards() {
        let top = row("");
        let choice = row("› 1. Yes, continue");
        let bottom = row("");
        let rows = [&top, &choice, &bottom];

        assert_eq!(codex_prompt_highlight_mask(&rows), vec![false; 3]);
    }
}
