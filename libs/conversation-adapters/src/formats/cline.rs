//! Cline (VS Code extension) adapter.
//!
//! Cline persists per-task transcripts at:
//!   ~/Library/Application Support/Code/User/globalStorage/
//!     saoudrizwan.claude-dev/tasks/<task_id>/api_conversation_history.json
//! (macOS path; Linux/Windows resolution is deferred to runtime callers — Phase A
//! hardcodes the macOS layout and surfaces the assumption here.)
//!
//! On disk the file is a single JSON array of Anthropic-style messages:
//!   [ {role: "user"|"assistant", content: <string|blocks[]>, ts?: <ms>}, ... ]
//!
//! We accept either form on input:
//!   1. JSON-array text  → flatten to one message per logical line
//!   2. JSONL (one message per line) — the form a hook may stream live
//!
//! The detect() probe runs against the first parsed line. Cline messages carry
//! a numeric `ts` (milliseconds since epoch) and lack a top-level `type` —
//! that combination distinguishes them from Claude Code (`type` field) and
//! Cursor (no `ts`). When the on-disk array form is encountered, the
//! whole-text adapter pass elsewhere falls through; we additionally accept the
//! array via `parse()` (best-effort) so callers don't have to pre-flatten.

use crate::shared::{filter_empty_turns, clean_system_tags};
use crate::turn::{AssistantBlock, ToolCall, Turn};
use crate::ConversationAdapter;
use serde_json::{Map, Value};
use std::collections::HashMap;

pub struct Cline;

impl ConversationAdapter for Cline {
    fn tool_name(&self) -> &'static str { "cline" }

    fn detect(&self, obj: &Value) -> bool {
        // Reject anything that looks like Claude Code (`type` field present).
        if obj.get("type").is_some() { return false; }
        // Cline messages always carry numeric `ts` (ms since epoch).
        let has_ts = obj.get("ts").map(|v| v.is_number()).unwrap_or(false);
        if !has_ts { return false; }
        let role = obj.get("role").and_then(|r| r.as_str());
        matches!(role, Some("user") | Some("assistant"))
    }

