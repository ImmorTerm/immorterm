//! Codex CLI adapter — direct port of claude-replay's `codex.mjs`.
//!
//! Two formats supported:
//!   - Legacy: `event_msg{task_started|task_complete}` brackets + `response_item` payloads.
//!   - New: `thread.started` + `item.completed` with nested `item` objects.
//! Both map `apply_patch` into Edit/Write and `exec_command` into Bash so the
//! downstream vocabulary matches Claude's.

use crate::shared::filter_empty_turns;
use crate::turn::{AssistantBlock, ToolCall, Turn};
use crate::ConversationAdapter;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::HashMap;

pub struct Codex;

impl ConversationAdapter for Codex {
    fn tool_name(&self) -> &'static str { "codex" }

    fn detect(&self, obj: &Value) -> bool {
        let t = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if t == "session_meta" { return true; }
        if t == "thread.started" { return true; }
        if t == "item.completed" && obj.get("item").is_some() { return true; }
        false
    }

    fn parse(&self, text: &str) -> Vec<Turn> {
        let mut events: Vec<Value> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            if let Ok(v) = serde_json::from_str(trimmed) { events.push(v); }
        }

        let is_new = events.iter().any(|e| {
            let t = e.get("type").and_then(|v| v.as_str()).unwrap_or("");
            t == "thread.started" || t == "item.completed"
        });
        if is_new { return parse_new_format(&events); }
        parse_legacy(&events)
    }
}

fn extract_codex_user_text(text: &str) -> String {
    const MARKER: &str = "## My request for Codex:";
    if let Some(idx) = text.find(MARKER) {
        return text[idx + MARKER.len()..].trim().to_string();
    }
    const MARKER2: &str = "## My request for Codex";
    if let Some(idx2) = text.find(MARKER2) {
        let after = &text[idx2 + MARKER2.len()..];
        // Strip an optional leading colon + whitespace.
        let stripped = after.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
        return stripped.trim().to_string();
    }
    text.trim().to_string()
}

struct ParsedPatch {
    file_path: String,
    is_new: bool,
    old_string: String,
    new_string: String,
    content: String,
}

