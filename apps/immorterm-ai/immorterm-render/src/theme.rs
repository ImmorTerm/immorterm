//! Terminal color theme — resolves Color variants to concrete RGBA values.
//!
//! Provides the standard 256-color terminal palette and a configurable theme
//! for default foreground/background colors.

use immorterm_core::cell::Color;

/// Terminal color theme with resolved default colors.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Default foreground color (when Cell fg is Color::Default)
    pub fg: [f32; 4],
    /// Default background color (when Cell bg is Color::Default)
    pub bg: [f32; 4],
    /// Cursor color
    pub cursor: [f32; 4],
    /// Selection highlight color (semi-transparent)
    pub selection: [f32; 4],
    /// Pseudo-cursor selection highlight color (accent-derived, semi-transparent)
    pub pseudo_selection: [f32; 4],
    /// Window border color (themed accent — frames the terminal content)
    pub border: [f32; 4],
    /// Override for the standard 16 ANSI colors (indices 0-15).
    /// When set, these replace the hardcoded Nord-inspired palette.
    /// Sourced from VS Code's `--vscode-terminal-ansi*` CSS variables.
    pub ansi_overrides: Option<[[f32; 4]; 16]>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            fg: [0.847, 0.871, 0.914, 1.0],       // #D8DEE9 (Nord Snow)
            bg: [0.0, 0.0, 0.0, 1.0],               // #000000 (Pure black — matches VS Code terminal)
            cursor: [0.816, 0.529, 0.439, 1.0],    // #D08770 (Nord Aurora Orange)
            selection: [0.263, 0.298, 0.369, 0.6], // #434C5E semi-transparent
            pseudo_selection: [0.193, 0.129, 0.289, 0.65], // Darkened purple accent for bg visibility
            border: [0.482, 0.322, 0.722, 1.0],   // #7B52B8 (status bar gradient end)
            ansi_overrides: None,
        }
    }
}

impl Theme {
    /// Resolve a terminal Color to concrete RGBA.
    pub fn resolve_fg(&self, color: &Color) -> [f32; 4] {
        match color {
            Color::Default => self.fg,
            Color::Indexed(idx) => self.resolve_indexed(*idx),
            Color::Rgb(r, g, b) => rgb_to_float(*r, *g, *b),
        }
    }

    /// Resolve a terminal Color to concrete RGBA for backgrounds.
    pub fn resolve_bg(&self, color: &Color) -> [f32; 4] {
        match color {
            Color::Default => self.bg,
            Color::Indexed(idx) => self.resolve_indexed(*idx),
            Color::Rgb(r, g, b) => rgb_to_float(*r, *g, *b),
        }
    }

    /// Resolve an indexed color, checking ANSI overrides for 0-15.
    fn resolve_indexed(&self, idx: u8) -> [f32; 4] {
        if idx < 16
            && let Some(ref overrides) = self.ansi_overrides {
                return overrides[idx as usize];
            }
        palette_color(idx)
    }
}

fn rgb_to_float(r: u8, g: u8, b: u8) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

/// Standard 256-color terminal palette.
fn palette_color(idx: u8) -> [f32; 4] {
    let (r, g, b) = match idx {
        // Standard 16 colors (using Nord-inspired palette)
        0 => (0x3B, 0x42, 0x52),   // Black
        1 => (0xBF, 0x61, 0x6A),   // Red
        2 => (0xA3, 0xBE, 0x8C),   // Green
        3 => (0xEB, 0xCB, 0x8B),   // Yellow
        4 => (0x81, 0xA1, 0xC1),   // Blue
        5 => (0xB4, 0x8E, 0xAD),   // Magenta
        6 => (0x88, 0xC0, 0xD0),   // Cyan
        7 => (0xE5, 0xE9, 0xF0),   // White
        8 => (0x4C, 0x56, 0x6A),   // Bright Black
        9 => (0xBF, 0x61, 0x6A),   // Bright Red
        10 => (0xA3, 0xBE, 0x8C),  // Bright Green
        11 => (0xEB, 0xCB, 0x8B),  // Bright Yellow
        12 => (0x81, 0xA1, 0xC1),  // Bright Blue
        13 => (0xB4, 0x8E, 0xAD),  // Bright Magenta
        14 => (0x8F, 0xBC, 0xBB),  // Bright Cyan
        15 => (0xEC, 0xEF, 0xF4),  // Bright White
        // 6x6x6 color cube (indices 16-231)
        16..=231 => {
            let idx = idx - 16;
            let b_val = idx % 6;
            let g_val = (idx / 6) % 6;
            let r_val = idx / 36;
            let to_byte = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (to_byte(r_val), to_byte(g_val), to_byte(b_val))
        }
        // Grayscale ramp (indices 232-255)
        232..=255 => {
            let level = 8 + 10 * (idx - 232);
            (level, level, level)
        }
    };
    rgb_to_float(r, g, b)
}
