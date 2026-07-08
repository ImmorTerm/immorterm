//! OpenCode JSONL adapter — direct port of claude-replay's `opencode.mjs`.
//!
//! Events carry `sessionID` + `type` ∈ {step_start, step_finish, tool_use, text,
//! reasoning, error}. Turns are bounded by `step_finish{reason:"stop"}`.
//! Tool names are mapped into Claude-style for cross-vendor consistency.

use crate::shared::filter_empty_turns;
use crate::turn::{AssistantBlock, ToolCall, Turn};
use crate::ConversationAdapter;
use once_cell::sync::Lazy;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

pub struct OpenCode;

static VALID_TYPES: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut s = HashSet::new();
    for t in ["step_start", "step_finish", "tool_use", "text", "reasoning", "error"] {
        s.insert(t);
    }
    s
});

static TOOL_MAP: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("bash", "Bash");
    m.insert("read", "Read");
    m.insert("write", "Write");
    m.insert("edit", "Edit");
    m.insert("patch", "Edit");
    m.insert("glob", "Glob");
    m.insert("grep", "Grep");
    m.insert("ls", "Glob");
    m.insert("webfetch", "WebFetch");
    m.insert("websearch", "WebSearch");
    m.insert("codesearch", "Grep");
    m.insert("task", "Task");
    m.insert("todo", "TodoWrite");
    m
});

impl ConversationAdapter for OpenCode {
    fn tool_name(&self) -> &'static str { "opencode" }

    fn detect(&self, obj: &Value) -> bool {
        let has_session = obj.get("sessionID").is_some();
        let t = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        has_session && VALID_TYPES.contains(t)
    }

    fn parse(&self, text: &str) -> Vec<Turn> {
        let mut events: Vec<Value> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            if let Ok(v) = serde_json::from_str(trimmed) { events.push(v); }
        }

        let mut turns: Vec<Turn> = Vec::new();
        let mut turn_index: u32 = 0;
        let mut blocks: Vec<AssistantBlock> = Vec::new();
        let mut timestamp = String::new();

        for evt in &events {
            let etype = evt.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let part = evt.get("part").cloned().unwrap_or(Value::Object(Map::new()));
            let ts = epoch_ms_to_iso(evt.get("timestamp"));

            match etype {
                "step_start" => {
                    if timestamp.is_empty() {
                        if let Some(t) = &ts { timestamp = t.clone(); }
                    }
                }
                "tool_use" => {
                    let raw_name = part.get("tool").and_then(|s| s.as_str()).unwrap_or("unknown");
                    let mapped_name = TOOL_MAP.get(raw_name).copied().unwrap_or(raw_name).to_string();
                    let state = part.get("state").cloned().unwrap_or(Value::Object(Map::new()));
                    let input = state.get("input").cloned().unwrap_or(Value::Object(Map::new()));
                    let output = state.get("output").cloned().unwrap_or(Value::String(String::new()));

                    let is_error = state.get("status").and_then(|s| s.as_str()) == Some("error")
                        || state.get("metadata").and_then(|m| m.get("exit")).and_then(|e| e.as_i64())
                            .map(|e| e != 0).unwrap_or(false);
                    let result_ts = state.get("time").and_then(|t| t.get("end"))
                        .and_then(|t| t.as_i64().or_else(|| t.as_f64().map(|f| f as i64)))
                        .and_then(|ms| epoch_ms_value_to_iso(ms));

                    let normalized_input = normalize_opencode_input(&mapped_name, &input);
                    let result_text = match &output {
                        Value::String(s) => s.clone(),
                        _ => output.to_string(),
                    };

                    blocks.push(AssistantBlock::tool_use(
                        ToolCall {
                            tool_use_id: part.get("callID").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            name: mapped_name,
                            input: normalized_input,
                            result: Some(result_text),
                            result_timestamp: result_ts,
                            is_error,
                        },
                        ts.clone(),
                    ));
                }
                "reasoning" => {
                    let content = part.get("text").and_then(|t| t.as_str()).unwrap_or("").trim();
                    if !content.is_empty() {
                        blocks.push(AssistantBlock::thinking(content.to_string(), ts.clone()));
                    }
                }
                "text" => {
                    let content = part.get("text").and_then(|t| t.as_str()).unwrap_or("").trim();
                    if !content.is_empty() {
                        blocks.push(AssistantBlock::text(content.to_string(), ts.clone()));
                    }
                }
                "step_finish" => {
                    let reason = part.get("reason").and_then(|r| r.as_str()).unwrap_or("");
                    if reason == "stop" {
                        finalize(&mut turns, &mut turn_index, &mut blocks, &mut timestamp);
                    }
                }
                "error" => {
                    let err_data = evt.get("error").cloned().unwrap_or(Value::Null);
                    let err_msg = err_data.get("data").and_then(|d| d.get("message")).and_then(|m| m.as_str())
                        .or_else(|| err_data.get("name").and_then(|n| n.as_str()))
                        .unwrap_or("Unknown error");
                    blocks.push(AssistantBlock::text(format!("Error: {}", err_msg), ts.clone()));
                    finalize(&mut turns, &mut turn_index, &mut blocks, &mut timestamp);
                }
                _ => {}
            }
        }

