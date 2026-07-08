//! ASCII Art → SVG Diagram Pipeline
//!
//! Detects box-drawing and ASCII art characters in terminal text regions,
//! then converts them to clean SVG markup for GPU overlay rendering via DrawHtml.
//!
//! Supports:
//! - Unicode box-drawing characters (U+2500–U+257F)
//! - ASCII art patterns (`+`, `-`, `|`, `/`, `\`, `=`, `*`)
//! - Arrow characters (`→`, `←`, `↑`, `↓`, `>`, `<`, `^`, `v` in context)
//! - Text label preservation as `<text>` elements

use serde::{Deserialize, Serialize};
use std::fmt::Write;

/// Configuration for diagram conversion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagramConfig {
    /// Cell width in SVG units (default: 10.0)
    pub cell_width: f32,
    /// Cell height in SVG units (default: 20.0)
    pub cell_height: f32,
    /// Line color as CSS color string (default: "#7ec8e3")
    pub stroke_color: String,
    /// Line width (default: 2.0)
    pub stroke_width: f32,
    /// Text color as CSS color string (default: "#e0e0e0")
    pub text_color: String,
    /// Font size for text labels in SVG units (default: 14.0)
    pub font_size: f32,
}

impl Default for DiagramConfig {
    fn default() -> Self {
        Self {
            cell_width: 10.0,
            cell_height: 20.0,
            stroke_color: "#7ec8e3".to_string(),
            stroke_width: 2.0,
            text_color: "#e0e0e0".to_string(),
            font_size: 14.0,
        }
    }
}

/// Result of ASCII→SVG conversion.
#[derive(Debug, Clone)]
pub struct DiagramSvg {
    /// Complete SVG markup string
    pub svg: String,
    /// Width in SVG units
    pub width: f32,
    /// Height in SVG units
    pub height: f32,
    /// Number of diagram characters detected
    pub char_count: usize,
}

/// Check if a character is a box-drawing or ASCII art character.
fn is_diagram_char(c: char) -> bool {
    matches!(
        c,
        '\u{2500}'..='\u{257F}'  // Unicode box drawing
        | '+' | '-' | '|' | '/' | '\\' | '=' | '*'
        | '→' | '←' | '↑' | '↓'
    )
}

/// Quick heuristic: does this text region likely contain ASCII art?
///
/// Returns `true` when:
/// - There are at least 2 lines
/// - More than 15% of characters are diagram characters
pub fn is_likely_diagram(lines: &[&str]) -> bool {
    if lines.len() < 2 {
        return false;
    }
    let total: usize = lines.iter().map(|l| l.chars().count()).sum();
    if total == 0 {
        return false;
    }
    let diagram_chars: usize = lines
        .iter()
        .flat_map(|l| l.chars())
        .filter(|&c| is_diagram_char(c))
        .count();
    diagram_chars as f32 / total as f32 > 0.15
}

