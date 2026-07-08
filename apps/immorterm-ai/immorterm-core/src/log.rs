//! Structured terminal log types and conversion functions.
//!
//! Three log formats:
//! 1. `.grid.jsonl` — Grid state snapshots for restoration + search
//! 2. `.cast` — Asciicast v2 stream for replay
//! 3. `.ai.jsonl` — AI conversation log for memory integration
//!
//! This module defines the schema structs and the core conversion from
//! `Grid`/`Scrollback` → compact attribute runs (JSON-serializable).

use serde::{Deserialize, Serialize};

use crate::cell::{CellAttrs, Color};
use crate::grid::{Grid, Row};
use crate::scrollback::Scrollback;

// ── Schema types ─────────────────────────────────────────────────────

/// Color representation in log files.
/// Matches the TypeScript schema: `"default"`, number (indexed), or `[r, g, b]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum LogColor {
    /// Terminal default color
    Default(DefaultColor),
    /// 256-color palette index (0-255)
    Indexed(u8),
    /// True color RGB
    Rgb([u8; 3]),
}

/// Sentinel for "default" color — serializes as the string `"default"`.
#[derive(Debug, Clone, PartialEq)]
pub struct DefaultColor;

impl Serialize for DefaultColor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("default")
    }
}

impl<'de> Deserialize<'de> for DefaultColor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "default" {
            Ok(DefaultColor)
        } else {
            Err(serde::de::Error::custom("expected \"default\""))
        }
    }
}

impl From<Color> for LogColor {
    fn from(color: Color) -> Self {
        match color {
            Color::Default => LogColor::Default(DefaultColor),
            Color::Indexed(i) => LogColor::Indexed(i),
            Color::Rgb(r, g, b) => LogColor::Rgb([r, g, b]),
        }
    }
}

impl From<&LogColor> for Color {
    fn from(color: &LogColor) -> Self {
        match color {
            LogColor::Default(_) => Color::Default,
            LogColor::Indexed(i) => Color::Indexed(*i),
            LogColor::Rgb([r, g, b]) => Color::Rgb(*r, *g, *b),
        }
    }
}

/// Attribute bitfield for log files.
///
/// Mapping: 1=bold, 2=italic, 4=underline, 8=strikethrough, 16=dim, 32=inverse, 64=blink
pub fn attrs_to_bitfield(attrs: CellAttrs) -> u8 {
    let mut bits: u8 = 0;
    if attrs.contains(CellAttrs::BOLD) {
        bits |= 1;
    }
    if attrs.contains(CellAttrs::ITALIC) {
        bits |= 2;
    }
    if attrs.contains(CellAttrs::UNDERLINE)
        || attrs.contains(CellAttrs::DOUBLE_UNDERLINE)
        || attrs.contains(CellAttrs::CURLY_UNDERLINE)
        || attrs.contains(CellAttrs::DOTTED_UNDERLINE)
        || attrs.contains(CellAttrs::DASHED_UNDERLINE)
    {
        bits |= 4;
    }
    if attrs.contains(CellAttrs::STRIKETHROUGH) {
        bits |= 8;
    }
    if attrs.contains(CellAttrs::DIM) {
        bits |= 16;
    }
    if attrs.contains(CellAttrs::INVERSE) {
        bits |= 32;
    }
    if attrs.contains(CellAttrs::BLINK) {
        bits |= 64;
    }
    bits
}

/// Convert a log bitfield back to CellAttrs.
pub fn bitfield_to_attrs(bits: u8) -> CellAttrs {
    let mut attrs = CellAttrs::empty();
    if bits & 1 != 0 {
        attrs |= CellAttrs::BOLD;
    }
    if bits & 2 != 0 {
        attrs |= CellAttrs::ITALIC;
    }
    if bits & 4 != 0 {
        attrs |= CellAttrs::UNDERLINE;
    }
    if bits & 8 != 0 {
        attrs |= CellAttrs::STRIKETHROUGH;
    }
    if bits & 16 != 0 {
        attrs |= CellAttrs::DIM;
    }
    if bits & 32 != 0 {
        attrs |= CellAttrs::INVERSE;
    }
    if bits & 64 != 0 {
        attrs |= CellAttrs::BLINK;
    }
    attrs
}

