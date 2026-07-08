//! Claude Code JSONL adapter — direct port of claude-replay's `claude-code.mjs`.
//!
//! Each line is `{type: "user"|"assistant", message: {role, content}, timestamp}`.
//! Consecutive user messages merge into a single turn; tool_results scan forward
//! from the next user message and match by `tool_use_id`.

use crate::shared::build_turns_from_entries;
use crate::turn::Turn;
use crate::ConversationAdapter;
use serde_json::Value;

pub struct ClaudeCode;

impl ConversationAdapter for ClaudeCode {
    fn tool_name(&self) -> &'static str { "claude-code" }

    fn detect(&self, obj: &Value) -> bool {
        match obj.get("type").and_then(|v| v.as_str()) {
            Some("user") | Some("assistant") => true,
            _ => false,
        }
    }

    fn parse(&self, text: &str) -> Vec<Turn> {
        let mut entries = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            let obj: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match obj.get("type").and_then(|v| v.as_str()) {
                Some("user") | Some("assistant") => entries.push(obj),
                _ => {}
            }
        }
        build_turns_from_entries(&entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_user_assistant_roundtrip() {
        let text = r#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2025-06-01T10:00:00Z"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]},"timestamp":"2025-06-01T10:00:01Z"}"#;
        let turns = ClaudeCode.parse(text);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_text, "hi");
        assert_eq!(turns[0].blocks.len(), 1);
        assert_eq!(turns[0].blocks[0].text, "hello");
    }

    #[test]
    fn detects_claude_code_by_type() {
        let obj: Value = serde_json::from_str(r#"{"type":"user"}"#).unwrap();
        assert!(ClaudeCode.detect(&obj));
        let cursor_like: Value = serde_json::from_str(r#"{"role":"user"}"#).unwrap();
        assert!(!ClaudeCode.detect(&cursor_like));
    }
}