/// Direction components a character connects to, relative to its cell center.
#[derive(Default)]
struct Connectivity {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

/// Determine which directions a character connects to.
fn connectivity(c: char, col: usize, row: usize, grid: &[Vec<char>]) -> Connectivity {
    match c {
        // ─ horizontal
        '─' | '-' | '═' | '=' => Connectivity {
            left: true,
            right: true,
            ..Default::default()
        },
        // │ vertical
        '│' | '|' | '║' => Connectivity {
            up: true,
            down: true,
            ..Default::default()
        },
        // Corners
        '┌' => Connectivity {
            right: true,
            down: true,
            ..Default::default()
        },
        '┐' => Connectivity {
            left: true,
            down: true,
            ..Default::default()
        },
        '└' => Connectivity {
            right: true,
            up: true,
            ..Default::default()
        },
        '┘' => Connectivity {
            left: true,
            up: true,
            ..Default::default()
        },
        // T-junctions
        '├' => Connectivity {
            up: true,
            down: true,
            right: true,
            ..Default::default()
        },
        '┤' => Connectivity {
            up: true,
            down: true,
            left: true,
            ..Default::default()
        },
        '┬' => Connectivity {
            left: true,
            right: true,
            down: true,
            ..Default::default()
        },
        '┴' => Connectivity {
            left: true,
            right: true,
            up: true,
            ..Default::default()
        },
        // Cross
        '┼' => Connectivity {
            up: true,
            down: true,
            left: true,
            right: true,
        },
        // Double-line corners
        '╔' => Connectivity {
            right: true,
            down: true,
            ..Default::default()
        },
        '╗' => Connectivity {
            left: true,
            down: true,
            ..Default::default()
        },
        '╚' => Connectivity {
            right: true,
            up: true,
            ..Default::default()
        },
        '╝' => Connectivity {
            left: true,
            up: true,
            ..Default::default()
        },
        // Double-line T-junctions
        '╠' => Connectivity {
            up: true,
            down: true,
            right: true,
            ..Default::default()
        },
        '╣' => Connectivity {
            up: true,
            down: true,
            left: true,
            ..Default::default()
        },
        '╦' => Connectivity {
            left: true,
            right: true,
            down: true,
            ..Default::default()
        },
        '╩' => Connectivity {
            left: true,
            right: true,
            up: true,
            ..Default::default()
        },
        '╬' => Connectivity {
            up: true,
            down: true,
            left: true,
            right: true,
        },
        // Arrows
        '→' | '>' => Connectivity {
            left: true,
            right: true,
            ..Default::default()
        },
        '←' | '<' => Connectivity {
            left: true,
            right: true,
            ..Default::default()
        },
        '↑' | '^' => Connectivity {
            up: true,
            down: true,
            ..Default::default()
        },
        '↓' | 'v' => Connectivity {
            up: true,
            down: true,
            ..Default::default()
        },
        // Diagonals
        '/' => Connectivity {
            // bottom-left to top-right
            up: true,
            down: true,
            left: true,
            right: true,
        },
        '\\' => Connectivity {
            // top-left to bottom-right
            up: true,
            down: true,
            left: true,
            right: true,
        },
        // Asterisk/star — connects all directions
        '*' => Connectivity {
            up: true,
            down: true,
            left: true,
            right: true,
        },
        // `+` — infer from neighbors
        '+' => {
            let has_up =
                row > 0 && grid[row - 1].get(col).is_some_and(|&c| is_diagram_char(c));
            let has_down = row + 1 < grid.len()
                && grid[row + 1].get(col).is_some_and(|&c| is_diagram_char(c));
            let has_left =
                col > 0 && grid[row].get(col - 1).is_some_and(|&c| is_diagram_char(c));
            let has_right = grid[row]
                .get(col + 1)
                .is_some_and(|&c| is_diagram_char(c));
            Connectivity {
                up: has_up,
                down: has_down,
                left: has_left,
                right: has_right,
            }
        }
        _ => Connectivity::default(),
    }
}

/// Emit SVG path segments for a single diagram character at the given cell.
fn emit_path_segments(
    c: char,
    col: usize,
    row: usize,
    grid: &[Vec<char>],
    config: &DiagramConfig,
    path: &mut String,
) {
    let cw = config.cell_width;
    let ch = config.cell_height;
    let x0 = col as f32 * cw; // cell left
    let y0 = row as f32 * ch; // cell top
    let cx = x0 + cw / 2.0; // cell center x
    let cy = y0 + ch / 2.0; // cell center y

    // Handle diagonals specially — they don't go through cell center
    match c {
        '/' => {
            let _ = write!(path, "M{},{} L{},{} ", x0 + cw, y0, x0, y0 + ch);
            return;
        }
        '\\' => {
            let _ = write!(path, "M{},{} L{},{} ", x0, y0, x0 + cw, y0 + ch);
            return;
        }
        _ => {}
    }

    // For arrow heads, draw the arrowhead marker
    match c {
        '→' | '>' => {
            // Horizontal line + right-pointing arrowhead
            let _ = write!(path, "M{},{} L{},{} ", x0, cy, x0 + cw, cy);
            let _ = write!(
                path,
                "M{},{} L{},{} L{},{} ",
                cx,
                cy - ch * 0.2,
                x0 + cw,
                cy,
                cx,
                cy + ch * 0.2
            );
            return;
        }
        '←' | '<' => {
            let _ = write!(path, "M{},{} L{},{} ", x0, cy, x0 + cw, cy);
            let _ = write!(
                path,
                "M{},{} L{},{} L{},{} ",
                cx,
                cy - ch * 0.2,
                x0,
                cy,
                cx,
                cy + ch * 0.2
            );
            return;
        }
        '↑' | '^' => {
            let _ = write!(path, "M{},{} L{},{} ", cx, y0, cx, y0 + ch);
            let _ = write!(
                path,
                "M{},{} L{},{} L{},{} ",
                cx - cw * 0.3,
                cy,
                cx,
                y0,
                cx + cw * 0.3,
                cy
            );
            return;
        }
        '↓' => {
            let _ = write!(path, "M{},{} L{},{} ", cx, y0, cx, y0 + ch);
            let _ = write!(
                path,
                "M{},{} L{},{} L{},{} ",
                cx - cw * 0.3,
                cy,
                cx,
                y0 + ch,
                cx + cw * 0.3,
                cy
            );
            return;
        }
        _ => {}
    }

    // Double lines
    let is_double = matches!(c, '═' | '=' | '║' | '╔' | '╗' | '╚' | '╝' | '╠' | '╣' | '╦' | '╩' | '╬');
    let offset = if is_double { ch * 0.1 } else { 0.0 };

    let conn = connectivity(c, col, row, grid);

    // For double lines, draw two parallel lines per direction
    if is_double {
        if conn.left {
            let _ = write!(path, "M{},{} L{},{} ", x0, cy - offset, cx, cy - offset);
            let _ = write!(path, "M{},{} L{},{} ", x0, cy + offset, cx, cy + offset);
        }
        if conn.right {
            let _ = write!(
                path,
                "M{},{} L{},{} ",
                cx,
                cy - offset,
                x0 + cw,
                cy - offset
            );
            let _ = write!(
                path,
                "M{},{} L{},{} ",
                cx,
                cy + offset,
                x0 + cw,
                cy + offset
            );
        }
        if conn.up {
            let _ = write!(path, "M{},{} L{},{} ", cx - offset, y0, cx - offset, cy);
            let _ = write!(path, "M{},{} L{},{} ", cx + offset, y0, cx + offset, cy);
        }
        if conn.down {
            let _ = write!(
                path,
                "M{},{} L{},{} ",
                cx - offset,
                cy,
                cx - offset,
                y0 + ch
            );
            let _ = write!(
                path,
                "M{},{} L{},{} ",
                cx + offset,
                cy,
                cx + offset,
                y0 + ch
            );
        }
    } else {
        // Single lines — draw from center to edge in each connected direction
        if conn.left {
            let _ = write!(path, "M{},{} L{},{} ", x0, cy, cx, cy);
        }
        if conn.right {
            let _ = write!(path, "M{},{} L{},{} ", cx, cy, x0 + cw, cy);
        }
        if conn.up {
            let _ = write!(path, "M{},{} L{},{} ", cx, y0, cx, cy);
        }
        if conn.down {
            let _ = write!(path, "M{},{} L{},{} ", cx, cy, cx, y0 + ch);
        }
    }
}

/// Detect and convert ASCII art in a text region to SVG.
///
/// Returns `None` if the input is empty or contains no diagram characters.
pub fn ascii_to_svg(lines: &[&str], config: &DiagramConfig) -> Option<DiagramSvg> {
    if lines.is_empty() {
        return None;
    }

    // Build character grid
    let grid: Vec<Vec<char>> = lines.iter().map(|l| l.chars().collect()).collect();
    let max_cols = grid.iter().map(|row| row.len()).max().unwrap_or(0);
    let num_rows = grid.len();

    if max_cols == 0 {
        return None;
    }

    let svg_width = max_cols as f32 * config.cell_width;
    let svg_height = num_rows as f32 * config.cell_height;

    let mut path_data = String::new();
    let mut text_elements = String::new();
    let mut char_count = 0usize;

    // Track runs of non-diagram text for <text> elements
    for (row_idx, row_chars) in grid.iter().enumerate() {
        let mut text_run_start: Option<usize> = None;
        let mut text_buf = String::new();

        for (col_idx, &c) in row_chars.iter().enumerate() {
            if is_diagram_char(c) {
                // Flush any accumulated text run
                if let Some(start_col) = text_run_start.take() {
                    let trimmed = text_buf.trim();
                    if !trimmed.is_empty() {
                        // Find the actual start position (skip leading spaces)
                        let leading = text_buf.len() - text_buf.trim_start().len();
                        let text_x =
                            (start_col + leading) as f32 * config.cell_width + config.cell_width / 2.0;
                        let text_y =
                            row_idx as f32 * config.cell_height + config.cell_height * 0.7;
                        let _ = write!(
                            text_elements,
                            r#"<text x="{}" y="{}" fill="{}" font-family="monospace" font-size="{}">{}</text>"#,
                            text_x,
                            text_y,
                            config.text_color,
                            config.font_size,
                            xml_escape(trimmed),
                        );
                    }
                    text_buf.clear();
                }

                emit_path_segments(c, col_idx, row_idx, &grid, config, &mut path_data);
                char_count += 1;
            } else if c != ' ' {
                // Non-space, non-diagram character — accumulate text
                if text_run_start.is_none() {
                    text_run_start = Some(col_idx);
                }
                text_buf.push(c);
            } else {
                // Space character
                if text_run_start.is_some() {
                    text_buf.push(c);
                }
            }
        }

        // Flush remaining text at end of row
        if let Some(start_col) = text_run_start {
            let trimmed = text_buf.trim();
            if !trimmed.is_empty() {
                let leading = text_buf.len() - text_buf.trim_start().len();
                let text_x =
                    (start_col + leading) as f32 * config.cell_width + config.cell_width / 2.0;
                let text_y = row_idx as f32 * config.cell_height + config.cell_height * 0.7;
                let _ = write!(
                    text_elements,
                    r#"<text x="{}" y="{}" fill="{}" font-family="monospace" font-size="{}">{}</text>"#,
                    text_x,
                    text_y,
                    config.text_color,
                    config.font_size,
                    xml_escape(trimmed),
                );
            }
        }
    }

    if char_count == 0 {
        return None;
    }

    let mut svg = String::with_capacity(path_data.len() + text_elements.len() + 256);
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}" style="background:transparent">"#,
        svg_width, svg_height,
    );
    let _ = write!(
        svg,
        r#"<path d="{}" stroke="{}" stroke-width="{}" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#,
        path_data.trim(),
        config.stroke_color,
        config.stroke_width,
    );
    svg.push_str(&text_elements);
    svg.push_str("</svg>");

    Some(DiagramSvg {
        svg,
        width: svg_width,
        height: svg_height,
        char_count,
    })
}