/// A single attribute run — consecutive cells with identical visual properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeRun {
    /// Text content
    pub t: String,
    /// Foreground color
    pub fg: LogColor,
    /// Background color
    pub bg: LogColor,
    /// Attribute bitfield
    pub a: u8,
    /// Repeat count for trailing spaces (omitted if 0)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub r: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A row encoded as attribute runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowRuns {
    pub row: usize,
    pub runs: Vec<AttributeRun>,
    /// Whether this row soft-wraps to the next row.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wrapped: bool,
}

/// A line in a scrollback dump (no row index needed, sequential).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrollbackLine {
    pub runs: Vec<AttributeRun>,
    /// Whether this line soft-wraps to the next line.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wrapped: bool,
}

/// Trigger that caused this snapshot.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotTrigger {
    Prompt,
    Periodic,
    Shutdown,
    Manual,
}

/// Grid state snapshot — the primary log entry in `.grid.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridSnapshot {
    /// Schema version
    pub v: u8,
    /// Record type discriminator
    #[serde(rename = "type")]
    pub record_type: String,
    /// Unix timestamp with millisecond precision
    pub ts: f64,
    /// What triggered this snapshot
    pub trigger: SnapshotTrigger,
    /// Terminal columns
    pub cols: usize,
    /// Terminal rows
    pub rows: usize,
    /// Cursor position
    pub cursor: CursorPos,
    /// Current working directory
    pub cwd: String,
    /// Last command exit code (null if unknown)
    pub exit_code: Option<i32>,
    /// Grid rows as attribute runs (only non-empty rows)
    pub grid: Vec<RowRuns>,
    /// Number of scrollback lines at time of snapshot
    pub sb_lines: usize,
    /// Hash of scrollback state (links to ScrollbackDump)
    pub sb_hash: String,
}

/// Scrollback dump — periodic full dump of scrollback history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrollbackDump {
    /// Schema version
    pub v: u8,
    /// Record type discriminator
    #[serde(rename = "type")]
    pub record_type: String,
    /// Unix timestamp
    pub ts: f64,
    /// All scrollback lines as attribute runs
    pub lines: Vec<ScrollbackLine>,
    /// Hash of this dump (referenced by GridSnapshot.sb_hash)
    pub hash: String,
}

/// Cursor position in a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorPos {
    pub col: usize,
    pub row: usize,
}

// ── Prompt events (for structured logging triggers) ──────────────────

/// Semantic prompt events from OSC 133.
#[derive(Debug, Clone)]
pub enum PromptEvent {
    /// OSC 133;A — Prompt start (shell is about to draw prompt)
    PromptStart,
    /// OSC 133;B — Input start (user is typing)
    InputStart,
    /// OSC 133;C — Command output start
    OutputStart,
    /// OSC 133;D — Command done (with exit code)
    CommandDone { exit_code: i32 },
}

// ── Conversion functions ─────────────────────────────────────────────

/// Compress a Row into attribute runs.
///
/// Groups consecutive cells with identical (fg, bg, attrs) into runs.
/// Trailing default-attribute spaces use the `r` (repeat) field.
pub fn row_to_runs(row: &Row) -> Vec<AttributeRun> {
    let cells = &row.cells;
    if cells.is_empty() {
        return vec![];
    }

    let mut runs: Vec<AttributeRun> = Vec::with_capacity(8);
    let mut i = 0;

    while i < cells.len() {
        let cell = &cells[i];

        // Skip wide-char continuation cells
        if cell.is_wide_continuation() {
            i += 1;
            continue;
        }

        let fg = LogColor::from(cell.fg);
        let bg = LogColor::from(cell.bg);
        let a = attrs_to_bitfield(cell.attrs);

        let mut text = String::new();
        text.push(cell.grapheme);
        i += 1;

        // Extend run with consecutive cells having same visual attrs
        while i < cells.len() {
            let next = &cells[i];
            if next.is_wide_continuation() {
                i += 1;
                continue;
            }
            let next_fg = LogColor::from(next.fg);
            let next_bg = LogColor::from(next.bg);
            let next_a = attrs_to_bitfield(next.attrs);
            if next_fg == fg && next_bg == bg && next_a == a {
                text.push(next.grapheme);
                i += 1;
            } else {
                break;
            }
        }

        runs.push(AttributeRun {
            t: text,
            fg,
            bg,
            a,
            r: 0,
        });
    }

    // Optimize: if last run is all spaces with default attrs, use repeat field
    if let Some(last) = runs.last_mut()
        && last.a == 0
            && last.fg == LogColor::Default(DefaultColor)
            && last.bg == LogColor::Default(DefaultColor)
        {
            let trimmed = last.t.trim_end_matches(' ');
            let trailing_spaces = last.t.len() - trimmed.len();
            if trailing_spaces > 0 && !trimmed.is_empty() {
                last.r = trailing_spaces;
                last.t = trimmed.to_string();
            } else if trimmed.is_empty() && trailing_spaces > 0 {
                // Entire run is spaces — use repeat
                last.t = " ".to_string();
                last.r = trailing_spaces - 1; // "t" has 1 space, repeat has the rest
            }
        }

    // Trim trailing empty runs (all-space default runs at end of row)
    while let Some(last) = runs.last() {
        let is_all_space = last.t.chars().all(|c| c == ' ')
            && last.a == 0
            && last.fg == LogColor::Default(DefaultColor)
            && last.bg == LogColor::Default(DefaultColor);
        if is_all_space {
            runs.pop();
        } else {
            break;
        }
    }

    runs
}

