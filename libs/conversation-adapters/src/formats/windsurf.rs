//! Windsurf Cascade adapter — parses `~/.windsurf/transcripts/{trajectory_id}.jsonl`.
//!
//! Cascade hook events (per docs.windsurf.com/windsurf/cascade/hooks):
//!   - `user_input`       — user prompt (`{type, status, content, timestamp, ...}`)
//!   - `planner_response` — assistant text/reasoning + optional `toolCalls[]`
//!   - `code_action`      — tool execution outcome (file edit, bash, ...)
//!
//! We emit one Turn per `user_input`. Subsequent `planner_response` /
//! `code_action` events fold into that turn's blocks until the next
//! `user_input`. Tool names are mapped into Claude's vocabulary (Bash, Edit,
//! Write, Read, Grep, Glob) so downstream consumers see the same shape across
//! vendors.

use crate::shared::filter_empty_turns;
use crate::turn::{AssistantBlock, ToolCall, Turn};
use crate::ConversationAdapter;
use once_cell::sync::Lazy;
use serde_json::{Map, Value};
use std::collections::HashMap;

pub struct Windsurf;

static TOOL_MAP: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("run_command", "Bash");
    m.insert("run_terminal_command", "Bash");
    m.insert("execute_command", "Bash");
    m.insert("edit_file", "Edit");
    m.insert("replace_file_content", "Edit");
    m.insert("write_file", "Write");
    m.insert("create_file", "Write");
    m.insert("view_file", "Read");
    m.insert("read_file", "Read");
    m.insert("grep_search", "Grep");
    m.insert("codebase_search", "Grep");
    m.insert("find_by_name", "Glob");
    m.insert("list_dir", "Glob");
    m.insert("browser_preview", "WebFetch");
    m.insert("web_search", "WebSearch");
    m
});

impl ConversationAdapter for Windsurf {
    fn tool_name(&self) -> &'static str { "windsurf" }

    fn detect(&self, obj: &Value) -> bool {
        let t = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        // Discriminator: any of Cascade's three known event types.
        // Status field is documented as required; presence is a strong signal,
        // but we don't gate on it (early prototypes may omit it).
        matches!(t, "user_input" | "planner_response" | "code_action")
    }