        finalize(&mut turns, &mut turn_index, &mut blocks, &mut timestamp);

        // Re-index (JS does this before returning — filter_empty_turns re-indexes too).
        filter_empty_turns(turns)
    }
}

fn finalize(
    turns: &mut Vec<Turn>,
    turn_index: &mut u32,
    blocks: &mut Vec<AssistantBlock>,
    timestamp: &mut String,
) {
    if blocks.is_empty() { return; }
    *turn_index += 1;
    turns.push(Turn {
        index: *turn_index,
        user_text: String::new(),
        blocks: std::mem::take(blocks),
        timestamp: std::mem::take(timestamp),
        system_events: Vec::new(),
    });
}

fn normalize_opencode_input(mapped_name: &str, input: &Value) -> Value {
    if mapped_name == "Bash" {
        if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
            let mut obj = Map::new();
            let full = if let Some(wd) = input.get("workdir").and_then(|w| w.as_str()) {
                format!("cd {} && {}", wd, cmd)
            } else { cmd.to_string() };
            obj.insert("command".into(), Value::String(full));
            return Value::Object(obj);
        }
    }
    if mapped_name == "Write" {
        if let Some(fp) = input.get("filePath").and_then(|p| p.as_str()) {
            let mut obj = Map::new();
            obj.insert("file_path".into(), Value::String(fp.to_string()));
            obj.insert(
                "content".into(),
                input.get("content").cloned().unwrap_or(Value::String(String::new())),
            );
            return Value::Object(obj);
        }
    }
    if mapped_name == "Read" {
        if let Some(fp) = input.get("filePath").and_then(|p| p.as_str()) {
            let mut obj = Map::new();
            obj.insert("file_path".into(), Value::String(fp.to_string()));
            return Value::Object(obj);
        }
    }
    if mapped_name == "Edit" {
        if let Some(fp) = input.get("filePath").and_then(|p| p.as_str()) {
            if let Some(obj) = input.as_object() {
                let mut out = obj.clone();
                out.insert("file_path".into(), Value::String(fp.to_string()));
                return Value::Object(out);
            }
        }
    }
    input.clone()
}

fn epoch_ms_to_iso(v: Option<&Value>) -> Option<String> {
    let v = v?;
    let ms = v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))?;
    epoch_ms_value_to_iso(ms)
}

fn epoch_ms_value_to_iso(ms: i64) -> Option<String> {
    // Minimal epoch-ms → ISO-8601 UTC formatter. Mirrors `new Date(ms).toISOString()`.
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000) as u32;

    let days = secs.div_euclid(86400);
    let sod = secs.rem_euclid(86400) as u32;
    let (y, m, d) = civil_from_days(days);
    let hour = sod / 3600;
    let min = (sod % 3600) / 60;
    let sec = sod % 60;
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, hour, min, sec, millis
    ))
}

// Howard Hinnant's civil_from_days — inverse of days_from_civil in turn.rs.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_opencode_by_session_and_type() {
        let obj: Value = serde_json::from_str(r#"{"sessionID":"s","type":"text"}"#).unwrap();
        assert!(OpenCode.detect(&obj));
        let bad: Value = serde_json::from_str(r#"{"type":"text"}"#).unwrap();
        assert!(!OpenCode.detect(&bad));
    }

    #[test]
    fn step_finish_stop_bounds_turn() {
        let text = r#"{"sessionID":"s","type":"step_start","timestamp":1748772000000}
{"sessionID":"s","type":"text","part":{"text":"hi"}}
{"sessionID":"s","type":"step_finish","part":{"reason":"stop"}}
{"sessionID":"s","type":"step_start","timestamp":1748772001000}
{"sessionID":"s","type":"text","part":{"text":"again"}}
{"sessionID":"s","type":"step_finish","part":{"reason":"stop"}}"#;
        let turns = OpenCode.parse(text);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].blocks[0].text, "hi");
        assert_eq!(turns[1].blocks[0].text, "again");
    }
}
