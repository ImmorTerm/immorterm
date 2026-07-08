//! Vendor-agnostic conversation schema — `immorterm-conversation.v1.jsonl`.
//!
//! One `NormalizedEvent` per line. Anthropic-style `ContentBlock`s are the
//! richest superset across vendors; other formats flatten into them cleanly.
//!
//! Defined by internal design notes §4.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Thinking { text: String },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub input_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_read: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_creation: u64,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub cost_usd: f64,
}

fn is_zero_u64(v: &u64) -> bool { *v == 0 }
fn is_zero_f64(v: &f64) -> bool { *v == 0.0 }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NormalizedEvent {
    pub v: u32,
    pub ts: f64,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub immorterm_session_id: String,
    pub tool: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_version: String,
    pub role: Role,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_id: String,
    pub content: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "is_default_usage")]
    pub usage: Usage,
}

fn is_default_usage(u: &Usage) -> bool {
    u == &Usage::default()
}

impl NormalizedEvent {
    pub fn new(tool: impl Into<String>, session_id: impl Into<String>, role: Role) -> Self {
        Self {
            v: SCHEMA_VERSION,
            ts: 0.0,
            session_id: session_id.into(),
            immorterm_session_id: String::new(),
            tool: tool.into(),
            tool_version: String::new(),
            role,
            message_id: String::new(),
            parent_id: String::new(),
            content: Vec::new(),
            usage: Usage::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_text_user_event() {
        let e = NormalizedEvent {
            v: 1,
            ts: 1776249086.484,
            session_id: "06875dde".into(),
            immorterm_session_id: "24243-537f67a6".into(),
            tool: "claude-code".into(),
            tool_version: "2.0.x".into(),
            role: Role::User,
            message_id: "msg_01".into(),
            parent_id: String::new(),
            content: vec![ContentBlock::Text { text: "hello".into() }],
            usage: Usage::default(),
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: NormalizedEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
        assert!(s.contains("\"role\":\"user\""));
        assert!(s.contains("\"type\":\"text\""));
        assert!(!s.contains("parent_id"), "empty parent_id must be omitted");
        assert!(!s.contains("usage"), "default usage must be omitted");
    }

    #[test]
    fn tool_use_and_result_roundtrip() {
        let blocks = vec![
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
            },
            ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: Value::String("file.txt".into()),
                is_error: false,
            },
        ];
        let s = serde_json::to_string(&blocks).unwrap();
        let back: Vec<ContentBlock> = serde_json::from_str(&s).unwrap();
        assert_eq!(blocks, back);
        assert!(s.contains("\"type\":\"tool_use\""));
        assert!(s.contains("\"type\":\"tool_result\""));
    }

    #[test]
    fn thinking_block_serializes_as_thinking_type() {
        let b = ContentBlock::Thinking { text: "reasoning".into() };
        let s = serde_json::to_string(&b).unwrap();
        assert!(s.contains("\"type\":\"thinking\""), "got {}", s);
    }
}
