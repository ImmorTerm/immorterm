//! AI Expression Protocol — per-cell rendering hints set by AI agents.
//!
//! AI calls `immorterm_express()` to set its "emotional state" which gets stamped
//! onto every subsequent terminal cell. The renderer interprets these hints as
//! visual effects (brightness, color, glow, animation).
//!
//! Storage: `ExpressionMeta` is a packed u16 per cell (2 bytes overhead).
//! The daemon holds the full `ExpressionState` and converts to `ExpressionMeta`
//! for cell stamping.

use serde::{Deserialize, Serialize};

// ─── Enums ──────────────────────────────────────────────────────────

/// AI confidence level — affects text brightness/alpha.
/// Packed into 4 bits (0 = unset/default, 1-15 mapped to ~0.07-1.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Confidence {
    /// No confidence set — render normally.
    #[default]
    Unset,
    /// Explicit confidence level (0.0 = invisible, 1.0 = fully bright).
    Level(u8), // 1-15, maps to 1/15..15/15
}

/// Danger level — triggers glow, vignette, screen shake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum DangerLevel {
    #[default]
    None = 0,
    Low = 1,
    Medium = 2,
    High = 3,
    Critical = 4,
}

/// AI mood — maps to a color palette in the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum Mood {
    #[default]
    Neutral = 0,
    Confident = 1,
    Cautious = 2,
    Creative = 3,
    Warning = 4,
    Error = 5,
    Success = 6,
    Excited = 7,
    Focused = 8,
    Playful = 9,
}

/// Per-character text animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum Animation {
    #[default]
    None = 0,
    Pulse = 1,
    Glow = 2,
    Wave = 3,
    Typewriter = 4,
    Rainbow = 5,
    Shimmer = 6,
}

/// One-shot celebration effect (triggers particles, then auto-clears).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Celebration {
    Confetti,
    Sparkle,
    Fireworks,
}

// ─── Full Expression State (daemon-side) ─────────────────────────────

/// The AI's current "emotional state" — set via MCP, applied to all subsequent cells.
///
/// Lives in `Terminal` alongside cursor attributes. When AI calls `immorterm_express()`,
/// this state updates. When text arrives from PTY, each cell gets stamped with a
/// compact `ExpressionMeta` derived from this state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExpressionState {
    /// Text brightness (0.0 = invisible, 1.0 = full brightness). None = unset.
    pub confidence: Option<f32>,
    /// Danger level — visual warning intensity.
    pub danger: DangerLevel,
    /// Semantic mood — mapped to color palette by renderer.
    pub mood: Mood,
    /// Per-character animation effect.
    pub animation: Animation,
    /// One-shot celebration (cleared after triggering).
    pub celebrate: Option<Celebration>,
    /// Effect intensity multiplier (0.0-1.0). Default 1.0.
    pub intensity: f32,
    /// Explicit color override (hex → RGBA). None = use mood-based color.
    pub color_override: Option<[f32; 4]>,
}

impl ExpressionState {
    pub fn new() -> Self {
        Self {
            intensity: 1.0,
            ..Default::default()
        }
    }

    /// Reset all expression to defaults (called by `reset: true`).
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Is this the default state (no expression active)?
    pub fn is_default(&self) -> bool {
        self.confidence.is_none()
            && self.danger == DangerLevel::None
            && self.mood == Mood::Neutral
            && self.animation == Animation::None
            && self.celebrate.is_none()
            && self.color_override.is_none()
    }

    /// Convert to compact per-cell metadata for stamping.
    pub fn to_meta(&self) -> ExpressionMeta {
        let mut m = ExpressionMeta::NONE;

        // Confidence: pack into 4 bits (0 = unset, 1-15 = levels)
        if let Some(c) = self.confidence {
            let level = (c.clamp(0.0, 1.0) * 15.0).round() as u16;
            let level = level.max(1); // never 0 when explicitly set
            m.0 |= level & 0x000F;
        }

        // Danger: bits 4-6
        m.0 |= (self.danger as u16 & 0x07) << 4;

        // Mood: bits 7-10
        m.0 |= (self.mood as u16 & 0x0F) << 7;

        // Animation: bits 11-13
        m.0 |= (self.animation as u16 & 0x07) << 11;

        // Color override flag: bit 14
        if self.color_override.is_some() {
            m.0 |= 1 << 14;
        }

        m
    }

    /// Take and clear the celebration (one-shot).
    pub fn take_celebration(&mut self) -> Option<Celebration> {
        self.celebrate.take()
    }
}

// ─── Compact Per-Cell Metadata (u16) ─────────────────────────────────

/// Packed AI expression metadata stored per terminal cell.
///
/// Layout (16 bits):
/// ```text
/// [15] reserved
/// [14] has_color_override
/// [13:11] animation (3 bits → 8 types)
/// [10:7]  mood      (4 bits → 16 moods)
/// [6:4]   danger    (3 bits → 8 levels)
/// [3:0]   confidence (4 bits → 0=unset, 1-15=levels)
/// ```
///
/// Default value `0` means "no expression" — render normally with zero overhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(transparent)]
pub struct ExpressionMeta(pub u16);

impl ExpressionMeta {
    /// No expression — render cell normally.
    pub const NONE: Self = Self(0);

    /// Returns true if this cell has no expression metadata.
    #[inline]
    pub fn is_none(&self) -> bool {
        self.0 == 0
    }

    /// Confidence level (None = unset, Some(0.067..1.0) = explicit).
    #[inline]
    pub fn confidence(&self) -> Option<f32> {
        let v = self.0 & 0x000F;
        if v == 0 {
            None
        } else {
            Some(v as f32 / 15.0)
        }
    }

