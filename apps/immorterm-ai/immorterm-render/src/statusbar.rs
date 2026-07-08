//! Status bar data, gradient colors, and animation helpers.
//!
//! The status bar renders as one extra row below the terminal grid.
//! All animation math is CPU-computed per-frame and applied as color
//! modulation to BgInstance/GlyphInstance data — no new shaders needed.
//!
//! Matches the latest C ImmorTerm screenrc format:
//! `%{G#2D004D#7B52B8}%{= #FFFFFF}  %2`%{= #FFFFFF} /%{= #FFFFFF} %t %?%K%?%=%{= #FFA500}%?%Z%? %=%{= #FFFFFF} Last:%{= #FFFFFF} %I%{= #E0B0FF} %J%{S#E0B0FF} ImmorTerm  %{-}`

/// Identifies which clickable target the mouse is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusBarTarget {
    Brand,     // "ImmorTerm" text → session picker
    AiStats,   // AI stats text → toggle mode
    ThemeArea, // Dot + time area → theme picker
    Title,     // Session title → tooltip with last user prompt
    Project,   // Project name (bottom-left) → project navigation menu
    #[default]
    None, // Not over a clickable region
}

/// AI stats display mode, cycled by clicking the stats section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AiStatsMode {
    #[default]
    Full,    // "Opus 4 | $1.23 | 45%"
    Compact, // "$1.23"
    Hidden,  // (nothing)
}

impl AiStatsMode {
    pub fn next(self) -> Self {
        match self {
            Self::Full => Self::Compact,
            Self::Compact => Self::Hidden,
            Self::Hidden => Self::Full,
        }
    }
}

/// Compile-time hex digit → u8 value.
const fn hex_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

/// Compile-time hex string → RGB float triple. Input: b"#RRGGBB".
const fn rgb(h: &[u8; 7]) -> [f32; 3] {
    [
        (hex_val(h[1]) * 16 + hex_val(h[2])) as f32 / 255.0,
        (hex_val(h[3]) * 16 + hex_val(h[4])) as f32 / 255.0,
        (hex_val(h[5]) * 16 + hex_val(h[6])) as f32 / 255.0,
    ]
}

/// Compile-time hex string → RGBA float quad (alpha = 1.0). Input: b"#RRGGBB".
const fn rgba(h: &[u8; 7]) -> [f32; 4] {
    let c = rgb(h);
    [c[0], c[1], c[2], 1.0]
}

/// Theme colors for the status bar gradient (7-stop).
/// Matches themes.ts exactly via compile-time hex conversion.
#[derive(Debug, Clone, Copy)]
pub struct StatusBarTheme {
    pub name: &'static str,
    pub gradient_stops: [[f32; 3]; 7], // bg1..bg7 from themes.ts
    pub fg: [f32; 4],                  // Default text color
    pub fg_accent: [f32; 4],           // Brand + dot accent color
}

impl Default for StatusBarTheme {
    fn default() -> Self {
        THEME_PRESETS[0]
    }
}