/// Expand attribute runs back into a Row (inverse of `row_to_runs`).
///
/// Each `AttributeRun` is exploded into individual `Cell`s with matching
/// colors/attributes. The `r` (repeat) field appends trailing spaces.
/// The row is padded or truncated to exactly `cols` columns.
pub fn runs_to_row(runs: &[AttributeRun], cols: usize, wrapped: bool) -> Row {
    use crate::cell::Cell;

    let mut cells = Vec::with_capacity(cols);

    for run in runs {
        let fg = Color::from(&run.fg);
        let bg = Color::from(&run.bg);
        let attrs = bitfield_to_attrs(run.a);

        for ch in run.t.chars() {
            if cells.len() >= cols {
                break;
            }
            cells.push(Cell {
                grapheme: ch,
                attrs,
                fg,
                bg,
                ..Cell::default()
            });
        }

        // Repeat field: trailing spaces with same attributes
        for _ in 0..run.r {
            if cells.len() >= cols {
                break;
            }
            cells.push(Cell {
                grapheme: ' ',
                attrs,
                fg,
                bg,
                ..Cell::default()
            });
        }
    }

    // Pad remaining columns with default cells
    cells.resize_with(cols, Cell::default);

    // Compute content_end_col: rightmost non-space cell + 1
    let content_end = cells
        .iter()
        .rposition(|c| c.grapheme != ' ' && c.width > 0)
        .map(|p| p + 1)
        .unwrap_or(0);

    Row {
        cells,
        dirty: false,
        wrapped,
        soft_wrapped: false,
        content_end_col: content_end,
        direction: None,
        alignment: None,
    }
}

/// Convert the grid to a vector of RowRuns (only non-empty rows).
pub fn grid_to_row_runs(grid: &Grid) -> Vec<RowRuns> {
    let mut result = Vec::new();
    for (row_idx, row) in grid.rows().enumerate() {
        let runs = row_to_runs(row);
        if !runs.is_empty() {
            result.push(RowRuns {
                row: row_idx,
                runs,
                wrapped: row.wrapped,
            });
        }
    }
    result
}

/// Create a full GridSnapshot from terminal state.
#[allow(clippy::too_many_arguments)]
pub fn grid_to_snapshot(
    grid: &Grid,
    scrollback: &Scrollback,
    cursor_col: usize,
    cursor_row: usize,
    cols: usize,
    rows: usize,
    cwd: &str,
    exit_code: Option<i32>,
    trigger: SnapshotTrigger,
    sb_hash: &str,
) -> GridSnapshot {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    GridSnapshot {
        v: 1,
        record_type: "snapshot".to_string(),
        ts,
        trigger,
        cols,
        rows,
        cursor: CursorPos {
            col: cursor_col,
            row: cursor_row,
        },
        cwd: cwd.to_string(),
        exit_code,
        grid: grid_to_row_runs(grid),
        sb_lines: scrollback.len(),
        sb_hash: sb_hash.to_string(),
    }
}

