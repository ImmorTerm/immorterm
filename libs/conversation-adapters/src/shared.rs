//! Shared parsing utilities — direct port of `claude-replay/src/formats/shared.mjs`.
//!
//! Keeping function names and algorithm shape close to the JS source so that
//! corrections in upstream can be replayed here mechanically.

use crate::turn::{AssistantBlock, BlockKind, ToolCall, Turn};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

static RE_TASK_NOTIF: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?s)<task-notification>\s*<task-id>[^<]*</task-id>\s*<output-file>[^<]*</output-file>\s*<status>([^<]*)</status>\s*<summary>([^<]*)</summary>\s*</task-notification>",
    ).unwrap()
});
static RE_OUTPUT_HINT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\n*Read the output file to retrieve the result:[^\n]*").unwrap()
});
static RE_USER_QUERY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<user_query>(.*?)</user_query>\s*").unwrap()
});
static RE_SYS_REMINDER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<system-reminder>.*?</system-reminder>\s*").unwrap()
});
static RE_IDE_OPENED: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<ide_opened_file>.*?</ide_opened_file>\s*").unwrap()
});
static RE_LOCAL_CAVEAT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<local-command-caveat>.*?</local-command-caveat>\s*").unwrap()
});
static RE_CMD_NAME: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<command-name>(.*?)</command-name>\s*").unwrap()
});
static RE_CMD_MESSAGE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<command-message>.*?</command-message>\s*").unwrap()
});
static RE_CMD_ARGS_EMPTY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"<command-args>\s*</command-args>\s*").unwrap()
});
static RE_CMD_ARGS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<command-args>(.*?)</command-args>\s*").unwrap()
});
static RE_LOCAL_STDOUT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<local-command-stdout>.*?</local-command-stdout>\s*").unwrap()
});

/// Strip system tags, IDE context, and command metadata from user text.
pub fn clean_system_tags(mut text: String) -> String {
    text = RE_TASK_NOTIF.replace_all(&text, |caps: &regex::Captures| {
        format!("[bg-task: {}]", &caps[2])
    }).into_owned();
    text = RE_OUTPUT_HINT.replace_all(&text, "").into_owned();
    text = RE_USER_QUERY.replace_all(&text, |caps: &regex::Captures| {
        caps[1].trim().to_string()
    }).into_owned();
    text = RE_SYS_REMINDER.replace_all(&text, "").into_owned();
    text = RE_IDE_OPENED.replace_all(&text, "").into_owned();
    text = RE_LOCAL_CAVEAT.replace_all(&text, "").into_owned();
    text = RE_CMD_NAME.replace_all(&text, |caps: &regex::Captures| {
        format!("{}\n", caps[1].trim())
    }).into_owned();
    text = RE_CMD_MESSAGE.replace_all(&text, "").into_owned();
    text = RE_CMD_ARGS_EMPTY.replace_all(&text, "").into_owned();
    text = RE_CMD_ARGS.replace_all(&text, |caps: &regex::Captures| {
        let t = caps[1].trim();
        if t.is_empty() { String::new() } else { format!("{}\n", t) }
    }).into_owned();
    text = RE_LOCAL_STDOUT.replace_all(&text, "").into_owned();
    text.trim().to_string()
}

/// Extract plain text from user message content (string or block array).
pub fn extract_text(content: &Value) -> String {
    match content {
        Value::String(s) => clean_system_tags(s.clone()),
        Value::Array(blocks) => {
            let parts: Vec<String> = blocks.iter()
                .filter_map(|b| {
                    if b.get("type")?.as_str()? == "text" {
                        Some(b.get("text")?.as_str()?.to_string())
                    } else { None }
                })
                .collect();
            clean_system_tags(parts.join("\n"))
        }
        _ => String::new(),
    }
}

/// Check if a user message contains only tool_result blocks.
pub fn is_tool_result_only(content: &Value) -> bool {
    match content {
        Value::Array(blocks) => !blocks.is_empty() && blocks.iter().all(|b| {
            b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
        }),
        _ => false,
    }
}