/// All 21 theme presets — hex values match themes.ts exactly.
pub const THEME_PRESETS: &[StatusBarTheme] = &[
    StatusBarTheme {
        name: "Purple Haze",
        gradient_stops: [rgb(b"#2D004D"), rgb(b"#3D1A6D"), rgb(b"#4D2A7D"), rgb(b"#5B2C8A"), rgb(b"#6B3FA0"), rgb(b"#7B52B8"), rgb(b"#8B008B")],
        fg: rgba(b"#FFFFFF"),
        fg_accent: rgba(b"#E0B0FF"),
    },
    StatusBarTheme {
        name: "Ocean Depths",
        gradient_stops: [rgb(b"#001F3F"), rgb(b"#003366"), rgb(b"#00447A"), rgb(b"#004C8C"), rgb(b"#0066B3"), rgb(b"#0080D9"), rgb(b"#00A0E0")],
        fg: rgba(b"#FFFFFF"),
        fg_accent: rgba(b"#87CEEB"),
    },
    StatusBarTheme {
        name: "Aurora Borealis",
        gradient_stops: [rgb(b"#020B1A"), rgb(b"#051832"), rgb(b"#082E46"), rgb(b"#0C4550"), rgb(b"#105E54"), rgb(b"#1C8B62"), rgb(b"#2ECC71")],
        fg: rgba(b"#E0FFF4"),
        fg_accent: rgba(b"#7DFFC3"),
    },
    StatusBarTheme {
        name: "Sunset Glow",
        gradient_stops: [rgb(b"#4A1C1C"), rgb(b"#6B2D2D"), rgb(b"#7E3636"), rgb(b"#8C3E3E"), rgb(b"#AD4F4F"), rgb(b"#CE6060"), rgb(b"#E07020")],
        fg: rgba(b"#FFFFFF"),
        fg_accent: rgba(b"#FFD700"),
    },
    StatusBarTheme {
        name: "Solar Flare",
        gradient_stops: [rgb(b"#1A0800"), rgb(b"#331400"), rgb(b"#4D2200"), rgb(b"#6B3500"), rgb(b"#884C00"), rgb(b"#B86B00"), rgb(b"#FF8C00")],
        fg: rgba(b"#FFFFFF"),
        fg_accent: rgba(b"#FFD700"),
    },
    StatusBarTheme {
        name: "Glacier",
        gradient_stops: [rgb(b"#06111C"), rgb(b"#0E2240"), rgb(b"#163560"), rgb(b"#224D7E"), rgb(b"#35689A"), rgb(b"#4A8AB5"), rgb(b"#7EC8E3")],
        fg: rgba(b"#F0F8FF"),
        fg_accent: rgba(b"#B4F0FF"),
    },
    StatusBarTheme {
        name: "Rose Gold",
        gradient_stops: [rgb(b"#3D1F2B"), rgb(b"#5C2E41"), rgb(b"#6C364C"), rgb(b"#7B3D57"), rgb(b"#9A4C6D"), rgb(b"#B95B83"), rgb(b"#D86A99")],
        fg: rgba(b"#FFFFFF"),
        fg_accent: rgba(b"#FFB6C1"),
    },
    StatusBarTheme {
        name: "Cyberpunk",
        gradient_stops: [rgb(b"#0D0221"), rgb(b"#1A0A3E"), rgb(b"#240E4C"), rgb(b"#2D1259"), rgb(b"#541388"), rgb(b"#7B1FA2"), rgb(b"#FF006E")],
        fg: rgba(b"#00FFFF"),
        fg_accent: rgba(b"#FF00FF"),
    },
    StatusBarTheme {
        name: "Monochrome Dark",
        gradient_stops: [rgb(b"#000000"), rgb(b"#1A1A1A"), rgb(b"#2D2D2D"), rgb(b"#404040"), rgb(b"#555555"), rgb(b"#6A6A6A"), rgb(b"#808080")],
        fg: rgba(b"#FFFFFF"),
        fg_accent: rgba(b"#CCCCCC"),
    },
    StatusBarTheme {
        name: "Monochrome Light",
        gradient_stops: [rgb(b"#FFFFFF"), rgb(b"#F0F0F0"), rgb(b"#E0E0E0"), rgb(b"#D0D0D0"), rgb(b"#C0C0C0"), rgb(b"#B0B0B0"), rgb(b"#A0A0A0")],
        fg: rgba(b"#000000"),
        fg_accent: rgba(b"#333333"),
    },
    StatusBarTheme {
        name: "Neon Tokyo",
        gradient_stops: [rgb(b"#080011"), rgb(b"#140828"), rgb(b"#281044"), rgb(b"#421860"), rgb(b"#661E74"), rgb(b"#AA1E6E"), rgb(b"#FF2975")],
        fg: rgba(b"#00FFE5"),
        fg_accent: rgba(b"#FFE54C"),
    },
    StatusBarTheme {
        name: "Dracula",
        gradient_stops: [rgb(b"#21222C"), rgb(b"#282A36"), rgb(b"#2E303E"), rgb(b"#343746"), rgb(b"#44475A"), rgb(b"#6272A4"), rgb(b"#BD93F9")],
        fg: rgba(b"#F8F8F2"),
        fg_accent: rgba(b"#FF79C6"),
    },
    StatusBarTheme {
        name: "Matrix",
        gradient_stops: [rgb(b"#000000"), rgb(b"#001500"), rgb(b"#002A00"), rgb(b"#004200"), rgb(b"#005E00"), rgb(b"#008B00"), rgb(b"#00FF41")],
        fg: rgba(b"#00FF41"),
        fg_accent: rgba(b"#33FF77"),
    },
    StatusBarTheme {
        name: "Vaporwave",
        gradient_stops: [rgb(b"#0A1520"), rgb(b"#1A1E30"), rgb(b"#2E2440"), rgb(b"#442A52"), rgb(b"#603066"), rgb(b"#983878"), rgb(b"#FF71CE")],
        fg: rgba(b"#00FFD4"),
        fg_accent: rgba(b"#01CDFE"),
    },
    StatusBarTheme {
        name: "Ember",
        gradient_stops: [rgb(b"#1A0A00"), rgb(b"#2E1400"), rgb(b"#441E08"), rgb(b"#5C2A10"), rgb(b"#763818"), rgb(b"#994520"), rgb(b"#D4760A")],
        fg: rgba(b"#FFF5E6"),
        fg_accent: rgba(b"#FFB84D"),
    },
    StatusBarTheme {
        name: "Electric Lime",
        gradient_stops: [rgb(b"#050A00"), rgb(b"#101C05"), rgb(b"#1E3008"), rgb(b"#2E4610"), rgb(b"#406018"), rgb(b"#588020"), rgb(b"#84CC16")],
        fg: rgba(b"#F0FFF0"),
        fg_accent: rgba(b"#BEFF5A"),
    },
    StatusBarTheme {
        name: "Tidal",
        gradient_stops: [rgb(b"#020A18"), rgb(b"#061530"), rgb(b"#0A2848"), rgb(b"#104060"), rgb(b"#186078"), rgb(b"#208890"), rgb(b"#2DD4BF")],
        fg: rgba(b"#E0FFFF"),
        fg_accent: rgba(b"#48D1CC"),
    },
    StatusBarTheme {
        name: "Amber",
        gradient_stops: [rgb(b"#0E0A00"), rgb(b"#1E1800"), rgb(b"#302800"), rgb(b"#483C00"), rgb(b"#665200"), rgb(b"#887000"), rgb(b"#BFA200")],
        fg: rgba(b"#FFFDD0"),
        fg_accent: rgba(b"#FFD700"),
    },
    StatusBarTheme {
        name: "Synthwave",
        gradient_stops: [rgb(b"#1A1A2E"), rgb(b"#262640"), rgb(b"#2C2C49"), rgb(b"#323252"), rgb(b"#4A3F6B"), rgb(b"#614385"), rgb(b"#FF2E97")],
        fg: rgba(b"#FFFFFF"),
        fg_accent: rgba(b"#00F3FF"),
    },
    StatusBarTheme {
        name: "Molten",
        gradient_stops: [rgb(b"#100004"), rgb(b"#200810"), rgb(b"#381018"), rgb(b"#501820"), rgb(b"#702020"), rgb(b"#983028"), rgb(b"#b83020")],
        fg: rgba(b"#FFD8C8"),
        fg_accent: rgba(b"#FF6840"),
    },
    StatusBarTheme {
        name: "Rainbow",
        gradient_stops: [rgb(b"#8B0000"), rgb(b"#8B4500"), rgb(b"#6B6B00"), rgb(b"#006400"), rgb(b"#00008B"), rgb(b"#4B0082"), rgb(b"#800080")],
        fg: rgba(b"#FFFFFF"),
        fg_accent: rgba(b"#FFD700"),
    },
    StatusBarTheme {
        name: "Delulus Club",
        gradient_stops: [rgb(b"#2D1864"), rgb(b"#4A26A8"), rgb(b"#6B3FD6"), rgb(b"#3BC43A"), rgb(b"#3BC43A"), rgb(b"#3BC43A"), rgb(b"#3BC43A")],
        fg: rgba(b"#FBF7F0"),
        fg_accent: rgba(b"#F4C21E"),
    },
    StatusBarTheme {
        name: "Hot Pink",
        gradient_stops: [rgb(b"#14000A"), rgb(b"#260012"), rgb(b"#3D001E"), rgb(b"#57002B"), rgb(b"#7A003C"), rgb(b"#AD0052"), rgb(b"#E0218A")],
        fg: rgba(b"#FFD9EC"),
        fg_accent: rgba(b"#FF4FA3"),
    },
];

