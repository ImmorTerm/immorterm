//! Cross-check Rust port against claude-replay's JS fixtures.
//!
//! Expectations mirror `claude-replay/test/test-parser.mjs` — if a port drifts,
//! a fixture assertion fails with the exact turn it couldn't reproduce.

use conversation_adapters::{
    detect_format_from_text,
    formats::{
        aider::Aider, claude_code::ClaudeCode, cline::Cline, codex::Codex, cursor::Cursor,
        gemini::Gemini, opencode::OpenCode, windsurf::Windsurf,
    },
    parse_turns_from_text,
    turn::BlockKind,
    ConversationAdapter,
};

fn load(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path, e))
}

#[test]
fn claude_code_fixture_three_turns() {
    let text = load("fixture.jsonl");
    assert_eq!(detect_format_from_text(&text), "claude-code");
    let turns = ClaudeCode.parse(&text);
    assert_eq!(turns.len(), 3, "expected 3 turns in fixture.jsonl");
    assert_eq!(turns[0].user_text, "Hello, what is 2+2?");
    assert_eq!(turns[2].user_text, "Thanks!");
    assert_eq!(turns[0].timestamp, "2025-06-01T10:00:00Z");
}

#[test]
fn cursor_fixture_two_turns_and_thinking_reclassification() {
    let text = load("fixture-cursor.jsonl");
    assert_eq!(detect_format_from_text(&text), "cursor");
    let turns = Cursor.parse(&text);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].user_text, "scan for ble devices");
    assert_eq!(turns[1].user_text, "connect to the first one");
    assert_eq!(turns[0].blocks.len(), 2);
    assert!(turns[0].blocks[0].text.contains("Planning scan"));
    assert!(turns[0].blocks[1].text.contains("Found 3 devices"));
    assert_eq!(turns[0].blocks[0].kind, BlockKind::Thinking);
    assert_eq!(turns[0].blocks[1].kind, BlockKind::Text);
    assert_eq!(turns[1].blocks[0].kind, BlockKind::Text);
    assert_eq!(turns[0].timestamp, "");
}

#[test]
fn codex_fixture_three_turns_with_tool_normalization() {
    let text = load("fixture-codex.jsonl");
    assert_eq!(detect_format_from_text(&text), "codex");
    let turns = Codex.parse(&text);
    assert_eq!(turns.len(), 3, "expected 3 turns");
    assert_eq!(turns[0].user_text, "list files here");
    assert_eq!(turns[1].user_text, "create hello.txt");
    assert_eq!(turns[2].user_text, "fix the typo");

    // Turn 0: Bash tool with normalized command + stripped metadata
    let bash = turns[0].blocks.iter().find(|b| b.kind == BlockKind::ToolUse).expect("bash block");
    let tc = bash.tool_call.as_ref().unwrap();
    assert_eq!(tc.name, "Bash");
    assert_eq!(tc.input.get("command").and_then(|c| c.as_str()), Some("cd /tmp/test && ls"));
    assert_eq!(tc.result.as_deref(), Some("file1.txt\nfile2.txt"));
    assert!(!tc.result.as_deref().unwrap_or("").contains("Chunk ID"));

    // Turn 1: apply_patch Add File → Write
    let write = turns[1].blocks.iter().find(|b| b.kind == BlockKind::ToolUse).unwrap();
    let wtc = write.tool_call.as_ref().unwrap();
    assert_eq!(wtc.name, "Write");
    assert_eq!(wtc.input.get("file_path").and_then(|v| v.as_str()), Some("/tmp/hello.txt"));
    assert_eq!(wtc.input.get("content").and_then(|v| v.as_str()), Some("hello world"));

    // Turn 2: apply_patch Update File → Edit
    let edit = turns[2].blocks.iter().find(|b| b.kind == BlockKind::ToolUse).unwrap();
    let etc = edit.tool_call.as_ref().unwrap();
    assert_eq!(etc.name, "Edit");
    assert_eq!(etc.input.get("file_path").and_then(|v| v.as_str()), Some("/tmp/hello.txt"));
    assert_eq!(etc.input.get("old_string").and_then(|v| v.as_str()), Some("hello world"));
    assert_eq!(etc.input.get("new_string").and_then(|v| v.as_str()), Some("hello, world!"));
    assert!(turns[0].timestamp.starts_with("2026-03-13"));
}

#[test]
fn gemini_fixture_parses_as_single_object() {
    let text = load("fixture-gemini.json");
    assert_eq!(detect_format_from_text(&text), "gemini");
    let turns = Gemini.parse(&text);
    assert!(!turns.is_empty(), "gemini fixture should produce turns");
    // First turn should have a real user_text — Gemini strips system_tags.
    assert!(!turns[0].user_text.is_empty());
}

#[test]
fn opencode_fixture_two_turns() {
    let text = load("fixture-opencode.jsonl");
    assert_eq!(detect_format_from_text(&text), "opencode");
    let turns = OpenCode.parse(&text);
    assert_eq!(turns.len(), 2);
}

