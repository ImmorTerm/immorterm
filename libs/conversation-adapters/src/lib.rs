//! Vendor-agnostic transcript adapters.
//!
//! Mirrors `claude-replay`'s format registry: each adapter exposes `tool_name`,
//! `detect`, and `parse`. `detect_format_from_text` scans JSONL line-by-line
//! (plus a text-level pass for Gemini) until one adapter claims the text.
//!
//! Adding a new format: implement `ConversationAdapter`, register it in
//! `json_adapters()` or `text_adapters()`, add a fixture under `tests/`.

// These lints fire on the line-for-line port of claude-replay's JS source.
// We deliberately keep the structure close to the upstream so corrections
// can be replayed mechanically; collapsing or re-styling diverges from JS.
#![allow(
    clippy::collapsible_if,
    clippy::match_like_matches_macro,
    clippy::unnecessary_cast,
    clippy::let_and_return,
    clippy::doc_lazy_continuation,
    clippy::redundant_closure,
    clippy::drain_collect,
)]

use conversation_schema::NormalizedEvent;
use serde_json::Value;

pub mod shared;
pub mod turn;

pub mod formats {
    pub mod aider;
    pub mod claude_code;
    pub mod cline;
    pub mod codex;
    pub mod cursor;
    pub mod gemini;
    pub mod opencode;
    pub mod windsurf;
}

use turn::{turns_to_events, Turn};

/// Common contract every per-vendor adapter implements.
pub trait ConversationAdapter: Sync {
    /// Lowercase tool identifier (e.g. `"claude-code"`).
    fn tool_name(&self) -> &'static str;

    /// JSONL-line detector. Return `true` if the first parseable object
    /// belongs to this format. Gemini overrides `detect_from_text` instead.
    fn detect(&self, _first_obj: &Value) -> bool { false }

    /// Whole-text detector for non-JSONL formats (Gemini).
    fn detect_from_text(&self, _text: &str) -> bool { false }

    /// Parse transcript text into vendor-agnostic `Turn`s.
    fn parse(&self, text: &str) -> Vec<Turn>;
}

/// JSONL adapters, in detection priority order.
/// More specific formats (codex, opencode, windsurf, cline) come before
/// generic (claude-code, cursor) — Cline must precede Cursor because both
/// share `{role, ...}` shape; Cline's numeric `ts` discriminator fires first.
pub fn json_adapters() -> Vec<&'static dyn ConversationAdapter> {
    vec![
        &formats::codex::Codex,
        &formats::opencode::OpenCode,
        &formats::windsurf::Windsurf,
        &formats::cline::Cline,
        &formats::claude_code::ClaudeCode,
        &formats::cursor::Cursor,
    ]
}

/// Text-level adapters (single JSON object or markdown rather than JSONL).
pub fn text_adapters() -> Vec<&'static dyn ConversationAdapter> {
    vec![&formats::gemini::Gemini, &formats::aider::Aider]
}

/// Detect which format `text` belongs to, returning the tool name or
/// `"unknown"` if no adapter claims it.
pub fn detect_format_from_text(text: &str) -> &'static str {
    for a in text_adapters() {
        if a.detect_from_text(text) {
            return a.tool_name();
        }
    }
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        let obj: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for a in json_adapters() {
            if a.detect(&obj) {
                return a.tool_name();
            }
        }
    }
    "unknown"
}

/// Parse `text` into `Turn`s using the detected adapter. Returns empty on unknown.
pub fn parse_turns_from_text(text: &str) -> (Vec<Turn>, &'static str) {
    let name = detect_format_from_text(text);
    if name == "unknown" { return (Vec::new(), name); }
    let all: Vec<&dyn ConversationAdapter> =
        text_adapters().into_iter().chain(json_adapters()).collect();
    if let Some(a) = all.iter().find(|a| a.tool_name() == name) {
        return (a.parse(text), name);
    }
    (Vec::new(), "unknown")
}

/// Parse `text` and flatten to `NormalizedEvent`s, labelled with the
/// `session_id` / `immorterm_session_id` the caller threads from the session
/// registry.
pub fn parse_events_from_text(
    text: &str,
    session_id: &str,
    immorterm_session_id: &str,
) -> Vec<NormalizedEvent> {
    let (turns, tool) = parse_turns_from_text(text);
    if turns.is_empty() { return Vec::new(); }
    turns_to_events(&turns, tool, session_id, immorterm_session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_format_returns_empty() {
        let (turns, name) = parse_turns_from_text("not json at all\nstill not json");
        assert!(turns.is_empty());
        assert_eq!(name, "unknown");
    }

    #[test]
    fn detect_claude_code_from_type_field() {
        let text = r#"{"type":"user","message":{"role":"user","content":"hi"}}"#;
        assert_eq!(detect_format_from_text(text), "claude-code");
    }
}