/// A single section of the status bar with its own FG color.
/// Background comes from the smooth gradient across all columns.
#[derive(Debug, Clone)]
pub struct StatusBarSection {
    pub text: String,
    pub fg: [f32; 4],
}

/// Status bar data built by the platform layer and consumed by the renderer.
/// Left sections left-to-right, center sections centered, right sections right-aligned.
/// Background is a smooth gradient from GRADIENT_START to GRADIENT_END.
#[derive(Debug, Clone, Default)]
pub struct StatusBarData {
    /// Left-aligned sections: project, separator, title
    pub left_sections: Vec<StatusBarSection>,
    /// Center-aligned sections: AI stats (fixed-width allocation)
    pub center_sections: Vec<StatusBarSection>,
    /// Right-aligned sections: "Last:", time, dot, brand
    pub right_sections: Vec<StatusBarSection>,
    /// Terminal width in columns
    pub cols: usize,
    /// Brand text start column (for shimmer targeting)
    pub brand_start_col: usize,
    /// Brand text end column (exclusive)
    pub brand_end_col: usize,
    /// AI stats section start column (center zone)
    pub ai_stats_start_col: usize,
    /// AI stats section end column (exclusive, center zone)
    pub ai_stats_end_col: usize,
    /// Theme area start column (time + dot region)
    pub theme_area_start_col: usize,
    /// Theme area end column (exclusive)
    pub theme_area_end_col: usize,
    /// Project name start column (for project menu)
    pub project_start_col: usize,
    /// Project name end column (exclusive)
    pub project_end_col: usize,
    /// Title section start column (for hover tooltip)
    pub title_start_col: usize,
    /// Title section end column (exclusive)
    pub title_end_col: usize,
    /// CTX bar fill percentage (0.0 = no bar, >0 = show colored fill)
    pub ctx_pct: f32,
    /// 7-stop gradient colors (from active theme, bg1..bg7)
    pub gradient_stops: [[f32; 3]; 7],
    /// Accent color (from active theme fg_accent)
    pub accent: [f32; 4],
    /// Which section is currently hovered (set by platform layer)
    pub hovered_target: StatusBarTarget,
    /// Full untruncated title (for hover tooltip expansion)
    pub full_title: String,
    /// Whether the title was truncated (marquee needed)
    pub title_truncated: bool,
    /// Sub-pixel horizontal shift for smooth title scrolling (0.0–1.0 in cell-widths).
    /// The renderer shifts all title glyphs left by this amount.
    pub title_pixel_shift: f32,
}