#[test]
fn windsurf_fixture_two_turns() {
    let text = load("fixture-windsurf.jsonl");
    assert_eq!(detect_format_from_text(&text), "windsurf");
    let turns = Windsurf.parse(&text);
    assert_eq!(turns.len(), 3, "expected 3 turns in fixture-windsurf.jsonl");
    assert_eq!(turns[0].user_text, "list files in the current directory");
    assert_eq!(turns[1].user_text, "create a hello.py file");
    assert_eq!(turns[2].user_text, "now run it");

    // Turn 0: planner_response thinking + text + tool_use mapped to Bash with cwd folded in.
    let bash = turns[0].blocks.iter().find(|b| b.tool_call.is_some()).expect("tool block");
    let tc = bash.tool_call.as_ref().unwrap();
    assert_eq!(tc.name, "Bash");
    assert_eq!(tc.input.get("command").and_then(|v| v.as_str()), Some("cd /tmp/demo && ls"));
    assert_eq!(tc.result.as_deref(), Some("alpha.txt\nbeta.txt\ngamma.md"));
    assert!(!tc.is_error);

    // Turn 1: write_file → Write with file_path normalized.
    let write = turns[1].blocks.iter().find(|b| b.tool_call.is_some()).unwrap();
    let wtc = write.tool_call.as_ref().unwrap();
    assert_eq!(wtc.name, "Write");
    assert_eq!(wtc.input.get("file_path").and_then(|v| v.as_str()), Some("/tmp/demo/hello.py"));

    // Reasoning from the first planner_response should appear as a thinking block.
    assert!(turns[0].blocks.iter().any(|b| b.kind == BlockKind::Thinking));
    assert_eq!(turns[0].timestamp, "2026-04-01T10:00:00Z");
}

#[test]
fn cline_fixture_two_turns() {
    let text = load("fixture-cline.jsonl");
    assert_eq!(detect_format_from_text(&text), "cline");
    let turns = Cline.parse(&text);
    assert_eq!(turns.len(), 2, "expected 2 turns in fixture-cline.jsonl");
    assert_eq!(turns[0].user_text, "add a fibonacci function to math.py");
    assert_eq!(turns[1].user_text, "now run the tests");

    // Turn 0: tool_use folds in the tool_result from the next pure-tool_result user message.
    let tool_block = turns[0].blocks.iter().find(|b| b.tool_call.is_some()).unwrap();
    let tc = tool_block.tool_call.as_ref().unwrap();
    assert_eq!(tc.name, "write_to_file");
    assert_eq!(tc.tool_use_id, "toolu_01");
    assert_eq!(tc.result.as_deref(), Some("File saved: math.py (98 bytes)"));

    // Turn 1: pytest tool_result attaches.
    let run = turns[1].blocks.iter().find(|b| b.tool_call.is_some()).unwrap();
    let rtc = run.tool_call.as_ref().unwrap();
    assert_eq!(rtc.name, "execute_command");
    assert_eq!(rtc.result.as_deref(), Some("3 passed in 0.05s"));
    // Timestamp is ISO-8601 derived from epoch ms.
    assert!(turns[0].timestamp.starts_with("2025-11-28") || turns[0].timestamp.starts_with("2025-12"),
        "unexpected ts: {}", turns[0].timestamp);
}

#[test]
fn aider_fixture_two_turns_markdown() {
    let text = load("fixture-aider.md");
    assert_eq!(detect_format_from_text(&text), "aider");
    let turns = Aider.parse(&text);
    assert_eq!(turns.len(), 2, "expected 2 turns in fixture-aider.md");
    assert_eq!(turns[0].user_text, "add a fibonacci function to math.py");
    assert_eq!(turns[1].user_text, "now write a quick test for it");
    // Assistant text body — should contain the code from the blockquote.
    assert!(turns[0].blocks[0].text.contains("def fib(n):"),
        "expected fib() in turn 0 assistant text, got: {}", turns[0].blocks[0].text);
    assert!(turns[1].blocks[0].text.contains("Tests pass."),
        "expected 'Tests pass.' in turn 1 assistant text, got: {}", turns[1].blocks[0].text);
    assert_eq!(turns[0].timestamp, "2026-04-01 10:00:00");
}

#[test]
fn system_tags_stripped_and_reminders_absorbed() {
    let text = load("fixture-system-tags.jsonl");
    let (turns, name) = parse_turns_from_text(&text);
    assert_eq!(name, "claude-code");
    assert!(!turns.is_empty());
    // No raw system-reminder wrapper should survive.
    for t in &turns {
        assert!(!t.user_text.contains("<system-reminder>"),
            "system-reminder should be stripped from user_text: {:?}", t.user_text);
        assert!(!t.user_text.contains("<user_query>"),
            "user_query wrapper should be stripped: {:?}", t.user_text);
    }
}