/// Minimal XML escaping for text content.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_likely_diagram_simple_box() {
        let lines = vec!["┌───┐", "│   │", "└───┘"];
        assert!(is_likely_diagram(&lines));
    }

    #[test]
    fn test_is_likely_diagram_ascii_box() {
        let lines = vec!["+---+", "|   |", "+---+"];
        assert!(is_likely_diagram(&lines));
    }

    #[test]
    fn test_not_diagram_plain_text() {
        let lines = vec!["Hello world", "This is text"];
        assert!(!is_likely_diagram(&lines));
    }

    #[test]
    fn test_not_diagram_single_line() {
        let lines = vec!["┌───┐"];
        assert!(!is_likely_diagram(&lines));
    }

    #[test]
    fn test_not_diagram_empty() {
        let lines: Vec<&str> = vec![];
        assert!(!is_likely_diagram(&lines));
    }

    #[test]
    fn test_ascii_to_svg_simple_box() {
        let lines = vec!["┌─┐", "│ │", "└─┘"];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config);
        assert!(result.is_some());
        let svg = result.unwrap();
        assert!(svg.svg.contains("<svg"));
        assert!(svg.svg.contains("<path"));
        assert!(svg.svg.contains("</svg>"));
        assert!(svg.char_count > 0);
        assert_eq!(svg.char_count, 8); // 4 corners + 2 horizontal + 2 vertical
    }

    #[test]
    fn test_ascii_to_svg_with_text() {
        let lines = vec!["┌─────┐", "│Hello│", "└─────┘"];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config);
        assert!(result.is_some());
        let svg = result.unwrap();
        assert!(svg.svg.contains("Hello"));
        assert!(svg.svg.contains("<text"));
    }

    #[test]
    fn test_ascii_to_svg_with_xml_special_chars() {
        let lines = vec!["┌─────┐", "│A<B&C│", "└─────┘"];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config);
        assert!(result.is_some());
        let svg = result.unwrap();
        assert!(svg.svg.contains("A&lt;B&amp;C"));
    }

    #[test]
    fn test_empty_input() {
        let lines: Vec<&str> = vec![];
        assert!(ascii_to_svg(&lines, &DiagramConfig::default()).is_none());
    }

    #[test]
    fn test_no_diagram_chars_returns_none() {
        let lines = vec!["Hello", "World"];
        assert!(ascii_to_svg(&lines, &DiagramConfig::default()).is_none());
    }

    #[test]
    fn test_dimensions() {
        let lines = vec!["┌──┐", "└──┘"];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config).unwrap();
        // 4 columns * 10.0 = 40.0, 2 rows * 20.0 = 40.0
        assert_eq!(result.width, 40.0);
        assert_eq!(result.height, 40.0);
    }

    #[test]
    fn test_custom_config() {
        let lines = vec!["┌─┐", "└─┘"];
        let config = DiagramConfig {
            cell_width: 20.0,
            cell_height: 30.0,
            stroke_color: "#ff0000".to_string(),
            stroke_width: 3.0,
            text_color: "#ffffff".to_string(),
            font_size: 16.0,
        };
        let result = ascii_to_svg(&lines, &config).unwrap();
        assert!(svg_contains_attr(&result.svg, "stroke", "#ff0000"));
        assert!(svg_contains_attr(&result.svg, "stroke-width", "3"));
        assert_eq!(result.width, 60.0); // 3 cols * 20
        assert_eq!(result.height, 60.0); // 2 rows * 30
    }

    #[test]
    fn test_ascii_plus_box() {
        let lines = vec!["+--+", "|  |", "+--+"];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config);
        assert!(result.is_some());
        let svg = result.unwrap();
        assert!(svg.char_count > 0);
    }

    #[test]
    fn test_t_junctions() {
        let lines = vec!["┌─┬─┐", "│ │ │", "├─┼─┤", "│ │ │", "└─┴─┘"];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config);
        assert!(result.is_some());
        let svg = result.unwrap();
        assert!(svg.char_count > 0);
    }

    #[test]
    fn test_arrows() {
        let lines = vec!["→←", "↑↓"];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config);
        assert!(result.is_some());
        let svg = result.unwrap();
        assert_eq!(svg.char_count, 4);
    }

    #[test]
    fn test_double_line_box() {
        let lines = vec!["╔══╗", "║  ║", "╚══╝"];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config);
        assert!(result.is_some());
        let svg = result.unwrap();
        assert!(svg.char_count > 0);
    }

    #[test]
    fn test_diagonal_lines() {
        let lines = vec!["/\\", "\\/"];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config);
        assert!(result.is_some());
        let svg = result.unwrap();
        assert_eq!(svg.char_count, 4);
    }

    #[test]
    fn test_mixed_diagram_and_text() {
        // A labeled box
        let lines = vec![
            "  Input  ",
            "┌───────┐",
            "│ data  │",
            "└───┬───┘",
            "    │    ",
            "    ↓    ",
            "┌───────┐",
            "│output │",
            "└───────┘",
        ];
        let config = DiagramConfig::default();
        let result = ascii_to_svg(&lines, &config);
        assert!(result.is_some());
        let svg = result.unwrap();
        assert!(svg.svg.contains("Input"));
        assert!(svg.svg.contains("data"));
        assert!(svg.svg.contains("output"));
    }

    /// Helper to check SVG attribute presence.
    fn svg_contains_attr(svg: &str, attr: &str, value: &str) -> bool {
        svg.contains(&format!(r#"{}="{}""#, attr, value))
    }
}