/// White foreground color.
pub const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Accent foreground (light purple #E0B0FF) — brand + animated dot.
pub const ACCENT: [f32; 4] = [0.878, 0.690, 1.0, 1.0];
/// Orange foreground (#FFA500) — AI stats.
pub const ORANGE: [f32; 4] = [1.0, 0.647, 0.0, 1.0];

/// Gradient start color (#2D004D) — darkest purple.
pub const GRADIENT_START: [f32; 3] = [0.176, 0.000, 0.302];
/// Gradient end color (#7B52B8) — lighter violet.
pub const GRADIENT_END: [f32; 3] = [0.482, 0.322, 0.722];

/// Duration of one complete animation cycle in milliseconds.
pub const SHIMMER_CYCLE_MS: f32 = 15000.0;
/// Gradient super-cycle: slow wave that shifts the gradient origin.
pub const GRADIENT_SUPERCYCLE_MS: f32 = 120000.0;
/// First 60s of the super-cycle: gradient is static (no wave).
pub const GRADIENT_STATIC_MS: f32 = 60000.0;
/// Number of wave oscillations in the active phase (60-120s).
pub const GRADIENT_WAVES: f32 = 2.0;
/// Max stops of shift per oscillation.
pub const GRADIENT_WAVE_SHIFT: f32 = 2.0;
/// Breathing dim factor (minimum brightness as fraction of normal).
pub const BREATHING_DIM: f32 = 0.70;
/// Shimmer spotlight radius in columns.
pub const SHIMMER_RADIUS: f32 = 3.0;

/// Animated dot character sequence (bloom animation).
/// Exact match of C version: · • ✢ ✳ ✶ ✻ ✽ ✻ ✶ ✳ ✢ •
pub const DOT_FRAMES: &[char] = &[
    '\u{00B7}', // ·
    '\u{2022}', // •
    '\u{2722}', // ✢
    '\u{2733}', // ✳
    '\u{2736}', // ✶
    '\u{273B}', // ✻
    '\u{273D}', // ✽
    '\u{273B}', // ✻
    '\u{2736}', // ✶
    '\u{2733}', // ✳
    '\u{2722}', // ✢
    '\u{2022}', // •
];

/// Fixed width for the center AI stats zone (columns).
/// Sized for the widest vendor-prefixed stats ("Claude RAM:482M CPU:0% 40m49s" = 29 chars + 1 padding).
/// Fixed so the title budget stays stable when stats toggle between modes.
const CENTER_ZONE_WIDTH: usize = 30;

/// Build the standard ImmorTerm status bar layout (default theme).
///
/// Backwards-compatible wrapper around [`build_sections_with_theme`].
pub fn build_default_sections(
    project: &str,
    title: &str,
    ai_stats: &str,
    last_active: &str,
    dot: char,
    cols: usize,
    ctx_pct: f32,
) -> StatusBarData {
    build_sections_with_theme(project, title, ai_stats, last_active, dot, cols, ctx_pct, &StatusBarTheme::default(), 0, 0.0)
}

/// Marquee scroll speed: characters per second.
const MARQUEE_CHARS_PER_SEC: f32 = 3.0;
/// Pause duration at the start of the marquee (seconds).
const MARQUEE_START_PAUSE_SECS: f32 = 30.0;
/// Pause duration at the end of the marquee (seconds).
const MARQUEE_END_PAUSE_SECS: f32 = 2.0;
/// Minimum gap between left sections and center/right zones (columns).
const TITLE_RIGHT_GAP: usize = 2;

/// Marquee scroll result: integer char offset + fractional sub-char shift.
/// The fractional part (0.0–1.0) is used for smooth sub-pixel scrolling.
pub struct MarqueeState {
    /// Integer character offset into the title.
    pub char_offset: usize,
    /// Fractional character shift (0.0–1.0) for smooth sub-pixel scrolling.
    /// Applied as a leftward pixel shift: `shift * cell_width`.
    pub fract: f32,
}

/// Compute the marquee scroll state from elapsed time.
/// Returns integer char offset + fractional shift for smooth sub-pixel scrolling.
/// The cycle: long pause at start → smooth scroll right → pause at end → smooth scroll left → repeat.
pub fn marquee_offset(time_secs: f32, overflow_chars: usize) -> MarqueeState {
    if overflow_chars == 0 {
        return MarqueeState { char_offset: 0, fract: 0.0 };
    }
    let scroll_duration = overflow_chars as f32 / MARQUEE_CHARS_PER_SEC;
    // Full cycle: start pause → scroll right → end pause → scroll left
    let cycle = MARQUEE_START_PAUSE_SECS + scroll_duration + MARQUEE_END_PAUSE_SECS + scroll_duration;
    let t = time_secs % cycle;

    let raw = if t < MARQUEE_START_PAUSE_SECS {
        // Pause at start
        0.0
    } else if t < MARQUEE_START_PAUSE_SECS + scroll_duration {
        // Scroll right (smooth)
        let progress = (t - MARQUEE_START_PAUSE_SECS) / scroll_duration;
        progress * overflow_chars as f32
    } else if t < MARQUEE_START_PAUSE_SECS + scroll_duration + MARQUEE_END_PAUSE_SECS {
        // Pause at end
        overflow_chars as f32
    } else {
        // Scroll left (smooth, back to start)
        let progress = (t - MARQUEE_START_PAUSE_SECS - scroll_duration - MARQUEE_END_PAUSE_SECS) / scroll_duration;
        (1.0 - progress) * overflow_chars as f32
    };

    let clamped = raw.clamp(0.0, overflow_chars as f32);
    MarqueeState {
        char_offset: clamped as usize,
        fract: clamped.fract(),
    }
}

/// Build the status bar layout with a custom theme.
///
/// Layout: `  project / title  ··  [  AI stats  ]  ··  Last: DD/MM HH:MM [dot] ImmorTerm  `
///
/// AI stats are **centered** (like the C binary's `%=...%=` format).
/// When `ctx_pct > 0`, the renderer draws a colored fill behind the center section.
///
/// - project, "/", title, "Last:", time: white FG
/// - AI stats: orange FG (#FFA500), centered in a fixed-width zone
/// - animated dot, "ImmorTerm" brand: theme accent FG with shimmer on brand
///
/// `scroll_offset` drives the LED-sign marquee for long titles (use [`marquee_offset`]).
/// Pass 0 for no scrolling. When the title is hovered, the caller should pass
/// `usize::MAX` to show the full title (suppressing center sections to make room).
/// `scroll_fract` is the sub-pixel fraction (0.0–1.0) for smooth scrolling.
#[allow(clippy::too_many_arguments)]
pub fn build_sections_with_theme(
    project: &str,
    title: &str,
    ai_stats: &str,
    last_active: &str,
    dot: char,
    cols: usize,
    ctx_pct: f32,
    theme: &StatusBarTheme,
    scroll_offset: usize,
    scroll_fract: f32,
) -> StatusBarData {
    let title_hovered = scroll_offset == usize::MAX;

    // ── Pre-compute right total width (needed for title budget) ──
    let right_text_last = format!(" Last: {} ", last_active);
    let right_text_dot = format!(" {} ", dot);
    let right_text_brand = " ImmorTerm ";
    let right_total_chars = right_text_last.chars().count()
        + right_text_dot.chars().count()
        + right_text_brand.chars().count();

    // ── Pre-compute center zone width ──
    // Fixed-width center zone (capped at 25% of terminal width) so the title
    // budget stays stable when AI stats toggle between modes (RAM/CPU vs CTX bar).
    let has_ai = !ai_stats.is_empty();
    let center_zone = if has_ai && !title_hovered {
        CENTER_ZONE_WIDTH.min(cols / 4)
    } else {
        0 // suppress center when title is hovered (or no stats)
    };

    // ── Compute title budget ──
    // Title must end before the center zone starts (or right sections if no center).
    // The center zone is *centered* in the terminal, so we compute max_title_width
    // from the center zone's actual start column, not from total column arithmetic.
    let prefix_text = format!("  {} ", project);
    let separator_text = "/ ";
    let prefix_width = prefix_text.chars().count() + separator_text.chars().count();
    let title_end_limit = if center_zone > 0 {
        // Center zone starts at this column
        cols.saturating_sub(center_zone) / 2
    } else {
        // No center zone — title must end before right sections
        cols.saturating_sub(right_total_chars)
    };
    let max_title_width = title_end_limit
        .saturating_sub(prefix_width)
        .saturating_sub(TITLE_RIGHT_GAP);

    let full_title_text = format!("{} ", title);
    let title_char_count = full_title_text.chars().count();
    let title_truncated = title_char_count > max_title_width && max_title_width > 2;

    // ── Build display title (truncated + marquee window) ──
    // For smooth scrolling, include one extra char beyond the visible window
    // so the partially-scrolled char on the right edge is rendered.
    // The renderer clips via the sub-pixel shift.
    let (display_title, title_pixel_shift) = if title_hovered || !title_truncated {
        // Hover: show full title; or title fits naturally — no shift
        (full_title_text.clone(), 0.0)
    } else if max_title_width <= 2 {
        ("\u{2026}".to_string(), 0.0)
    } else {
        // Marquee: extract a sliding window of max_title_width + 1 chars
        // (the extra char is the one partially scrolling in from the right).
        let chars: Vec<char> = full_title_text.chars().collect();
        let overflow = title_char_count.saturating_sub(max_title_width);
        let offset = scroll_offset.min(overflow);
        let at_start = offset == 0 && scroll_fract < 0.001;
        // Include one extra char for the partial scroll (if available)
        let extra = if scroll_fract > 0.001 && offset + max_title_width < chars.len() { 1 } else { 0 };
        let end = (offset + max_title_width + extra).min(chars.len());
        let mut result: String = if offset > 0 {
            // Prepend a space so the "/" separator keeps visual breathing room during scroll
            format!(" {}", chars[offset..end].iter().collect::<String>())
        } else {
            chars[offset..end].iter().collect()
        };
        // Show ellipsis at the end only while paused at the start (before animation begins)
        if at_start && result.chars().count() >= max_title_width {
            let mut chars_iter = result.chars();
            let mut truncated = String::new();
            for _ in 0..max_title_width.saturating_sub(1) {
                if let Some(c) = chars_iter.next() {
                    truncated.push(c);
                }
            }
            truncated.push('\u{2026}');
            result = truncated;
        }
        (result, scroll_fract)
    };

    // ── Left sections ──
    let left_sections = vec![
        StatusBarSection {
            text: prefix_text,
            fg: theme.fg,
        },
        StatusBarSection {
            text: separator_text.to_string(),
            fg: theme.fg,
        },
        StatusBarSection {
            text: display_title,
            fg: theme.fg,
        },
    ];

    // ── Center sections (AI stats — centered in fixed-width zone) ──
    // center_zone already computed above (0 when title_hovered to give title full width)
    let mut center_sections = Vec::new();

    let (ai_stats_start_col, ai_stats_end_col) = if has_ai && center_zone > 0 {
        let center_start = (cols.saturating_sub(center_zone)) / 2;

        if ctx_pct > 0.0 {
            // CTX mode: ▰▱ bar matching the C binary exactly.
            // Format: "CTX: ▰▰▰▰▱▱▱▱▱▱ 42%"
            const BAR_WIDTH: usize = 10;
            let pct_int = ctx_pct.round() as usize;
            let filled = (pct_int * BAR_WIDTH + 50) / 100; // round
            let empty = BAR_WIDTH - filled;
            let pct_str = format!(" {}%", pct_int);
            let label = "CTX: ";

            // Total display width: "CTX: " + bar + " NN%"
            let total = label.len() + BAR_WIDTH + pct_str.len();
            let left_pad = if total < center_zone { (center_zone - total) / 2 } else { 0 };
            let right_pad = center_zone.saturating_sub(total + left_pad);

            let fill_color = ctx_fill_color(ctx_pct);

            // Left padding
            if left_pad > 0 {
                center_sections.push(StatusBarSection {
                    text: " ".repeat(left_pad),
                    fg: [0.0; 4],
                });
            }
            // "CTX: " label in white
            center_sections.push(StatusBarSection {
                text: label.to_string(),
                fg: theme.fg,
            });
            // Filled blocks ▰ in tier color
            if filled > 0 {
                center_sections.push(StatusBarSection {
                    text: "\u{25B0}".repeat(filled), // ▰
                    fg: fill_color,
                });
            }
            // Empty blocks ▱ in gray
            if empty > 0 {
                center_sections.push(StatusBarSection {
                    text: "\u{25B1}".repeat(empty), // ▱
                    fg: CTX_EMPTY_COLOR,
                });
            }
            // " NN%" in white
            center_sections.push(StatusBarSection {
                text: pct_str,
                fg: theme.fg,
            });
            // Right padding
            if right_pad > 0 {
                center_sections.push(StatusBarSection {
                    text: " ".repeat(right_pad),
                    fg: [0.0; 4],
                });
            }
        } else {
            // Process mode: pad and center the stats text
            let text_len = ai_stats.chars().count();
            let padded = if text_len < center_zone {
                let left_pad = (center_zone - text_len) / 2;
                let right_pad = center_zone - text_len - left_pad;
                format!("{:>w1$}{}{:>w2$}", "", ai_stats, "", w1 = left_pad, w2 = right_pad)
            } else {
                ai_stats.to_string()
            };
            center_sections.push(StatusBarSection {
                text: padded,
                fg: ORANGE,
            });
        }
        (center_start, center_start + center_zone)
    } else {
        (0, 0)
    };

    // ── Right sections (no AI stats — they moved to center) ──
    let mut right_sections = Vec::new();

    // "Last:" label + time value (theme fg)
    right_sections.push(StatusBarSection {
        text: format!(" Last: {} ", last_active),
        fg: theme.fg,
    });

    // Animated dot (theme accent color, centered between time and brand)
    right_sections.push(StatusBarSection {
        text: format!(" {} ", dot),
        fg: theme.fg_accent,
    });

    // Brand "ImmorTerm" (theme accent color + shimmer)
    let brand = " ImmorTerm ";
    right_sections.push(StatusBarSection {
        text: brand.to_string(),
        fg: theme.fg_accent,
    });

    // ── Compute section column positions for hit-testing ──
    let right_total: usize = right_sections.iter().map(|s| s.text.chars().count()).sum();
    let right_start = cols.saturating_sub(right_total);
    let n = right_sections.len();

    let mut cursor = right_start;
    let mut starts = Vec::with_capacity(n);
    for sec in &right_sections {
        starts.push(cursor);
        cursor += sec.text.chars().count();
    }

    // Theme area = Last/time only (dot is independent, not part of any hover zone)
    let theme_area_start_col = starts[0];
    let theme_area_end_col = if n > 1 { starts[1] } else { 0 };
    let brand_start_col = starts.last().copied().unwrap_or(0);

    // Project column range: left_sections[0] (project name, starts at col 0)
    let project_end_col = left_sections.first().map_or(0, |s| s.text.chars().count());

    // Title column range: left_sections[2] (after project + separator)
    let title_start_col: usize = left_sections.iter().take(2).map(|s| s.text.chars().count()).sum();
    let title_end_col = title_start_col + left_sections.get(2).map_or(0, |s| s.text.chars().count());

    StatusBarData {
        left_sections,
        center_sections,
        right_sections,
        cols,
        brand_start_col,
        brand_end_col: cols,
        ai_stats_start_col,
        ai_stats_end_col,
        ctx_pct: if has_ai { ctx_pct } else { 0.0 },
        theme_area_start_col,
        theme_area_end_col,
        project_start_col: 0,
        project_end_col,
        title_start_col,
        title_end_col,
        gradient_stops: theme.gradient_stops,
        accent: theme.fg_accent,
        hovered_target: StatusBarTarget::None,
        full_title: full_title_text,
        title_truncated,
        title_pixel_shift,
    }
}

/// Interpolate between custom gradient start and end colors.
/// Matches the C code: palindrome reflection so monotonic gradients
/// (dark→bright) never hit a jarring color cliff at the wrap point.
pub fn gradient_color_with(t: f32, wave_offset: f32, start: [f32; 3], end: [f32; 3]) -> [f32; 4] {
    // C code: t += offset * GRADIENT_WAVE_SHIFT (2.0)
    let t = t + wave_offset * GRADIENT_WAVE_SHIFT;
    // For 2-stop gradient: span = 1.0, period = 2.0
    // Palindrome reflection: values > 1.0 bounce back instead of wrapping
    let period = 2.0f32;
    let t = ((t % period) + period) % period; // positive modulo
    let t = if t > 1.0 { period - t } else { t };
    [
        start[0] + (end[0] - start[0]) * t,
        start[1] + (end[1] - start[1]) * t,
        start[2] + (end[2] - start[2]) * t,
        1.0,
    ]
}

/// Interpolate between gradient start and end colors (default theme).
/// `t` is in [0, 1], `wave_offset` shifts for the super-cycle animation.
pub fn gradient_color(t: f32, wave_offset: f32) -> [f32; 4] {
    gradient_color_with(t, wave_offset, GRADIENT_START, GRADIENT_END)
}

/// Interpolate across 7 gradient stops with palindrome reflection.
/// `t` is in [0, 1], mapped to 6 segments between 7 stops.
/// Wave offset shifts the gradient origin for the super-cycle animation.
pub fn gradient_color_7stop(t: f32, wave_offset: f32, stops: &[[f32; 3]; 7]) -> [f32; 4] {
    let t = t + wave_offset * GRADIENT_WAVE_SHIFT;
    let t = t * 6.0; // Map [0,1] to [0,6] for 7 stops
    let period = 12.0f32;
    let t = ((t % period) + period) % period; // positive modulo
    let t = if t > 6.0 { period - t } else { t }; // palindrome reflection
    let seg = (t as usize).min(5);
    let frac = t - seg as f32;
    let a = stops[seg];
    let b = stops[seg + 1];
    [
        a[0] + (b[0] - a[0]) * frac,
        a[1] + (b[1] - a[1]) * frac,
        a[2] + (b[2] - a[2]) * frac,
        1.0,
    ]
}

/// Hit-test: determine which clickable target a column position falls in.
/// Brand is the rightmost element — any column past `brand_start_col` counts as Brand,
/// including the padding gap on the right edge of the status bar.
pub fn hit_test(data: &StatusBarData, col: usize) -> StatusBarTarget {
    if col >= data.brand_start_col {
        StatusBarTarget::Brand
    } else if data.ai_stats_end_col > data.ai_stats_start_col
        && col >= data.ai_stats_start_col
        && col < data.ai_stats_end_col
    {
        StatusBarTarget::AiStats
    } else if data.theme_area_end_col > data.theme_area_start_col
        && col >= data.theme_area_start_col
        && col < data.theme_area_end_col
    {
        StatusBarTarget::ThemeArea
    } else if data.title_end_col > data.title_start_col
        && col >= data.title_start_col
        && col < data.title_end_col
    {
        StatusBarTarget::Title
    } else if data.project_end_col > data.project_start_col
        && col >= data.project_start_col
        && col < data.project_end_col
    {
        StatusBarTarget::Project
    } else {
        StatusBarTarget::None
    }
}

/// Shimmer brightness boost for a column at a given time.
/// Matches the C version: linear cone falloff with radius 2.5, blending toward white.
/// Returns a multiplier >= 1.0 during the shimmer phase (first 1.5s of 15s cycle).
pub fn shimmer_brightness(col: usize, brand_start: usize, brand_end: usize, time_secs: f32) -> f32 {
    let cycle_pos = (time_secs * 1000.0) % SHIMMER_CYCLE_MS;

    // Shimmer only during first 1500ms of the cycle
    if cycle_pos > 1500.0 {
        return 1.0;
    }

    // Only shimmer over brand text columns
    if col < brand_start || col >= brand_end {
        return 1.0;
    }

    let brand_width = (brand_end - brand_start) as f32;
    if brand_width < 1.0 {
        return 1.0;
    }

    // Spotlight sweeps across brand text (relative to brand_start)
    let progress = cycle_pos / 1500.0;
    let spotlight = progress * (brand_width - 1.0);
    let dist = ((col - brand_start) as f32 - spotlight).abs();

    // Linear cone falloff matching C version (radius = 2.5)
    let radius = 2.5f32;
    if dist >= radius {
        return 1.0;
    }
    let brightness = 1.0 - dist / radius;
    1.0 + brightness * 0.85
}

/// CTX bar fill color — matches the C binary's thresholds exactly.
/// Returns RGBA fill color for the ▰ filled blocks.
pub fn ctx_fill_color(pct: f32) -> [f32; 4] {
    if pct >= 95.0 {
        [1.0, 0.0, 0.0, 1.0]             // #FF0000 red
    } else if pct >= 85.0 {
        [1.0, 0.2, 0.2, 1.0]             // #FF3333 dark red
    } else if pct >= 70.0 {
        [1.0, 0.420, 0.0, 1.0]           // #FF6B00 orange
    } else if pct >= 50.0 {
        [1.0, 0.722, 0.0, 1.0]           // #FFB800 yellow
    } else {
        [0.0, 0.8, 0.267, 1.0]           // #00CC44 green
    }
}

/// Gray color for the ▱ empty blocks in CTX bar.
const CTX_EMPTY_COLOR: [f32; 4] = [0.267, 0.267, 0.267, 1.0]; // #444444

/// Breathing brightness factor. Returns a multiplier in [BREATHING_DIM, 1.3].
/// Active during 8-13s of the 15s cycle.
pub fn breathing_factor(time_secs: f32) -> f32 {
    let cycle_pos = (time_secs * 1000.0) % SHIMMER_CYCLE_MS;

    // Breathing only during 8000-13000ms of the cycle
    if !(8000.0..=13000.0).contains(&cycle_pos) {
        return 1.0;
    }

    // Map 8000-13000 to 0-1 for one sine cycle
    let t = (cycle_pos - 8000.0) / 5000.0;
    let sine = (t * std::f32::consts::PI * 2.0).sin();

    // Map sine [-1, 1] to [BREATHING_DIM, 1.3]
    let range = 1.3 - BREATHING_DIM;
    BREATHING_DIM + (sine + 1.0) * 0.5 * range
}

/// Get the animated dot character for the current time.
/// During bloom (2-7s of 15s cycle), cycles through the bloom sequence.
/// Outside bloom, stays as a small middle dot `·`.
pub fn animated_dot_char(time_secs: f32) -> char {
    let cycle_pos = (time_secs * 1000.0) % SHIMMER_CYCLE_MS;

    // Dot animation during 2000-7000ms
    if !(2000.0..=7000.0).contains(&cycle_pos) {
        return '\u{00B7}'; // · (resting state)
    }

    // 200ms per frame
    let frame = ((cycle_pos - 2000.0) / 200.0) as usize;
    DOT_FRAMES[frame % DOT_FRAMES.len()]
}

/// Gradient wave offset for the super-cycle (120s).
/// Matches C code exactly: first 60s static, then 60s of GRADIENT_WAVES (2)
/// oscillations using cosine ease-in/ease-out. Returns [0, 1].
pub fn gradient_wave_offset(time_secs: f32) -> f32 {
    let cycle_ms = (time_secs * 1000.0) % GRADIENT_SUPERCYCLE_MS;
    // First 60s: static (no offset)
    if cycle_ms < GRADIENT_STATIC_MS {
        return 0.0;
    }
    // C code: phase = (super_ms - GRADIENT_STATIC_MS) / (SUPERCYCLE - STATIC)
    let phase = (cycle_ms - GRADIENT_STATIC_MS) / (GRADIENT_SUPERCYCLE_MS - GRADIENT_STATIC_MS);
    // wave = fmod(phase * GRADIENT_WAVES, 1.0)
    let wave = (phase * GRADIENT_WAVES) % 1.0;
    // offset = 0.5 - 0.5 * cos(wave * 2π) — smooth ease-in/ease-out
    0.5 - 0.5 * (wave * 2.0 * std::f32::consts::PI).cos()
}