    /// Danger level.
    #[inline]
    pub fn danger(&self) -> DangerLevel {
        match (self.0 >> 4) & 0x07 {
            1 => DangerLevel::Low,
            2 => DangerLevel::Medium,
            3 => DangerLevel::High,
            4 => DangerLevel::Critical,
            _ => DangerLevel::None,
        }
    }

    /// Semantic mood.
    #[inline]
    pub fn mood(&self) -> Mood {
        match (self.0 >> 7) & 0x0F {
            1 => Mood::Confident,
            2 => Mood::Cautious,
            3 => Mood::Creative,
            4 => Mood::Warning,
            5 => Mood::Error,
            6 => Mood::Success,
            7 => Mood::Excited,
            8 => Mood::Focused,
            9 => Mood::Playful,
            _ => Mood::Neutral,
        }
    }

    /// Text animation.
    #[inline]
    pub fn animation(&self) -> Animation {
        match (self.0 >> 11) & 0x07 {
            1 => Animation::Pulse,
            2 => Animation::Glow,
            3 => Animation::Wave,
            4 => Animation::Typewriter,
            5 => Animation::Rainbow,
            6 => Animation::Shimmer,
            _ => Animation::None,
        }
    }

    /// Whether this cell has an explicit color override (stored externally).
    #[inline]
    pub fn has_color_override(&self) -> bool {
        self.0 & (1 << 14) != 0
    }
}

// ─── Parsing helpers (from MCP string arguments) ─────────────────────

impl DangerLevel {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "low" => Self::Low,
            "medium" | "med" => Self::Medium,
            "high" => Self::High,
            "critical" | "crit" => Self::Critical,
            _ => Self::None,
        }
    }
}

impl Mood {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "confident" => Self::Confident,
            "cautious" | "uncertain" => Self::Cautious,
            "creative" => Self::Creative,
            "warning" | "warn" => Self::Warning,
            "error" | "err" => Self::Error,
            "success" | "ok" => Self::Success,
            "excited" => Self::Excited,
            "focused" | "focus" => Self::Focused,
            "playful" | "fun" => Self::Playful,
            _ => Self::Neutral,
        }
    }
}

impl Animation {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "pulse" => Self::Pulse,
            "glow" => Self::Glow,
            "wave" => Self::Wave,
            "typewriter" | "type" => Self::Typewriter,
            "rainbow" => Self::Rainbow,
            "shimmer" => Self::Shimmer,
            _ => Self::None,
        }
    }
}

impl Celebration {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "confetti" => Some(Self::Confetti),
            "sparkle" | "sparkles" => Some(Self::Sparkle),
            "fireworks" => Some(Self::Fireworks),
            _ => None,
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_expression_is_none() {
        let state = ExpressionState::new();
        assert!(state.is_default());
        let meta = state.to_meta();
        assert!(meta.is_none());
        assert_eq!(meta.0, 0);
    }

    #[test]
    fn confidence_roundtrip() {
        let mut state = ExpressionState::new();
        state.confidence = Some(0.7);
        let meta = state.to_meta();
        // 0.7 * 15 = 10.5, rounds to 11 → 11/15 ≈ 0.733
        let c = meta.confidence().unwrap();
        assert!((c - 0.733).abs() < 0.01);
    }

    #[test]
    fn danger_roundtrip() {
        let mut state = ExpressionState::new();
        state.danger = DangerLevel::Critical;
        let meta = state.to_meta();
        assert_eq!(meta.danger(), DangerLevel::Critical);
    }

    #[test]
    fn mood_roundtrip() {
        let mut state = ExpressionState::new();
        state.mood = Mood::Creative;
        let meta = state.to_meta();
        assert_eq!(meta.mood(), Mood::Creative);
    }

    #[test]
    fn animation_roundtrip() {
        let mut state = ExpressionState::new();
        state.animation = Animation::Wave;
        let meta = state.to_meta();
        assert_eq!(meta.animation(), Animation::Wave);
    }

    #[test]
    fn color_override_flag() {
        let mut state = ExpressionState::new();
        state.color_override = Some([1.0, 0.0, 0.0, 1.0]);
        let meta = state.to_meta();
        assert!(meta.has_color_override());
    }

    #[test]
    fn full_expression_roundtrip() {
        let mut state = ExpressionState::new();
        state.confidence = Some(0.5);
        state.danger = DangerLevel::High;
        state.mood = Mood::Cautious;
        state.animation = Animation::Pulse;
        state.color_override = Some([1.0, 0.5, 0.0, 1.0]);

        let meta = state.to_meta();

        assert!(meta.confidence().unwrap() > 0.4);
        assert_eq!(meta.danger(), DangerLevel::High);
        assert_eq!(meta.mood(), Mood::Cautious);
        assert_eq!(meta.animation(), Animation::Pulse);
        assert!(meta.has_color_override());
    }

    #[test]
    fn parse_danger_from_string() {
        assert_eq!(DangerLevel::from_str_loose("HIGH"), DangerLevel::High);
        assert_eq!(DangerLevel::from_str_loose("crit"), DangerLevel::Critical);
        assert_eq!(DangerLevel::from_str_loose("unknown"), DangerLevel::None);
    }

    #[test]
    fn parse_mood_from_string() {
        assert_eq!(Mood::from_str_loose("uncertain"), Mood::Cautious);
        assert_eq!(Mood::from_str_loose("OK"), Mood::Success);
    }

    #[test]
    fn celebration_one_shot() {
        let mut state = ExpressionState::new();
        state.celebrate = Some(Celebration::Confetti);
        assert_eq!(state.take_celebration(), Some(Celebration::Confetti));
        assert_eq!(state.take_celebration(), None);
    }
}