/// Collect all assistant content blocks starting from index `start`.
/// Returns (blocks, next_index).
pub fn collect_assistant_blocks(entries: &[Value], start: usize) -> (Vec<AssistantBlock>, usize) {
    let mut blocks = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut i = start;

    while i < entries.len() {
        let entry = &entries[i];
        let role = entry.get("message").and_then(|m| m.get("role")).and_then(|r| r.as_str())
            .or_else(|| entry.get("type").and_then(|t| t.as_str()));
        if role != Some("assistant") { break; }

        let ts = entry.get("timestamp").and_then(|t| t.as_str()).map(|s| s.to_string());
        if let Some(content) = entry.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
            for block in content {
                let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match btype {
                    "text" => {
                        let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("").trim().to_string();
                        if text.is_empty() || text == "No response requested." { continue; }
                        let key = format!("text:{}", text);
                        if !seen.insert(key) { continue; }
                        blocks.push(AssistantBlock::text(text, ts.clone()));
                    }
                    "thinking" => {
                        let text = block.get("thinking").and_then(|t| t.as_str()).unwrap_or("").trim().to_string();
                        if text.is_empty() { continue; }
                        let key = format!("thinking:{}", text);
                        if !seen.insert(key) { continue; }
                        blocks.push(AssistantBlock::thinking(text, ts.clone()));
                    }
                    "tool_use" => {
                        let tool_id = block.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let key = format!("tool_use:{}", tool_id);
                        if !seen.insert(key) { continue; }
                        let call = ToolCall {
                            tool_use_id: tool_id,
                            name: block.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            input: block.get("input").cloned().unwrap_or(Value::Object(Default::default())),
                            result: None,
                            result_timestamp: None,
                            is_error: false,
                        };
                        blocks.push(AssistantBlock::tool_use(call, ts.clone()));
                    }
                    _ => {}
                }
            }
        }
        i += 1;
    }
    (blocks, i)
}

/// Scan forward from `result_start` for user messages containing tool_result blocks.
/// Match them to tool_use blocks by tool_use_id.
pub fn attach_tool_results(blocks: &mut [AssistantBlock], entries: &[Value], result_start: usize) -> usize {
    let mut pending: HashMap<String, usize> = HashMap::new();
    for (idx, b) in blocks.iter().enumerate() {
        if b.kind == BlockKind::ToolUse {
            if let Some(tc) = &b.tool_call {
                pending.insert(tc.tool_use_id.clone(), idx);
            }
        }
    }
    if pending.is_empty() { return result_start; }

    let mut i = result_start;
    while i < entries.len() && !pending.is_empty() {
        let entry = &entries[i];
        let role = entry.get("message").and_then(|m| m.get("role")).and_then(|r| r.as_str())
            .or_else(|| entry.get("type").and_then(|t| t.as_str()));
        if role == Some("assistant") { break; }
        if role == Some("user") {
            let content = entry.get("message").and_then(|m| m.get("content"));
            if let Some(Value::Array(arr)) = content {
                let mut has_tool_result = false;
                for block in arr {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        has_tool_result = true;
                        let tid = block.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        if let Some(&bidx) = pending.get(&tid) {
                            let result_content = block.get("content").cloned().unwrap_or(Value::Null);
                            let mut result_text = match &result_content {
                                Value::Array(parts) => parts.iter()
                                    .filter_map(|p| {
                                        if p.get("type")?.as_str()? == "text" {
                                            Some(p.get("text")?.as_str()?.to_string())
                                        } else { None }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                Value::String(s) => s.clone(),
                                Value::Null => String::new(),
                                other => other.to_string(),
                            };
                            // strip <tool_use_error> wrapper
                            if result_text.starts_with("<tool_use_error>") && result_text.ends_with("</tool_use_error>") {
                                result_text = result_text["<tool_use_error>".len()..result_text.len()-"</tool_use_error>".len()].to_string();
                            }
                            let is_err = block.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                            if let Some(tc) = blocks[bidx].tool_call.as_mut() {
                                tc.result = Some(result_text);
                                tc.result_timestamp = entry.get("timestamp").and_then(|t| t.as_str()).map(|s| s.to_string());
                                tc.is_error = is_err;
                            }
                            pending.remove(&tid);
                        }
                    }
                }
                if !has_tool_result { break; }
            } else {
                break;
            }
        }
        i += 1;
    }
    i
}

static RE_BG_TASK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\[bg-task:\s*(.+?)\]").unwrap()
});

