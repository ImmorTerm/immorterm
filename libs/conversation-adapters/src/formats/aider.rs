//! Aider markdown chat-history adapter.
//!
//! Aider persists conversation history at `<git_root>/.aider.chat.history.md`.
//! Format (markdown, not JSON):
//!
//!   # aider chat started at 2026-04-01 10:00:00
//!
//!   #### user message line 1
//!   #### user message line 2
//!
//!   > assistant response line 1
//!   > assistant response line 2
//!
//!   #### next user prompt
//!
//!   > next assistant reply
//!
//! Detection: a `# aider chat started at ...` header anywhere in the file.
//! Parsing: walk the lines, group consecutive `####` lines into a user block,
//! then group consecutive `>`-prefixed lines into the matching assistant block.
//! Anything outside those two prefixes is treated as a section break.

use crate::shared::filter_empty_turns;
use crate::turn::{AssistantBlock, Turn};
use crate::ConversationAdapter;
use once_cell::sync::Lazy;
use regex::Regex;

pub struct Aider;

static RE_SESSION_HEADER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^#\s+aider chat started at\s+(.*)$").unwrap()
});

impl ConversationAdapter for Aider {
    fn tool_name(&self) -> &'static str { "aider" }

    fn detect_from_text(&self, text: &str) -> bool {
        RE_SESSION_HEADER.is_match(text)
    }

    fn parse(&self, text: &str) -> Vec<Turn> {
        let mut turns: Vec<Turn> = Vec::new();
        let mut turn_index: u32 = 0;
        let mut current_user: Vec<String> = Vec::new();
        let mut current_assistant: Vec<String> = Vec::new();
        let mut current_timestamp = String::new();
        let mut last_session_header: String = String::new();
        let mut state: ParseState = ParseState::Idle;

        for raw in text.lines() {
            // Session header — captures timestamp for subsequent turns.
            if let Some(caps) = RE_SESSION_HEADER.captures(raw) {
                // Flush any in-flight turn before starting new session.
                flush_turn(
                    &mut turns,
                    &mut turn_index,
                    &mut current_user,
                    &mut current_assistant,
                    &mut current_timestamp,
                );
                last_session_header = caps.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
                state = ParseState::Idle;
                continue;
            }

            // User-prompt line: `#### message`.
            if let Some(rest) = raw.strip_prefix("####") {
                let text = rest.trim_start();
                // If we were in an assistant block, the upcoming user prompt
                // ends the prior turn.
                if matches!(state, ParseState::Assistant) {
                    flush_turn(
                        &mut turns,
                        &mut turn_index,
                        &mut current_user,
                        &mut current_assistant,
                        &mut current_timestamp,
                    );
                }
                if matches!(state, ParseState::Idle) {
                    current_timestamp = last_session_header.clone();
                }
                current_user.push(text.to_string());
                state = ParseState::User;
                continue;
            }

            // Assistant blockquote line: `> message` or `>` (empty quote).
            if raw.starts_with('>') {
                let stripped = if let Some(rest) = raw.strip_prefix("> ") {
                    rest.to_string()
                } else if let Some(rest) = raw.strip_prefix('>') {
                    rest.to_string()
                } else {
                    raw.to_string()
                };
                current_assistant.push(stripped);
                state = ParseState::Assistant;
                continue;
            }

            // Blank line within a block: keep paragraph spacing.
            if raw.trim().is_empty() {
                match state {
                    ParseState::User => current_user.push(String::new()),
                    ParseState::Assistant => current_assistant.push(String::new()),
                    ParseState::Idle => {}
                }
                continue;
            }

            // Anything else — non-header, non-####, non-> content. Treat as
            // continuation of the active block (Aider sometimes inlines
            // tool-output without the `>` prefix).
            match state {
                ParseState::User => current_user.push(raw.to_string()),
                ParseState::Assistant => current_assistant.push(raw.to_string()),
                ParseState::Idle => {}
            }
        }

        flush_turn(
            &mut turns,
            &mut turn_index,
            &mut current_user,
            &mut current_assistant,
            &mut current_timestamp,
        );

        filter_empty_turns(turns)
    }
}

#[derive(Clone, Copy)]
enum ParseState { Idle, User, Assistant }

fn flush_turn(
    turns: &mut Vec<Turn>,
    turn_index: &mut u32,
    user_lines: &mut Vec<String>,
    assistant_lines: &mut Vec<String>,
    timestamp: &mut String,
) {
    let user_text = collapse(user_lines.drain(..).collect());
    let assistant_text = collapse(assistant_lines.drain(..).collect());
    if user_text.is_empty() && assistant_text.is_empty() { return; }
    let mut blocks = Vec::new();
    if !assistant_text.is_empty() {
        blocks.push(AssistantBlock::text(assistant_text, None));
    }
    *turn_index += 1;
    turns.push(Turn {
        index: *turn_index,
        user_text,
        blocks,
        timestamp: std::mem::take(timestamp),
        system_events: Vec::new(),
    });
}

fn collapse(lines: Vec<String>) -> String {
    // Trim trailing blank lines, join with `\n`, then trim outer whitespace.
    let mut v = lines;
    while matches!(v.last(), Some(s) if s.trim().is_empty()) {
        v.pop();
    }
    while matches!(v.first(), Some(s) if s.trim().is_empty()) {
        v.remove(0);
    }
    v.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_aider_header() {
        let text = "# aider chat started at 2026-04-01 10:00:00\n\n#### hello\n\n> world";
        assert!(Aider.detect_from_text(text));
        assert!(!Aider.detect_from_text("# something else\n#### hi"));
    }

    #[test]
    fn parses_single_turn() {
        let text = "# aider chat started at 2026-04-01 10:00:00\n\n#### what is 2+2\n\n> 4\n";
        let turns = Aider.parse(text);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].user_text, "what is 2+2");
        assert_eq!(turns[0].blocks.len(), 1);
        assert_eq!(turns[0].blocks[0].text, "4");
        assert_eq!(turns[0].timestamp, "2026-04-01 10:00:00");
    }

    #[test]
    fn parses_two_alternating_turns() {
        let text = "# aider chat started at 2026-04-01 10:00:00\n\n#### add a fib function\n\n> Done. I added `fib()` in math.py.\n> See diff above.\n\n#### now run the tests\n\n> Tests passed.\n";
        let turns = Aider.parse(text);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user_text, "add a fib function");
        assert!(turns[0].blocks[0].text.contains("fib()"));
        assert_eq!(turns[1].user_text, "now run the tests");
        assert_eq!(turns[1].blocks[0].text, "Tests passed.");
    }
}