fn parse_codex_patch(patch_str: &str) -> ParsedPatch {
    let mut lines: Vec<&str> = patch_str.split('\n').collect();
    while matches!(lines.last(), Some(&"")) { lines.pop(); }

    let mut file_path = String::new();
    let mut is_new = false;
    let mut old_lines: Vec<String> = Vec::new();
    let mut new_lines: Vec<String> = Vec::new();

    for line in lines {
        if line.starts_with("*** Begin Patch") || line.starts_with("*** End Patch") { continue; }
        if let Some(rest) = line.strip_prefix("*** Add File:") {
            file_path = rest.trim().to_string();
            is_new = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("*** Update File:") {
            file_path = rest.trim().to_string();
            is_new = false;
            continue;
        }
        if line.starts_with("@@") { continue; }
        if let Some(rest) = line.strip_prefix('+') {
            new_lines.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix('-') {
            old_lines.push(rest.to_string());
        } else {
            old_lines.push(line.to_string());
            new_lines.push(line.to_string());
        }
    }

    if is_new {
        ParsedPatch {
            file_path,
            is_new: true,
            content: new_lines.join("\n"),
            old_string: String::new(),
            new_string: String::new(),
        }
    } else {
        ParsedPatch {
            file_path,
            is_new: false,
            content: String::new(),
            old_string: old_lines.join("\n"),
            new_string: new_lines.join("\n"),
        }
    }
}

fn patch_to_input(p: &ParsedPatch) -> Value {
    let mut obj = Map::new();
    obj.insert("file_path".into(), Value::String(p.file_path.clone()));
    if p.is_new {
        obj.insert("content".into(), Value::String(p.content.clone()));
        obj.insert("isNew".into(), Value::Bool(true));
    } else {
        obj.insert("old_string".into(), Value::String(p.old_string.clone()));
        obj.insert("new_string".into(), Value::String(p.new_string.clone()));
        obj.insert("isNew".into(), Value::Bool(false));
    }
    Value::Object(obj)
}

fn parse_new_format(events: &[Value]) -> Vec<Turn> {
    let mut blocks: Vec<AssistantBlock> = Vec::new();
    let mut user_text = String::new();

    for evt in events {
        if evt.get("type").and_then(|t| t.as_str()) != Some("item.completed") { continue; }
        let item = match evt.get("item") {
            Some(i) if i.is_object() => i,
            _ => continue,
        };
        let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let ts = evt.get("timestamp").and_then(|t| t.as_str()).map(|s| s.to_string());

        match item_type {
            "command_execution" => {
                let cmd_raw = item.get("command").cloned().unwrap_or(Value::Null);
                let cmd = match &cmd_raw {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                let clean = clean_bash_lc(&cmd);
                let mut input_map = Map::new();
                input_map.insert("command".into(), Value::String(clean));
                let aggregated = item.get("aggregated_output").and_then(|o| o.as_str()).unwrap_or("").trim();
                let exit = item.get("exit_code").and_then(|e| e.as_i64());
                let is_error = matches!(exit, Some(code) if code != 0);
                blocks.push(AssistantBlock::tool_use(
                    ToolCall {
                        tool_use_id: item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        name: "Bash".into(),
                        input: Value::Object(input_map),
                        result: Some(aggregated.to_string()),
                        result_timestamp: ts.clone(),
                        is_error,
                    },
                    ts,
                ));
            }
            "reasoning" => {
                let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if !text.trim().is_empty() {
                    blocks.push(AssistantBlock::thinking(text.to_string(), ts));
                }
            }
            "agent_message" => {
                let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if !text.trim().is_empty() {
                    blocks.push(AssistantBlock::text(text.to_string(), ts));
                }
            }
            "function_call" => {
                let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("unknown").to_string();
                let args_str = item.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                let mut input: Value = serde_json::from_str(args_str)
                    .unwrap_or_else(|_| {
                        let mut m = Map::new();
                        m.insert("raw".into(), Value::String(args_str.to_string()));
                        Value::Object(m)
                    });
                if name == "exec_command" {
                    if let Some(cmd) = input.get("cmd").and_then(|c| c.as_str()) {
                        let full = if let Some(wd) = input.get("workdir").and_then(|w| w.as_str()) {
                            format!("cd {} && {}", wd, cmd)
                        } else { cmd.to_string() };
                        let mut obj = Map::new();
                        obj.insert("command".into(), Value::String(full));
                        input = Value::Object(obj);
                    }
                }
                let mut mapped_name = name.clone();
                if name == "exec_command" { mapped_name = "Bash".into(); }
                if name == "apply_patch" {
                    let patch_src = item.get("arguments").and_then(|a| a.as_str()).map(|s| s.to_string())
                        .or_else(|| input.get("raw").and_then(|r| r.as_str()).map(|s| s.to_string()))
                        .unwrap_or_default();
                    let parsed = parse_codex_patch(&patch_src);
                    mapped_name = if parsed.is_new { "Write".into() } else { "Edit".into() };
                    input = patch_to_input(&parsed);
                }
                let output = item.get("output").and_then(|o| o.as_str()).unwrap_or("").trim();
                let result = if output.is_empty() { None } else { Some(output.to_string()) };
                let is_error = item.get("status").and_then(|s| s.as_str()) == Some("failed");
                blocks.push(AssistantBlock::tool_use(
                    ToolCall {
                        tool_use_id: item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        name: mapped_name,
                        input,
                        result,
                        result_timestamp: ts.clone(),
                        is_error,
                    },
                    ts,
                ));
            }
            "message" if item.get("role").and_then(|r| r.as_str()) == Some("user") => {
                if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                    let parts: Vec<String> = content.iter()
                        .filter_map(|b| {
                            if b.get("type")?.as_str()? == "input_text" {
                                Some(b.get("text")?.as_str()?.to_string())
                            } else { None }
                        })
                        .collect();
                    user_text = extract_codex_user_text(&parts.join("\n"));
                }
            }
            _ => {}
        }
    }

    if blocks.is_empty() { return Vec::new(); }
    let ut = if user_text.is_empty() { "Task".to_string() } else { user_text };
    vec![Turn {
        index: 1,
        user_text: ut,
        blocks,
        timestamp: String::new(),
        system_events: Vec::new(),
    }]
}

// Regex set for legacy function_call_output cleanup — matches the JS source.
static RE_CHUNK_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^Chunk ID:.*\n?").unwrap());
static RE_WALL_TIME: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^Wall time:.*\n?").unwrap());
static RE_EXIT_LINE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^Process exited with code \d+\n?").unwrap());
static RE_ORIG_TOKENS: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^Original token count:.*\n?").unwrap());
static RE_OUTPUT_HEADER: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^Output:\n?").unwrap());
static RE_BASH_LC: Lazy<Regex> = Lazy::new(|| Regex::new(r"^/bin/bash\s+-lc\s+").unwrap());

fn clean_bash_lc(cmd: &str) -> String {
    let no_shebang = RE_BASH_LC.replace(cmd, "").into_owned();
    let trimmed = no_shebang.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn parse_legacy(events: &[Value]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut turn_index: u32 = 0;
    let mut current_user_text = String::new();
    let mut current_timestamp = String::new();
    let mut current_blocks: Vec<AssistantBlock> = Vec::new();
    let mut pending: HashMap<String, usize> = HashMap::new();
    let mut in_turn = false;

    for evt in events {
        let etype = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = evt.get("payload").cloned().unwrap_or(Value::Object(Map::new()));
        let ts = evt.get("timestamp").and_then(|v| v.as_str()).map(|s| s.to_string());

        let ptype = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if etype == "event_msg" && ptype == "task_started" {
            in_turn = true;
            current_user_text.clear();
            current_timestamp = ts.clone().unwrap_or_default();
            current_blocks.clear();
            pending.clear();
            continue;
        }
        if etype == "event_msg" && ptype == "task_complete" {
            if in_turn {
                turn_index += 1;
                turns.push(Turn {
                    index: turn_index,
                    user_text: std::mem::take(&mut current_user_text),
                    blocks: std::mem::take(&mut current_blocks),
                    timestamp: std::mem::take(&mut current_timestamp),
                    system_events: Vec::new(),
                });
            }
            in_turn = false;
            continue;
        }
        if !in_turn { continue; }

        if etype == "event_msg" && ptype == "user_message" {
            let msg = payload.get("message").and_then(|m| m.as_str()).unwrap_or("");
            current_user_text = extract_codex_user_text(msg);
            if let Some(t) = &ts { current_timestamp = t.clone(); }
            continue;
        }

        if etype == "response_item" {
            let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let phase = payload.get("phase").and_then(|p| p.as_str()).unwrap_or("");

            if ptype == "message" && role == "user" {
                if let Some(content) = payload.get("content").and_then(|c| c.as_array()) {
                    let parts: Vec<String> = content.iter()
                        .filter_map(|b| {
                            if b.get("type")?.as_str()? == "input_text" {
                                Some(b.get("text")?.as_str()?.to_string())
                            } else { None }
                        })
                        .collect();
                    let raw = parts.join("\n");
                    let extracted = extract_codex_user_text(&raw);
                    if !extracted.is_empty() && current_user_text.is_empty() {
                        current_user_text = extracted;
                    }
                }
                continue;
            }
            if ptype == "message" && role == "developer" { continue; }
            if ptype == "message" && role == "assistant" {
                let mut text_parts: Vec<String> = Vec::new();
                if let Some(content) = payload.get("content").and_then(|c| c.as_array()) {
                    for b in content {
                        if b.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                    }
                }
                let block_text = text_parts.join("\n").trim().to_string();
                if block_text.is_empty() { continue; }
                let kind_is_thinking = phase == "commentary";
                if kind_is_thinking {
                    current_blocks.push(AssistantBlock::thinking(block_text, ts.clone()));
                } else {
                    current_blocks.push(AssistantBlock::text(block_text, ts.clone()));
                }
                continue;
            }
            if ptype == "reasoning" { continue; }

            if ptype == "function_call" {
                let call_id = payload.get("call_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let fn_name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                let args_str = payload.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                let mut input: Value = serde_json::from_str(args_str).unwrap_or_else(|_| {
                    let mut m = Map::new();
                    m.insert("raw".into(), Value::String(args_str.to_string()));
                    Value::Object(m)
                });
                if fn_name == "exec_command" {
                    if let Some(cmd) = input.get("cmd").and_then(|c| c.as_str()) {
                        let full = if let Some(wd) = input.get("workdir").and_then(|w| w.as_str()) {
                            format!("cd {} && {}", wd, cmd)
                        } else { cmd.to_string() };
                        let mut obj = Map::new();
                        obj.insert("command".into(), Value::String(full));
                        input = Value::Object(obj);
                    }
                }
                let mapped_name = if fn_name == "exec_command" { "Bash".to_string() } else { fn_name.clone() };
                current_blocks.push(AssistantBlock::tool_use(
                    ToolCall {
                        tool_use_id: call_id.clone(),
                        name: mapped_name,
                        input,
                        result: None,
                        result_timestamp: None,
                        is_error: false,
                    },
                    ts.clone(),
                ));
                pending.insert(call_id, current_blocks.len() - 1);
                continue;
            }

            if ptype == "function_call_output" {
                let call_id = payload.get("call_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let output = payload.get("output").and_then(|o| o.as_str()).unwrap_or("").to_string();
                let stripped = strip_codex_output_headers(&output);
                if let Some(&bidx) = pending.get(&call_id) {
                    if let Some(tc) = current_blocks[bidx].tool_call.as_mut() {
                        tc.result = Some(stripped.trim().to_string());
                        tc.result_timestamp = ts.clone();
                        tc.is_error = output.contains("Process exited with code")
                            && !output.contains("code 0");
                    }
                    pending.remove(&call_id);
                }
                continue;
            }

            if ptype == "custom_tool_call" {
                let call_id = payload.get("call_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let tool_name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                let (mapped_name, input) = if tool_name == "apply_patch" {
                    let src = payload.get("input").and_then(|v| v.as_str()).unwrap_or("");
                    let parsed = parse_codex_patch(src);
                    let name = if parsed.is_new { "Write".to_string() } else { "Edit".to_string() };
                    (name, patch_to_input(&parsed))
                } else {
                    let raw = payload.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let mut m = Map::new();
                    m.insert("raw".into(), Value::String(raw));
                    (tool_name.clone(), Value::Object(m))
                };
                current_blocks.push(AssistantBlock::tool_use(
                    ToolCall {
                        tool_use_id: call_id.clone(),
                        name: mapped_name,
                        input,
                        result: None,
                        result_timestamp: None,
                        is_error: false,
                    },
                    ts.clone(),
                ));
                pending.insert(call_id, current_blocks.len() - 1);
                continue;
            }

            if ptype == "custom_tool_call_output" {
                let call_id = payload.get("call_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let (output_text, exit_nonzero) = match payload.get("output") {
                    Some(Value::String(s)) => (s.clone(), false),
                    Some(Value::Object(o)) => {
                        let out = o.get("output").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let exit = o.get("metadata").and_then(|m| m.get("exit_code")).and_then(|e| e.as_i64());
                        (out, matches!(exit, Some(code) if code != 0))
                    }
                    _ => (String::new(), false),
                };
                if let Some(&bidx) = pending.get(&call_id) {
                    if let Some(tc) = current_blocks[bidx].tool_call.as_mut() {
                        tc.result = Some(output_text.trim().to_string());
                        tc.result_timestamp = ts.clone();
                        tc.is_error = exit_nonzero;
                    }
                    pending.remove(&call_id);
                }
                continue;
            }
        }
    }

    if in_turn && (!current_user_text.is_empty() || !current_blocks.is_empty()) {
        turn_index += 1;
        turns.push(Turn {
            index: turn_index,
            user_text: current_user_text,
            blocks: current_blocks,
            timestamp: current_timestamp,
            system_events: Vec::new(),
        });
    }

    filter_empty_turns(turns)
}

fn strip_codex_output_headers(output: &str) -> String {
    let s = RE_CHUNK_ID.replace_all(output, "").into_owned();
    let s = RE_WALL_TIME.replace_all(&s, "").into_owned();
    let s = RE_EXIT_LINE.replace_all(&s, "").into_owned();
    let s = RE_ORIG_TOKENS.replace_all(&s, "").into_owned();
    let s = RE_OUTPUT_HEADER.replace_all(&s, "").into_owned();
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_session_meta_and_thread_started() {
        let a: Value = serde_json::from_str(r#"{"type":"session_meta"}"#).unwrap();
        assert!(Codex.detect(&a));
        let b: Value = serde_json::from_str(r#"{"type":"thread.started"}"#).unwrap();
        assert!(Codex.detect(&b));
        let c: Value = serde_json::from_str(r#"{"type":"item.completed","item":{}}"#).unwrap();
        assert!(Codex.detect(&c));
        let d: Value = serde_json::from_str(r#"{"type":"item.completed"}"#).unwrap();
        assert!(!Codex.detect(&d));
    }

    #[test]
    fn extract_user_text_strips_marker() {
        let s = "## IDE context etc.\n## My request for Codex:\nactual prompt";
        assert_eq!(extract_codex_user_text(s), "actual prompt");
    }

    #[test]
    fn new_format_single_turn_from_item_completed() {
        let text = r#"{"type":"thread.started"}
{"type":"item.completed","item":{"type":"agent_message","text":"done"},"timestamp":"2025-06-01T10:00:00Z"}"#;
        let turns = Codex.parse(text);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].blocks[0].text, "done");
    }
}