/// Create a ScrollbackDump from the scrollback buffer.
pub fn scrollback_to_dump(scrollback: &Scrollback) -> ScrollbackDump {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let lines: Vec<ScrollbackLine> = scrollback
        .iter()
        .map(|row| ScrollbackLine {
            runs: row_to_runs(row),
            wrapped: row.wrapped,
        })
        .collect();

    // Simple hash: line count + first/last content
    let hash = compute_scrollback_hash(&lines, scrollback.len());

    ScrollbackDump {
        v: 1,
        record_type: "scrollback".to_string(),
        ts,
        lines,
        hash,
    }
}

/// Compute a lightweight hash for scrollback state (for dedup).
fn compute_scrollback_hash(lines: &[ScrollbackLine], len: usize) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    len.hash(&mut hasher);
    // Hash first and last line content for change detection
    if let Some(first) = lines.first() {
        for run in &first.runs {
            run.t.hash(&mut hasher);
        }
    }
    if let Some(last) = lines.last() {
        for run in &last.runs {
            run.t.hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

// ── ANSI restoration ─────────────────────────────────────────────────

/// Convert a LogColor to SGR parameters for foreground.
fn color_to_sgr_fg(color: &LogColor) -> String {
    match color {
        LogColor::Default(_) => "39".to_string(),
        LogColor::Indexed(i) => format!("38;5;{}", i),
        LogColor::Rgb([r, g, b]) => format!("38;2;{};{};{}", r, g, b),
    }
}

/// Convert a LogColor to SGR parameters for background.
fn color_to_sgr_bg(color: &LogColor) -> String {
    match color {
        LogColor::Default(_) => "49".to_string(),
        LogColor::Indexed(i) => format!("48;5;{}", i),
        LogColor::Rgb([r, g, b]) => format!("48;2;{};{};{}", r, g, b),
    }
}

/// Convert attribute runs to an ANSI-escaped string.
///
/// Used for session restoration: write this to the terminal and you get
/// the exact visual appearance back (colors, bold, etc.).
pub fn runs_to_ansi(runs: &[AttributeRun]) -> String {
    let mut out = String::with_capacity(256);
    let default_fg = LogColor::Default(DefaultColor);
    let default_bg = LogColor::Default(DefaultColor);

    for run in runs {
        // Build SGR sequence
        let mut sgr_parts: Vec<String> = Vec::new();
        sgr_parts.push("0".to_string()); // Reset first

        if run.fg != default_fg {
            sgr_parts.push(color_to_sgr_fg(&run.fg));
        }
        if run.bg != default_bg {
            sgr_parts.push(color_to_sgr_bg(&run.bg));
        }
        if run.a & 1 != 0 {
            sgr_parts.push("1".to_string()); // bold
        }
        if run.a & 2 != 0 {
            sgr_parts.push("3".to_string()); // italic
        }
        if run.a & 4 != 0 {
            sgr_parts.push("4".to_string()); // underline
        }
        if run.a & 8 != 0 {
            sgr_parts.push("9".to_string()); // strikethrough
        }
        if run.a & 16 != 0 {
            sgr_parts.push("2".to_string()); // dim
        }
        if run.a & 32 != 0 {
            sgr_parts.push("7".to_string()); // inverse
        }
        if run.a & 64 != 0 {
            sgr_parts.push("5".to_string()); // blink
        }

        // Only emit SGR if there are non-default attributes
        let has_attrs = run.fg != default_fg
            || run.bg != default_bg
            || run.a != 0;
        if has_attrs {
            out.push_str(&format!("\x1b[{}m", sgr_parts.join(";")));
        } else {
            out.push_str("\x1b[0m");
        }

        // Text content
        out.push_str(&run.t);

        // Repeat (trailing spaces)
        if run.r > 0 {
            for _ in 0..run.r {
                out.push(' ');
            }
        }
    }

    // Reset at end
    out.push_str("\x1b[0m");
    out
}

/// Convert a full GridSnapshot to ANSI string for restoration.
///
/// Uses the `wrapped` flag to join soft-wrapped rows into logical lines.
/// Only emits `\n` at hard breaks (`wrapped == false`), so the output
/// reflows correctly at any terminal width.
///
/// Trailing whitespace is stripped from each line to avoid column-width-dependent
/// padding (e.g. a snapshot captured at 104 cols would pad lines with spaces to
/// col 104, causing wrapping artifacts when restored at a different width).
///
/// Uses `\r\n` (CR+LF) instead of bare `\n` because this ANSI is fed directly
/// to `terminal.process()` bypassing the PTY. The PTY's `ONLCR` termios flag
/// normally translates `\n` → `\r\n`, but since we bypass it, bare `\n` only
/// moves the cursor down without returning to column 0, causing cascading
/// horizontal misalignment.
pub fn snapshot_to_ansi(snapshot: &GridSnapshot, scrollback: Option<&ScrollbackDump>) -> String {
    let mut out = String::with_capacity(4096);

    // Scrollback first — join wrapped lines into logical lines
    if let Some(sb) = scrollback {
        for line in &sb.lines {
            out.push_str(&trim_ansi_trailing_spaces(&runs_to_ansi(&line.runs)));
            if !line.wrapped {
                out.push_str("\r\n");
            }
        }
    }

    // Grid rows — fill gaps between recorded rows with empty lines
    let mut last_row = 0;
    for row_runs in &snapshot.grid {
        // Fill empty rows between last and current
        while last_row < row_runs.row {
            out.push_str("\r\n");
            last_row += 1;
        }
        out.push_str(&trim_ansi_trailing_spaces(&runs_to_ansi(&row_runs.runs)));
        // Only emit newline at hard breaks, not soft wraps
        if !row_runs.wrapped && row_runs.row < snapshot.rows.saturating_sub(1) {
            out.push_str("\r\n");
        }
        last_row = row_runs.row + 1;
    }

    out
}

/// Strip trailing spaces from an ANSI string produced by `runs_to_ansi`.
///
/// `runs_to_ansi` always appends a final `\x1b[0m` reset. We strip trailing
/// spaces that appear before this reset — these are column-padding artifacts
/// from the fixed-width grid and serve no visual purpose.
fn trim_ansi_trailing_spaces(ansi: &str) -> String {
    const RESET: &str = "\x1b[0m";
    if let Some(before_reset) = ansi.strip_suffix(RESET) {
        let trimmed = before_reset.trim_end_matches(' ');
        if trimmed.len() < before_reset.len() {
            return format!("{}{}", trimmed, RESET);
        }
    }
    ansi.to_string()
}

/// Strip attribute runs to plain text (for search indexing).
pub fn strip_runs(runs: &[AttributeRun]) -> String {
    let mut out = String::with_capacity(128);
    for run in runs {
        out.push_str(&run.t);
        if run.r > 0 {
            for _ in 0..run.r {
                out.push(' ');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::Cell;
    use crate::grid::Row;

    fn make_row(chars: &str) -> Row {
        let mut row = Row::new(chars.len());
        for (i, c) in chars.chars().enumerate() {
            row.cells[i].grapheme = c;
        }
        row
    }

    fn make_colored_row() -> Row {
        let mut row = Row::new(20);
        // "hello" in green
        for (i, c) in "hello".chars().enumerate() {
            row.cells[i].grapheme = c;
            row.cells[i].fg = Color::Rgb(0, 255, 0);
        }
        // " " spaces in default
        // "world" in red+bold
        for (i, c) in "world".chars().enumerate() {
            row.cells[i + 6].grapheme = c;
            row.cells[i + 6].fg = Color::Rgb(255, 0, 0);
            row.cells[i + 6].attrs = CellAttrs::BOLD;
        }
        row
    }

    #[test]
    fn row_to_runs_basic() {
        let row = make_row("hello     ");
        let runs = row_to_runs(&row);
        // "hello" + trailing spaces get trimmed
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].t, "hello");
    }

    #[test]
    fn row_to_runs_colored() {
        let row = make_colored_row();
        let runs = row_to_runs(&row);
        // Should have: "hello" (green), " " (default), "world" (red+bold)
        // Trailing spaces trimmed
        assert!(runs.len() >= 3);
        assert_eq!(runs[0].t, "hello");
        assert_eq!(runs[0].fg, LogColor::Rgb([0, 255, 0]));
        // Last run with content should be "world"
        let world_run = runs.iter().find(|r| r.t.contains("world")).unwrap();
        assert_eq!(world_run.fg, LogColor::Rgb([255, 0, 0]));
        assert_eq!(world_run.a & 1, 1); // bold
    }

    #[test]
    fn runs_to_ansi_roundtrip() {
        let runs = vec![
            AttributeRun {
                t: "hello".to_string(),
                fg: LogColor::Rgb([0, 255, 0]),
                bg: LogColor::Default(DefaultColor),
                a: 0,
                r: 1,
            },
            AttributeRun {
                t: "world".to_string(),
                fg: LogColor::Rgb([255, 0, 0]),
                bg: LogColor::Default(DefaultColor),
                a: 1, // bold
                r: 0,
            },
        ];
        let ansi = runs_to_ansi(&runs);
        assert!(ansi.contains("\x1b["));
        assert!(ansi.contains("38;2;0;255;0")); // green fg
        assert!(ansi.contains("38;2;255;0;0")); // red fg
        assert!(ansi.contains("hello"));
        assert!(ansi.contains("world"));
    }

    #[test]
    fn strip_runs_plain_text() {
        let runs = vec![
            AttributeRun {
                t: "hello".to_string(),
                fg: LogColor::Default(DefaultColor),
                bg: LogColor::Default(DefaultColor),
                a: 0,
                r: 1,
            },
            AttributeRun {
                t: "world".to_string(),
                fg: LogColor::Default(DefaultColor),
                bg: LogColor::Default(DefaultColor),
                a: 0,
                r: 0,
            },
        ];
        assert_eq!(strip_runs(&runs), "hello world");
    }

    #[test]
    fn color_conversion_roundtrip() {
        let colors = vec![
            Color::Default,
            Color::Indexed(42),
            Color::Rgb(128, 64, 255),
        ];
        for color in colors {
            let log_color = LogColor::from(color);
            let back: Color = Color::from(&log_color);
            assert_eq!(color, back);
        }
    }

    #[test]
    fn attrs_roundtrip() {
        let attrs = CellAttrs::BOLD | CellAttrs::ITALIC | CellAttrs::UNDERLINE;
        let bits = attrs_to_bitfield(attrs);
        assert_eq!(bits, 1 | 2 | 4);
        let back = bitfield_to_attrs(bits);
        assert!(back.contains(CellAttrs::BOLD));
        assert!(back.contains(CellAttrs::ITALIC));
        assert!(back.contains(CellAttrs::UNDERLINE));
    }

    fn plain_run(text: &str) -> AttributeRun {
        AttributeRun {
            t: text.to_string(),
            fg: LogColor::Default(DefaultColor),
            bg: LogColor::Default(DefaultColor),
            a: 0,
            r: 0,
        }
    }

    fn test_snapshot(cols: usize, rows: usize, grid: Vec<RowRuns>) -> GridSnapshot {
        GridSnapshot {
            v: 1,
            record_type: "grid".to_string(),
            ts: 0.0,
            trigger: SnapshotTrigger::Manual,
            cols,
            rows,
            cursor: CursorPos { row: 0, col: 0 },
            cwd: String::new(),
            exit_code: None,
            grid,
            sb_lines: 0,
            sb_hash: String::new(),
        }
    }

    /// Strip ANSI escape sequences for test assertions.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut in_esc = false;
        for c in s.chars() {
            if in_esc {
                if c.is_ascii_alphabetic() {
                    in_esc = false;
                }
            } else if c == '\x1b' {
                in_esc = true;
            } else {
                out.push(c);
            }
        }
        out
    }

    fn test_scrollback(lines: Vec<ScrollbackLine>) -> ScrollbackDump {
        ScrollbackDump {
            v: 1,
            record_type: "scrollback".to_string(),
            ts: 0.0,
            lines,
            hash: "test".to_string(),
        }
    }

    #[test]
    fn snapshot_to_ansi_joins_wrapped_rows() {
        // Simulate a long line "AAABBB" wrapped across 2 grid rows (3 cols each)
        let snapshot = test_snapshot(3, 3, vec![
            RowRuns { row: 0, runs: vec![plain_run("AAA")], wrapped: true },
            RowRuns { row: 1, runs: vec![plain_run("BBB")], wrapped: false },
            RowRuns { row: 2, runs: vec![], wrapped: false },
        ]);
        let ansi = snapshot_to_ansi(&snapshot, None);
        let plain = strip_ansi(&ansi);
        // Wrapped rows should NOT have \n between them — "AAABBB" on one logical line
        assert!(plain.contains("AAABBB"), "wrapped rows should be joined: {:?}", plain);
        // The hard break after row 1 should produce a \r\n (CR+LF, since this
        // ANSI bypasses the PTY and its ONLCR translation)
        let parts: Vec<&str> = plain.split("\r\n").collect();
        assert_eq!(parts[0], "AAABBB");
    }

    #[test]
    fn snapshot_to_ansi_scrollback_wrapped() {
        let snapshot = test_snapshot(4, 2, vec![
            RowRuns { row: 0, runs: vec![plain_run("grid")], wrapped: false },
        ]);
        let scrollback = test_scrollback(vec![
            ScrollbackLine { runs: vec![plain_run("XXXX")], wrapped: true },
            ScrollbackLine { runs: vec![plain_run("YYYY")], wrapped: false },
            ScrollbackLine { runs: vec![plain_run("ZZZZ")], wrapped: false },
        ]);
        let ansi = snapshot_to_ansi(&snapshot, Some(&scrollback));
        let plain = strip_ansi(&ansi);
        // "XXXX" wraps into "YYYY" = one logical line "XXXXYYYY"
        assert!(plain.contains("XXXXYYYY"), "scrollback wrapped lines should join: {:?}", plain);
        // "ZZZZ" is a separate hard-break line
        assert!(plain.contains("ZZZZ"));
    }

    #[test]
    fn snapshot_to_ansi_no_wrap_backward_compat() {
        // Old-style snapshot without wrapped flag (all default false)
        let snapshot = test_snapshot(5, 2, vec![
            RowRuns { row: 0, runs: vec![plain_run("hello")], wrapped: false },
            RowRuns { row: 1, runs: vec![plain_run("world")], wrapped: false },
        ]);
        let ansi = snapshot_to_ansi(&snapshot, None);
        let plain = strip_ansi(&ansi);
        // Each row gets its own \r\n (CR+LF, bypasses PTY ONLCR)
        assert!(plain.contains("hello"), "row 0 content: {:?}", plain);
        assert!(plain.contains("world"), "row 1 content: {:?}", plain);
        // Should NOT be joined
        assert!(!plain.contains("helloworld"), "non-wrapped rows should not join: {:?}", plain);
        // Verify CR+LF separator
        assert!(plain.contains("hello\r\nworld"), "should use \\r\\n: {:?}", plain);
    }

    #[test]
    fn runs_to_row_roundtrip() {
        // Create a colored row, convert to runs, convert back, verify cells match
        let original = make_colored_row();
        let runs = row_to_runs(&original);
        let restored = runs_to_row(&runs, 20, false);

        // Verify content matches (first 11 chars: "hello world" + spaces)
        for i in 0..11 {
            assert_eq!(
                original.cells[i].grapheme, restored.cells[i].grapheme,
                "grapheme mismatch at cell {}", i
            );
            assert_eq!(
                original.cells[i].fg, restored.cells[i].fg,
                "fg mismatch at cell {}", i
            );
            assert_eq!(
                original.cells[i].bg, restored.cells[i].bg,
                "bg mismatch at cell {}", i
            );
        }
        assert!(!restored.wrapped);
        assert_eq!(restored.cells.len(), 20);
    }

    #[test]
    fn runs_to_row_with_repeat() {
        let runs = vec![
            AttributeRun {
                t: "hi".to_string(),
                fg: LogColor::Default(DefaultColor),
                bg: LogColor::Default(DefaultColor),
                a: 0,
                r: 3, // 3 trailing spaces
            },
        ];
        let row = runs_to_row(&runs, 10, true);
        assert_eq!(row.cells[0].grapheme, 'h');
        assert_eq!(row.cells[1].grapheme, 'i');
        assert_eq!(row.cells[2].grapheme, ' '); // repeat
        assert_eq!(row.cells[3].grapheme, ' '); // repeat
        assert_eq!(row.cells[4].grapheme, ' '); // repeat
        assert_eq!(row.cells[5].grapheme, ' '); // padding
        assert!(row.wrapped);
    }
}