/// Build turns from normalized JSONL entries (Claude Code shape).
/// Shared by claude-code and cursor since both use the same
/// user→assistant→tool_result entry pattern.
pub fn build_turns_from_entries(entries: &[Value]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut i = 0usize;
    let mut turn_index = 0u32;

    while i < entries.len() {
        let entry = &entries[i];
        let role = entry.get("message").and_then(|m| m.get("role")).and_then(|r| r.as_str())
            .or_else(|| entry.get("type").and_then(|t| t.as_str()));

        if role == Some("user") {
            let content = entry.get("message").and_then(|m| m.get("content")).cloned().unwrap_or(Value::String(String::new()));
            if is_tool_result_only(&content) { i += 1; continue; }
            let mut user_text = extract_text(&content);
            let timestamp = entry.get("timestamp").and_then(|t| t.as_str()).unwrap_or("").to_string();
            i += 1;

            // Absorb consecutive non-tool-result user messages
            while i < entries.len() {
                let next = &entries[i];
                let next_role = next.get("message").and_then(|m| m.get("role")).and_then(|r| r.as_str())
                    .or_else(|| next.get("type").and_then(|t| t.as_str()));
                if next_role != Some("user") { break; }
                let next_content = next.get("message").and_then(|m| m.get("content")).cloned().unwrap_or(Value::String(String::new()));
                if is_tool_result_only(&next_content) { break; }
                let next_text = extract_text(&next_content);
                if !next_text.is_empty() {
                    if user_text.is_empty() { user_text = next_text; }
                    else { user_text = format!("{}\n{}", user_text, next_text); }
                }
                i += 1;
            }

            // Extract [bg-task: …] system events
            let mut system_events = Vec::new();
            let replaced = RE_BG_TASK.replace_all(&user_text, |caps: &regex::Captures| {
                system_events.push(caps[1].trim().to_string());
                String::new()
            });
            user_text = replaced.trim().to_string();

            let (mut blocks, next_i) = collect_assistant_blocks(entries, i);
            i = next_i;
            i = attach_tool_results(&mut blocks, entries, i);

            turn_index += 1;
            let mut turn = Turn { index: turn_index, user_text, blocks, timestamp, system_events: Vec::new() };
            if !system_events.is_empty() { turn.system_events = system_events; }
            turns.push(turn);
        } else if role == Some("assistant") {
            let (mut blocks, next_i) = collect_assistant_blocks(entries, i);
            i = next_i;
            i = attach_tool_results(&mut blocks, entries, i);
            if let Some(last) = turns.last_mut() {
                last.blocks.extend(blocks);
            } else {
                turn_index += 1;
                let ts = entry.get("timestamp").and_then(|t| t.as_str()).unwrap_or("").to_string();
                turns.push(Turn { index: turn_index, user_text: String::new(), blocks, timestamp: ts, system_events: Vec::new() });
            }
        } else {
            i += 1;
        }
    }
    filter_empty_turns(turns)
}

/// Filter empty turns and re-index sequentially.
pub fn filter_empty_turns(turns: Vec<Turn>) -> Vec<Turn> {
    let mut out: Vec<Turn> = turns.into_iter().filter(|t| {
        if !t.user_text.is_empty() { return true; }
        if !t.system_events.is_empty() { return true; }
        t.blocks.iter().any(|b| match b.kind {
            BlockKind::ToolUse => true,
            BlockKind::Text => !b.text.is_empty() && b.text != "No response requested.",
            BlockKind::Thinking => !b.text.is_empty(),
        })
    }).collect();
    for (j, t) in out.iter_mut().enumerate() {
        t.index = (j + 1) as u32;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_strips_system_reminder() {
        let got = clean_system_tags("<system-reminder>hi</system-reminder>\n\nhello".into());
        assert_eq!(got, "hello");
    }

    #[test]
    fn clean_strips_user_query_wrapper() {
        let got = clean_system_tags("<user_query>what is 2+2</user_query>".into());
        assert_eq!(got, "what is 2+2");
    }

    #[test]
    fn is_tool_result_only_detects() {
        let v = json!([{ "type": "tool_result", "tool_use_id": "t1", "content": "ok" }]);
        assert!(is_tool_result_only(&v));
        let mixed = json!([
            { "type": "tool_result", "tool_use_id": "t1", "content": "ok" },
            { "type": "text", "text": "hi" }
        ]);
        assert!(!is_tool_result_only(&mixed));
    }
}