    fn parse(&self, text: &str) -> Vec<Turn> {
        // Try array form first (api_conversation_history.json).
        let trimmed = text.trim_start();
        let entries: Vec<Value> = if trimmed.starts_with('[') {
            match serde_json::from_str::<Value>(trimmed) {
                Ok(Value::Array(arr)) => arr,
                _ => return Vec::new(),
            }
        } else {
            // JSONL form — one message per line.
            let mut out = Vec::new();
            for line in text.lines() {
                let t = line.trim();
                if t.is_empty() { continue; }
                if let Ok(v) = serde_json::from_str::<Value>(t) {
                    out.push(v);
                }
            }
            out
        };

        let mut turns: Vec<Turn> = Vec::new();
        let mut turn_index: u32 = 0;
        let mut current_user_text = String::new();
        let mut current_timestamp = String::new();
        let mut current_blocks: Vec<AssistantBlock> = Vec::new();
        let mut pending_tool: HashMap<String, usize> = HashMap::new();
        let mut have_turn = false;

        for entry in &entries {
            let role = entry.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let ts_iso = epoch_ms_to_iso(entry.get("ts"));

            if role == "user" {
                let content = entry.get("content").cloned().unwrap_or(Value::Null);
                if is_pure_tool_result(&content) {
                    // Attach tool_results to pending tool_use blocks; do NOT start a turn.
                    if let Value::Array(blocks) = &content {
                        for b in blocks {
                            if b.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                                continue;
                            }
                            let tid = b.get("tool_use_id").and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let result_text = stringify_tool_result_content(b.get("content"));
                            let is_err = b.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                            if let Some(&bidx) = pending_tool.get(&tid) {
                                if let Some(tc) = current_blocks[bidx].tool_call.as_mut() {
                                    tc.result = Some(result_text);
                                    tc.result_timestamp = ts_iso.clone();
                                    tc.is_error = is_err;
                                }
                                pending_tool.remove(&tid);
                            }
                        }
                    }
                    continue;
                }

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

                current_user_text = clean_system_tags(extract_user_text(&content));
                current_timestamp = ts_iso.unwrap_or_default();
                have_turn = true;
                continue;
            }

            if role == "assistant" {
                if !have_turn { have_turn = true; }
                let content = entry.get("content").cloned().unwrap_or(Value::Null);
                let blocks_arr = match content {
                    Value::Array(arr) => arr,
                    Value::String(s) => {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            current_blocks.push(AssistantBlock::text(trimmed.to_string(), ts_iso.clone()));
                        }
                        continue;
                    }
                    _ => continue,
                };
                for block in &blocks_arr {
                    let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match btype {
                        "text" => {
                            let txt = block.get("text").and_then(|t| t.as_str()).unwrap_or("").trim();
                            if !txt.is_empty() {
                                current_blocks.push(AssistantBlock::text(txt.to_string(), ts_iso.clone()));
                            }
                        }
                        "thinking" => {
                            let txt = block.get("thinking").and_then(|t| t.as_str()).unwrap_or("").trim();
                            if !txt.is_empty() {
                                current_blocks.push(AssistantBlock::thinking(txt.to_string(), ts_iso.clone()));
                            }
                        }
                        "tool_use" => {
                            let id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let input = block.get("input").cloned().unwrap_or(Value::Object(Map::new()));
                            current_blocks.push(AssistantBlock::tool_use(
                                ToolCall {
                                    tool_use_id: id.clone(),
                                    name,
                                    input,
                                    result: None,
                                    result_timestamp: None,
                                    is_error: false,
                                },
                                ts_iso.clone(),
                            ));
                            if !id.is_empty() {
                                pending_tool.insert(id, current_blocks.len() - 1);
                            }
                        }
                        _ => {}
                    }
                }
                continue;
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

fn extract_user_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let parts: Vec<String> = blocks.iter()
                .filter_map(|b| {
                    if b.get("type")?.as_str()? == "text" {
                        Some(b.get("text")?.as_str()?.to_string())
                    } else { None }
                })
                .collect();
            parts.join("\n")
        }
        _ => String::new(),
    }
}

fn is_pure_tool_result(content: &Value) -> bool {
    match content {
        Value::Array(blocks) => !blocks.is_empty() && blocks.iter().all(|b| {
            b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
        }),
        _ => false,
    }
}

fn stringify_tool_result_content(content: Option<&Value>) -> String {
    let Some(c) = content else { return String::new(); };
    match c {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter()
            .filter_map(|p| {
                if p.get("type")?.as_str()? == "text" {
                    Some(p.get("text")?.as_str()?.to_string())
                } else { None }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn epoch_ms_to_iso(v: Option<&Value>) -> Option<String> {
    let v = v?;
    let ms = v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))?;
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
    fn detects_cline_by_ts_and_role() {
        let a: Value = serde_json::from_str(r#"{"role":"user","ts":1700000000000,"content":"hi"}"#).unwrap();
        assert!(Cline.detect(&a));
        // No ts → not Cline.
        let b: Value = serde_json::from_str(r#"{"role":"user","content":"hi"}"#).unwrap();
        assert!(!Cline.detect(&b));
        // type field → Claude Code.
        let c: Value = serde_json::from_str(r#"{"type":"user","ts":1,"role":"user"}"#).unwrap();
        assert!(!Cline.detect(&c));
    }

    #[test]
    fn pairs_tool_use_with_pure_tool_result_user() {
        let text = r#"{"role":"user","ts":1700000000000,"content":"run ls"}
{"role":"assistant","ts":1700000001000,"content":[{"type":"tool_use","id":"t1","name":"execute_command","input":{"command":"ls"}}]}
{"role":"user","ts":1700000002000,"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"a\nb"}]}]}"#;
        let turns = Cline.parse(text);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_text, "run ls");
        let tc = turns[0].blocks[0].tool_call.as_ref().unwrap();
        assert_eq!(tc.name, "execute_command");
        assert_eq!(tc.result.as_deref(), Some("a\nb"));
    }
}
