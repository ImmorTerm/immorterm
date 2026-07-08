//! Cursor JSONL adapter — direct port of claude-replay's `cursor.mjs`.
//!
//! Cursor uses `{role, message: {role, content}}` without a top-level `type`.
//! We normalize to Claude Code's shape, then reclassify all-but-last assistant
//! text blocks as `thinking` (Cursor emits reasoning interleaved with replies).

use crate::shared::build_turns_from_entries;
use crate::turn::{BlockKind, Turn};
use crate::ConversationAdapter;
use serde_json::{json, Value};

pub struct Cursor;

impl ConversationAdapter for Cursor {
    fn tool_name(&self) -> &'static str { "cursor" }

    fn detect(&self, obj: &Value) -> bool {
        if obj.get("type").is_some() { return false; }
        let role = obj.get("message").and_then(|m| m.get("role")).and_then(|r| r.as_str())
            .or_else(|| obj.get("role").and_then(|r| r.as_str()));
        matches!(role, Some("user") | Some("assistant"))
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
            if obj.get("type").is_some() { continue; }
            let role = obj.get("message").and_then(|m| m.get("role")).and_then(|r| r.as_str())
                .or_else(|| obj.get("role").and_then(|r| r.as_str()))
                .unwrap_or("");
            if role != "user" && role != "assistant" { continue; }
            let content = obj.get("message").and_then(|m| m.get("content")).cloned()
                .unwrap_or(Value::String(String::new()));
            let timestamp = obj.get("timestamp").cloned().unwrap_or(Value::Null);
            entries.push(json!({
                "type": role,
                "message": { "role": role, "content": content },
                "timestamp": timestamp,
            }));
        }

        let mut turns = build_turns_from_entries(&entries);

        // Cursor-specific: reclassify all-but-last text block as thinking.
        for turn in turns.iter_mut() {
            if turn.blocks.len() < 2 { continue; }
            let last = turn.blocks.len() - 1;
            for b in &mut turn.blocks[..last] {
                if b.kind == BlockKind::Text {
                    b.kind = BlockKind::Thinking;
                }
            }
        }

        turns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cursor_not_claude_code() {
        let cursor: Value = serde_json::from_str(r#"{"role":"user","message":{"role":"user","content":"x"}}"#).unwrap();
        assert!(Cursor.detect(&cursor));
        let cc: Value = serde_json::from_str(r#"{"type":"user","message":{"role":"user","content":"x"}}"#).unwrap();
        assert!(!Cursor.detect(&cc));
    }

    #[test]
    fn reclassifies_intermediate_text_as_thinking() {
        let text = r#"{"role":"user","message":{"role":"user","content":"go"}}
{"role":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"thinking..."},{"type":"text","text":"final answer"}]}}"#;
        let turns = Cursor.parse(text);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].blocks.len(), 2);
        assert_eq!(turns[0].blocks[0].kind, BlockKind::Thinking);
        assert_eq!(turns[0].blocks[1].kind, BlockKind::Text);
    }
}
