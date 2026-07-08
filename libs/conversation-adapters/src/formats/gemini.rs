//! Gemini CLI adapter — direct port of claude-replay's `gemini.mjs`.
//!
//! Single JSON object: `{sessionId, messages[]}`. Messages carry `type`,
//! `content`, and for `gemini` messages: `thoughts[]` + `toolCalls[]`.
//! Tool names are mapped into Claude-style (`Bash`, `Read`, ...) so downstream
//! consumers see a uniform vocabulary across vendors.

use crate::shared::{clean_system_tags, filter_empty_turns};
use crate::turn::{AssistantBlock, ToolCall, Turn};
use crate::ConversationAdapter;
use once_cell::sync::Lazy;
use serde_json::{Map, Value};
use std::collections::HashMap;

pub struct Gemini;

static TOOL_MAP: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("run_shell_command", "Bash");
    m.insert("shell", "Bash");
    m.insert("read_file", "Read");
    m.insert("read_many_files", "Read");
    m.insert("edit_file", "Edit");
    m.insert("write_file", "Write");
    m.insert("write_to_file", "Write");
    m.insert("list_directory", "Glob");
    m.insert("search_files", "Grep");
    m.insert("grep_search", "Grep");
    m.insert("web_search", "WebSearch");
    m.insert("web_fetch", "WebFetch");
    m.insert("complete_task", "complete_task");
    m
});

impl ConversationAdapter for Gemini {
    fn tool_name(&self) -> &'static str { "gemini" }

    fn detect_from_text(&self, text: &str) -> bool {
        let trimmed = text.trim();
        if !trimmed.starts_with('{') { return false; }
        let obj: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return false,
        };
        obj.get("sessionId").is_some()
            && obj.get("messages").map(|m| m.is_array()).unwrap_or(false)
    }

    fn parse(&self, text: &str) -> Vec<Turn> {
        let data: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        let messages = match data.get("messages").and_then(|m| m.as_array()) {
            Some(m) => m,
            None => return Vec::new(),
        };

        let mut turns: Vec<Turn> = Vec::new();
        let mut turn_index: u32 = 0;
        let mut user_text = String::new();
        let mut timestamp = String::new();
        let mut blocks: Vec<AssistantBlock> = Vec::new();

        for msg in messages {
            let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let ts = msg.get("timestamp").and_then(|t| t.as_str()).map(|s| s.to_string());

            if msg_type == "user" {
                finalize(&mut turns, &mut turn_index, &mut user_text, &mut timestamp, &mut blocks);
                user_text = clean_system_tags(
                    msg.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                );
                timestamp = ts.unwrap_or_default();
                continue;
            }

            if msg_type == "gemini" {
                if let Some(thoughts) = msg.get("thoughts").and_then(|t| t.as_array()) {
                    for thought in thoughts {
                        let subject = thought.get("subject").and_then(|s| s.as_str()).unwrap_or("").trim();
                        let description = thought.get("description").and_then(|s| s.as_str()).unwrap_or("").trim();
                        if subject.is_empty() && description.is_empty() { continue; }
                        let think = if !subject.is_empty() {
                            format!("{}: {}", subject, description)
                        } else { description.to_string() };
                        let tts = thought.get("timestamp").and_then(|t| t.as_str()).map(|s| s.to_string())
                            .or_else(|| ts.clone());
                        blocks.push(AssistantBlock::thinking(think, tts));
                    }
                }

                if let Some(tool_calls) = msg.get("toolCalls").and_then(|t| t.as_array()) {
                    for tc in tool_calls {
                        let raw_name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                        let mapped_name = TOOL_MAP.get(raw_name).copied().unwrap_or(raw_name).to_string();
                        let input = tc.get("args").cloned().unwrap_or(Value::Object(Map::new()));
                        let normalized_input = if mapped_name == "Bash" {
                            if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
                                let mut obj = Map::new();
                                obj.insert("command".into(), Value::String(cmd.to_string()));
                                Value::Object(obj)
                            } else { input.clone() }
                        } else { input };

                        let result_text = extract_tool_result(tc.get("result"));
                        let exit_code = tc.get("result")
                            .and_then(|r| r.as_array())
                            .and_then(|arr| arr.first())
                            .and_then(|first| first.get("functionResponse"))
                            .and_then(|fr| fr.get("response"))
                            .and_then(|resp| resp.get("exitCode"))
                            .and_then(|e| e.as_i64());
                        let is_error = tc.get("status").and_then(|s| s.as_str()) == Some("error")
                            || matches!(exit_code, Some(code) if code != 0);

                        blocks.push(AssistantBlock::tool_use(
                            ToolCall {
                                tool_use_id: tc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                name: mapped_name,
                                input: normalized_input,
                                result: result_text,
                                result_timestamp: tc.get("timestamp").and_then(|t| t.as_str()).map(|s| s.to_string()),
                                is_error,
                            },
                            ts.clone(),
                        ));
                    }
                }

                let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("").trim();
                if !content.is_empty() {
                    blocks.push(AssistantBlock::text(content.to_string(), ts.clone()));
                }
                continue;
            }
        }

        finalize(&mut turns, &mut turn_index, &mut user_text, &mut timestamp, &mut blocks);
        filter_empty_turns(turns)
    }
}

fn finalize(
    turns: &mut Vec<Turn>,
    turn_index: &mut u32,
    user_text: &mut String,
    timestamp: &mut String,
    blocks: &mut Vec<AssistantBlock>,
) {
    if user_text.is_empty() && blocks.is_empty() { return; }
    *turn_index += 1;
    turns.push(Turn {
        index: *turn_index,
        user_text: std::mem::take(user_text),
        blocks: std::mem::take(blocks),
        timestamp: std::mem::take(timestamp),
        system_events: Vec::new(),
    });
}

fn extract_tool_result(result: Option<&Value>) -> Option<String> {
    let result = result?;
    if let Value::String(s) = result { return Some(s.clone()); }
    let arr = result.as_array()?;
    let first = arr.first()?;
    let fr = first.get("functionResponse")?;
    let resp = fr.get("response")?;
    let output = resp.get("output").and_then(|v| v.as_str()).unwrap_or("");
    let error = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if output.is_empty() && !error.is_empty() && error != "(none)" {
        return Some(error.to_string());
    }
    if output.is_empty() { None } else { Some(output.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_single_json_session() {
        let text = r#"{"sessionId":"abc","messages":[]}"#;
        assert!(Gemini.detect_from_text(text));
        assert!(!Gemini.detect_from_text(r#"{"type":"user"}"#));
    }

    #[test]
    fn parses_user_then_gemini_message() {
        let text = r#"{"sessionId":"s1","messages":[
            {"type":"user","content":"hi","timestamp":"2025-06-01T10:00:00Z"},
            {"type":"gemini","content":"hello","timestamp":"2025-06-01T10:00:01Z"}
        ]}"#;
        let turns = Gemini.parse(text);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_text, "hi");
        assert_eq!(turns[0].blocks.len(), 1);
        assert_eq!(turns[0].blocks[0].text, "hello");
    }
}
