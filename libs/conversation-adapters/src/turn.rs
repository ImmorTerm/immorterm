//! Intermediate turn-grouped representation — direct Rust translation of
//! claude-replay's `Turn`/`AssistantBlock`/`ToolCall` types.
//!
//! We parse into `Turn`s first (close to claude-replay's shape so porting is
//! line-by-line verifiable), then flatten into `NormalizedEvent`s for storage.

use conversation_schema::{ContentBlock, NormalizedEvent, Role};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub tool_use_id: String,
    pub name: String,
    pub input: Value,
    pub result: Option<String>,
    pub result_timestamp: Option<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Text,
    Thinking,
    ToolUse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssistantBlock {
    pub kind: BlockKind,
    pub text: String,
    pub tool_call: Option<ToolCall>,
    pub timestamp: Option<String>,
}

impl AssistantBlock {
    pub fn text(text: impl Into<String>, ts: Option<String>) -> Self {
        Self { kind: BlockKind::Text, text: text.into(), tool_call: None, timestamp: ts }
    }
    pub fn thinking(text: impl Into<String>, ts: Option<String>) -> Self {
        Self { kind: BlockKind::Thinking, text: text.into(), tool_call: None, timestamp: ts }
    }
    pub fn tool_use(call: ToolCall, ts: Option<String>) -> Self {
        Self { kind: BlockKind::ToolUse, text: String::new(), tool_call: Some(call), timestamp: ts }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Turn {
    pub index: u32,
    pub user_text: String,
    pub blocks: Vec<AssistantBlock>,
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub system_events: Vec<String>,
}

/// Flatten a list of turns into normalized events (user message, then assistant
/// message, then any tool_result user messages). One `NormalizedEvent` per role
/// switch — matches the schema's "one event per line" contract.
pub fn turns_to_events(
    turns: &[Turn],
    tool: &str,
    session_id: &str,
    immorterm_session_id: &str,
) -> Vec<NormalizedEvent> {
    let mut out = Vec::new();
    for turn in turns {
        let ts = parse_iso_ts(&turn.timestamp);
        if !turn.user_text.is_empty() {
            out.push(NormalizedEvent {
                v: conversation_schema::SCHEMA_VERSION,
                ts,
                session_id: session_id.to_string(),
                immorterm_session_id: immorterm_session_id.to_string(),
                tool: tool.to_string(),
                tool_version: String::new(),
                role: Role::User,
                message_id: String::new(),
                parent_id: String::new(),
                content: vec![ContentBlock::Text { text: turn.user_text.clone() }],
                usage: Default::default(),
            });
        }

        // Assistant event: collect text/thinking/tool_use blocks
        let mut a_blocks = Vec::new();
        let mut tool_results = Vec::new();
        for b in &turn.blocks {
            match b.kind {
                BlockKind::Text => a_blocks.push(ContentBlock::Text { text: b.text.clone() }),
                BlockKind::Thinking => a_blocks.push(ContentBlock::Thinking { text: b.text.clone() }),
                BlockKind::ToolUse => {
                    if let Some(tc) = &b.tool_call {
                        a_blocks.push(ContentBlock::ToolUse {
                            id: tc.tool_use_id.clone(),
                            name: tc.name.clone(),
                            input: tc.input.clone(),
                        });
                        if let Some(r) = &tc.result {
                            tool_results.push(ContentBlock::ToolResult {
                                tool_use_id: tc.tool_use_id.clone(),
                                content: Value::String(r.clone()),
                                is_error: tc.is_error,
                            });
                        }
                    }
                }
            }
        }
        if !a_blocks.is_empty() {
            out.push(NormalizedEvent {
                v: conversation_schema::SCHEMA_VERSION,
                ts,
                session_id: session_id.to_string(),
                immorterm_session_id: immorterm_session_id.to_string(),
                tool: tool.to_string(),
                tool_version: String::new(),
                role: Role::Assistant,
                message_id: String::new(),
                parent_id: String::new(),
                content: a_blocks,
                usage: Default::default(),
            });
        }
        if !tool_results.is_empty() {
            out.push(NormalizedEvent {
                v: conversation_schema::SCHEMA_VERSION,
                ts,
                session_id: session_id.to_string(),
                immorterm_session_id: immorterm_session_id.to_string(),
                tool: tool.to_string(),
                tool_version: String::new(),
                role: Role::User,
                message_id: String::new(),
                parent_id: String::new(),
                content: tool_results,
                usage: Default::default(),
            });
        }
    }
    out
}

fn parse_iso_ts(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    // Minimal ISO-8601 → epoch-seconds conversion without chrono. Best-effort;
    // failures produce 0.0 (we don't block on missing timestamps).
    chrono_like::parse_rfc3339_seconds(s).unwrap_or(0.0)
}

/// Tiny inline ISO-8601 parser — no chrono dep. Handles `YYYY-MM-DDTHH:MM:SS(.fff)?(Z|±HH:MM)?`.
mod chrono_like {
    pub fn parse_rfc3339_seconds(s: &str) -> Option<f64> {
        // Extract Y-M-DTH:M:S and optional fractional seconds.
        let s = s.trim();
        if s.len() < 19 { return None; }
        let year: i64 = s[0..4].parse().ok()?;
        let month: u32 = s[5..7].parse().ok()?;
        let day: u32 = s[8..10].parse().ok()?;
        let hour: u32 = s[11..13].parse().ok()?;
        let minute: u32 = s[14..16].parse().ok()?;
        let second: u32 = s[17..19].parse().ok()?;

        let mut frac = 0.0_f64;
        let mut i = 19;
        if s.as_bytes().get(i) == Some(&b'.') {
            i += 1;
            let start = i;
            while i < s.len() && s.as_bytes()[i].is_ascii_digit() { i += 1; }
            let digits = &s[start..i];
            if let Ok(n) = digits.parse::<u64>() {
                frac = n as f64 / 10f64.powi(digits.len() as i32);
            }
        }

        // Days since Unix epoch (proleptic Gregorian). Inline to avoid chrono.
        let days = days_from_civil(year, month as i32, day as i32);
        let secs = days * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + second as i64;
        Some(secs as f64 + frac)
    }

    // Howard Hinnant's days_from_civil algorithm.
    fn days_from_civil(y: i64, m: i32, d: i32) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = (if y >= 0 { y } else { y - 399 }) / 400;
        let yoe = (y - era * 400) as u64;
        let doy = ((153 * (if m > 2 { m - 3 } else { m + 9 }) as u64 + 2) / 5
            + d as u64 - 1) as u64;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe as i64 - 719_468
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_ts_parses_epoch_and_fraction() {
        // 2025-06-01T10:00:00Z → 1748772000
        let t = chrono_like::parse_rfc3339_seconds("2025-06-01T10:00:00Z").unwrap();
        assert!((t - 1748772000.0).abs() < 1.0, "got {}", t);
        let t2 = chrono_like::parse_rfc3339_seconds("2025-06-01T10:00:00.500Z").unwrap();
        assert!((t2 - 1748772000.5).abs() < 0.001);
    }

    #[test]
    fn empty_turn_produces_no_event() {
        let turns = vec![Turn::default()];
        let events = turns_to_events(&turns, "x", "s", "i");
        assert!(events.is_empty());
    }

    #[test]
    fn user_and_assistant_flatten_correctly() {
        let turns = vec![Turn {
            index: 1,
            user_text: "hi".into(),
            blocks: vec![AssistantBlock::text("hello back", None)],
            timestamp: "2025-06-01T10:00:00Z".into(),
            system_events: vec![],
        }];
        let events = turns_to_events(&turns, "claude-code", "S1", "I1");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].role, Role::User);
        assert_eq!(events[1].role, Role::Assistant);
    }
}