    fn parse(&self, text: &str) -> Vec<Turn> {
        let mut events: Vec<Value> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) { events.push(v); }
        }

        let mut turns: Vec<Turn> = Vec::new();
        let mut turn_index: u32 = 0;
        let mut current_user_text = String::new();
        let mut current_timestamp = String::new();
        let mut current_blocks: Vec<AssistantBlock> = Vec::new();
        let mut pending_tool: HashMap<String, usize> = HashMap::new();
        let mut have_turn = false;

        for evt in &events {
            let etype = evt.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let ts = evt.get("timestamp").and_then(|v| v.as_str()).map(|s| s.to_string());

            match etype {
                "user_input" => {
                    if have_turn {
                        turn_index += 1;
                        turns.push(Turn {
                            index: turn_index,
                            user_text: std::mem::take(&mut current_user_text),
                            blocks: std::mem::take(&mut current_blocks),
                            timestamp: std::mem::take(&mut current_timestamp),
                            system_events: Vec::new(),
                        });
                        pending_tool.clear();
                    }
                    let content = evt.get("content").and_then(|v| v.as_str())
                        .or_else(|| evt.get("text").and_then(|v| v.as_str()))
                        .or_else(|| evt.get("prompt").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    current_user_text = content;
                    current_timestamp = ts.unwrap_or_default();
                    have_turn = true;
                }
                "planner_response" => {
                    if !have_turn { have_turn = true; }
                    // Reasoning / thinking text.
                    if let Some(reasoning) = evt.get("reasoning").and_then(|v| v.as_str()) {
                        let trimmed = reasoning.trim();
                        if !trimmed.is_empty() {
                            current_blocks.push(AssistantBlock::thinking(trimmed.to_string(), ts.clone()));
                        }
                    }
                    // Visible response text.
                    let response_text = evt.get("content").and_then(|v| v.as_str())
                        .or_else(|| evt.get("text").and_then(|v| v.as_str()))
                        .or_else(|| evt.get("response").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .trim();
                    if !response_text.is_empty() {
                        current_blocks.push(AssistantBlock::text(response_text.to_string(), ts.clone()));
                    }
                    // Inline toolCalls[] (Cascade emits these alongside the response).
                    if let Some(calls) = evt.get("toolCalls").and_then(|v| v.as_array()) {
                        for tc in calls {
                            let raw_name = tc.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                            let mapped = TOOL_MAP.get(raw_name).copied().unwrap_or(raw_name).to_string();
                            let input = tc.get("args").cloned()
                                .or_else(|| tc.get("arguments").cloned())
                                .or_else(|| tc.get("input").cloned())
                                .unwrap_or(Value::Object(Map::new()));
                            let normalized = normalize_input(&mapped, &input);
                            let call_id = tc.get("id").and_then(|v| v.as_str())
                                .or_else(|| tc.get("callId").and_then(|v| v.as_str()))
                                .unwrap_or("")
                                .to_string();
                            current_blocks.push(AssistantBlock::tool_use(
                                ToolCall {
                                    tool_use_id: call_id.clone(),
                                    name: mapped,
                                    input: normalized,
                                    result: None,
                                    result_timestamp: None,
                                    is_error: false,
                                },
                                ts.clone(),
                            ));
                            if !call_id.is_empty() {
                                pending_tool.insert(call_id, current_blocks.len() - 1);
                            }
                        }
                    }
                }
                "code_action" => {
                    if !have_turn { have_turn = true; }
                    let info = evt.get("tool_info").cloned()
                        .or_else(|| evt.get("toolInfo").cloned())
                        .unwrap_or(Value::Object(Map::new()));
                    let raw_name = info.get("name").and_then(|v| v.as_str())
                        .or_else(|| evt.get("agent_action_name").and_then(|v| v.as_str()))
                        .or_else(|| evt.get("action").and_then(|v| v.as_str()))
                        .unwrap_or("unknown");
                    let mapped = TOOL_MAP.get(raw_name).copied().unwrap_or(raw_name).to_string();
                    let call_id = evt.get("tool_call_id").and_then(|v| v.as_str())
                        .or_else(|| info.get("id").and_then(|v| v.as_str()))
                        .or_else(|| info.get("callId").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let output = evt.get("output").and_then(|v| v.as_str())
                        .or_else(|| info.get("output").and_then(|v| v.as_str()))
                        .or_else(|| evt.get("result").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let status_s = evt.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    let exit_code = info.get("exit_code").and_then(|v| v.as_i64())
                        .or_else(|| evt.get("exit_code").and_then(|v| v.as_i64()));
                    let is_error = status_s == "error" || status_s == "failed"
                        || matches!(exit_code, Some(c) if c != 0);

                    if let Some(&bidx) = pending_tool.get(&call_id) {
                        if let Some(tc) = current_blocks[bidx].tool_call.as_mut() {
                            tc.result = Some(output);
                            tc.result_timestamp = ts.clone();
                            tc.is_error = is_error;
                        }
                        pending_tool.remove(&call_id);
                    } else {
                        // Standalone code_action without a prior tool_use.
                        let input = info.get("args").cloned()
                            .or_else(|| info.get("input").cloned())
                            .unwrap_or(Value::Object(Map::new()));
                        let normalized = normalize_input(&mapped, &input);
                        current_blocks.push(AssistantBlock::tool_use(
                            ToolCall {
                                tool_use_id: call_id,
                                name: mapped,
                                input: normalized,
                                result: if output.is_empty() { None } else { Some(output) },
                                result_timestamp: ts.clone(),
                                is_error,
                            },
                            ts.clone(),
                        ));
                    }
                }
                _ => {}
            }
        }

        if have_turn {
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
}

fn normalize_input(mapped_name: &str, input: &Value) -> Value {
    if mapped_name == "Bash" {
        if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
            let mut obj = Map::new();
            let full = if let Some(wd) = input.get("cwd").and_then(|w| w.as_str())
                .or_else(|| input.get("workdir").and_then(|w| w.as_str()))
            {
                format!("cd {} && {}", wd, cmd)
            } else {
                cmd.to_string()
            };
            obj.insert("command".into(), Value::String(full));
            return Value::Object(obj);
        }
    }
    if mapped_name == "Write" || mapped_name == "Edit" || mapped_name == "Read" {
        if let Some(obj_in) = input.as_object() {
            let mut out = obj_in.clone();
            if let Some(fp) = obj_in.get("path").and_then(|v| v.as_str())
                .or_else(|| obj_in.get("filePath").and_then(|v| v.as_str()))
                .or_else(|| obj_in.get("file_path").and_then(|v| v.as_str()))
            {
                out.insert("file_path".into(), Value::String(fp.to_string()));
            }
            return Value::Object(out);
        }
    }
    input.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_user_input_and_planner_response() {
        let a: Value = serde_json::from_str(r#"{"type":"user_input","content":"hi"}"#).unwrap();
        assert!(Windsurf.detect(&a));
        let b: Value = serde_json::from_str(r#"{"type":"planner_response","content":"ok"}"#).unwrap();
        assert!(Windsurf.detect(&b));
        let c: Value = serde_json::from_str(r#"{"type":"code_action","tool_info":{}}"#).unwrap();
        assert!(Windsurf.detect(&c));
        let d: Value = serde_json::from_str(r#"{"type":"user","message":{}}"#).unwrap();
        assert!(!Windsurf.detect(&d));
    }

    #[test]
    fn pairs_planner_tool_call_with_code_action_result() {
        let text = r#"{"type":"user_input","content":"list files","timestamp":"2026-04-01T10:00:00Z"}
{"type":"planner_response","content":"running ls","toolCalls":[{"id":"c1","name":"run_command","args":{"command":"ls"}}],"timestamp":"2026-04-01T10:00:01Z"}
{"type":"code_action","tool_call_id":"c1","status":"success","output":"a.txt\nb.txt","timestamp":"2026-04-01T10:00:02Z"}"#;
        let turns = Windsurf.parse(text);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_text, "list files");
        let bash = turns[0].blocks.iter().find(|b| b.tool_call.is_some()).unwrap();
        let tc = bash.tool_call.as_ref().unwrap();
        assert_eq!(tc.name, "Bash");
        assert_eq!(tc.input.get("command").and_then(|v| v.as_str()), Some("ls"));
        assert_eq!(tc.result.as_deref(), Some("a.txt\nb.txt"));
        assert!(!tc.is_error);
    }
}
