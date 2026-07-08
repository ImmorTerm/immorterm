//! AI conversation extraction from terminal grid snapshots.
//!
//! Detects AI tool sessions and extracts structured conversation turns
//! by diffing consecutive grid snapshots. Writes to `.ai.jsonl` and
//! optionally pushes events via a callback trait.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;

use regex::Regex;
use serde::Serialize;
use tracing::{info, warn};

use immorterm_core::log::{strip_runs, GridSnapshot};

/// Role of a single line within AI conversation output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineRole {
    User,
    Assistant,
    None,
}

/// Known AI coding tools.
///
/// Phase A vendors are: Claude, Codex, Cursor, Windsurf, Cline, Opencode,
/// Gemini, Aider, Copilot. Continue / Cody pre-date Phase A and are kept
/// for back-compat (their TUIs still surface in ai.jsonl when detected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiTool {
    Claude,
    Aider,
    Cursor,
    Copilot,
    Codex,
    Windsurf,
    Cline,
    Opencode,
    Gemini,
    Continue,
    Cody,
    Unknown,
}

impl AiTool {
    /// Human-readable name for this tool. Matches the `tool` string written
    /// by hub session-link, daemon classify_ai_process, and the registry
    /// `tool` field — DRY across all three call sites.
    pub fn name(&self) -> &'static str {
        match self {
            AiTool::Claude => "claude",
            AiTool::Aider => "aider",
            AiTool::Cursor => "cursor",
            AiTool::Copilot => "copilot",
            AiTool::Codex => "codex",
            AiTool::Windsurf => "windsurf",
            AiTool::Cline => "cline",
            AiTool::Opencode => "opencode",
            AiTool::Gemini => "gemini",
            AiTool::Continue => "continue",
            AiTool::Cody => "cody",
            AiTool::Unknown => "unknown",
        }
    }
}

/// An extracted `<<html>>...<<\/html>>` block from AI output.
#[derive(Debug, Clone, Serialize)]
pub struct HtmlBlock {
    /// Zero-based index of this block within the turn.
    pub index: usize,
    /// The raw HTML content between the markers.
    pub html: String,
}

/// AI conversation event written to `.ai.jsonl`.
#[derive(Debug, Serialize)]
pub struct AiEvent {
    pub v: u8,
    pub ts: f64,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Cleaned content with `<<html>>` blocks stripped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Original content with `<<html>>` blocks preserved.
    /// Only present when the turn contained HTML blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_raw: Option<String>,
    /// Extracted HTML blocks for interactive replay.
    /// Only present when the turn contained HTML blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html_blocks: Option<Vec<HtmlBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_visible: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// Callback trait for AI event notification.
///
/// The Rust daemon implements this to forward events to OpenMemory.
/// The C binary passes `None` (events are handled by hooks instead).
pub trait LogEventSink: Send {
    fn on_event(&mut self, event: &AiEvent);
}

/// Tracks AI conversation state and extracts turns from grid diffs.
pub struct AiExtractor {
    _session_name: String,
    writer: Option<BufWriter<File>>,
    current_tool: Option<AiTool>,
    current_pid: Option<u32>,
    ai_start_ts: Option<f64>,
    prev_text: Vec<String>,
    pending_lines: Vec<String>,
    event_sink: Option<Box<dyn LogEventSink>>,
    /// Last user prompt text (cleaned, truncated to 200 chars).
    /// Updated each time a "user" turn is flushed.
    last_user_prompt: Option<String>,
}

impl AiExtractor {
    /// Create a new AI extractor.
    ///
    /// - `session_name`: Used for filenames
    /// - `log_dir`: Directory for `.ai.jsonl` output
    /// - `event_sink`: Optional callback for event notification (e.g., OpenMemory push)
    pub fn new(
        session_name: &str,
        log_dir: &Path,
        event_sink: Option<Box<dyn LogEventSink>>,
    ) -> Self {
        let ai_path = log_dir.join("ai.jsonl");
        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&ai_path)
            .map(BufWriter::new)
            .map_err(|e| warn!("Failed to open AI log {:?}: {}", ai_path, e))
            .ok();

        Self {
            _session_name: session_name.to_string(),
            writer,
            current_tool: None,
            current_pid: None,
            ai_start_ts: None,
            prev_text: Vec::new(),
            pending_lines: Vec::new(),
            event_sink,
            last_user_prompt: None,
        }
    }

    /// Called when AI tool state changes (detected or exited).
    pub fn on_ai_state_change(
        &mut self,
        tool: Option<AiTool>,
        pid: Option<u32>,
        transcript_path: Option<&str>,
        cost_usd: Option<f64>,
    ) {
        let now = now_ts();

        match (self.current_tool, tool) {
            (None, Some(new_tool)) => {
                // AI just started
                self.current_tool = Some(new_tool);
                self.current_pid = pid;
                self.ai_start_ts = Some(now);
                self.prev_text.clear();
                self.pending_lines.clear();

                let event = AiEvent {
                    v: 1,
                    ts: now,
                    event: "ai_detected".to_string(),
                    tool: Some(new_tool.name().to_string()),
                    pid,
                    role: None,
                    content: None,
                    content_raw: None,
                    html_blocks: None,
                    tools_visible: None,
                    transcript_path: transcript_path.map(|s| s.to_string()),
                    duration_s: None,
                    cost_usd: None,
                };
                self.write_event(&event);
                info!("AI detected: {} (PID {:?})", new_tool.name(), pid);
            }
            (Some(old_tool), None) => {
                // AI just exited — flush pending content first
                self.flush_pending_turn();

                let duration = self.ai_start_ts.map(|start| now - start);
                let event = AiEvent {
                    v: 1,
                    ts: now,
                    event: "ai_exited".to_string(),
                    tool: Some(old_tool.name().to_string()),
                    pid: self.current_pid,
                    role: None,
                    content: None,
                    content_raw: None,
                    html_blocks: None,
                    tools_visible: None,
                    transcript_path: transcript_path.map(|s| s.to_string()),
                    duration_s: duration,
                    cost_usd,
                };
                self.write_event(&event);
                info!(
                    "AI exited: {} (duration: {:.1}s)",
                    old_tool.name(),
                    duration.unwrap_or(0.0)
                );

                self.current_tool = None;
                self.current_pid = None;
                self.ai_start_ts = None;
                self.prev_text.clear();
                self.pending_lines.clear();
            }
            _ => {
                // No change or same tool still running
            }
        }
    }

    /// Called after each grid snapshot — diffs to extract new content.
    pub fn on_snapshot(&mut self, snapshot: &GridSnapshot) {
        if self.current_tool.is_none() {
            return;
        }

        let current_text: Vec<String> = snapshot
            .grid
            .iter()
            .map(|row_runs| strip_runs(&row_runs.runs))
            .collect();

        if !self.prev_text.is_empty() {
            let new_lines: Vec<String> = current_text
                .iter()
                .filter(|line| {
                    let trimmed = line.trim();
                    !trimmed.is_empty() && !self.prev_text.iter().any(|p| p.trim() == trimmed)
                })
                .cloned()
                .collect();

            if !new_lines.is_empty() {
                // Eagerly capture user prompts for status bar tooltip
                // (don't wait for the 5-line flush threshold)
                for line in &new_lines {
                    if self.classify_line(line) == LineRole::User {
                        self.last_user_prompt = Some(clean_user_prompt(line.trim()));
                        break;
                    }
                }
                self.pending_lines.extend(new_lines);
            }
        }

        // Flush when we accumulate 5+ lines
        if self.pending_lines.len() >= 5 {
            self.flush_pending_turn();
        }

        self.prev_text = current_text;
    }

    /// Flush accumulated lines as conversation turns.
    ///
    /// Lines are split at prompt boundaries (❯, $, >) so that each user prompt
    /// and each assistant response becomes its own turn — rather than lumping
    /// multiple exchanges into one giant turn.
    ///
    /// If the content contains `<<html>>...<<\/html>>` blocks, the turn is
    /// stored in three forms:
    /// - `content` — stripped (HTML blocks removed, clean text)
    /// - `content_raw` — original with markers preserved
    /// - `html_blocks` — extracted HTML for interactive replay
    fn flush_pending_turn(&mut self) {
        if self.pending_lines.is_empty() {
            return;
        }

        // Split pending lines into individual turns at prompt boundaries.
        // A prompt boundary is a line starting with ❯, $, or > (user prompt).
        // Each such line starts a "user" chunk; everything before it (if any)
        // is an "assistant" chunk.
        let chunks = self.split_into_turns();

        for chunk in chunks {
            if chunk.is_empty() {
                continue;
            }

            let raw = chunk.join("\n");
            let role = self.classify_role(&raw);

            // Extract <<html>>...<</html>> blocks
            let (content, content_raw, html_blocks) = extract_html_blocks(&raw);

            // Skip empty/whitespace-only content
            if content.trim().is_empty() {
                continue;
            }

            // Capture user prompts for status bar tooltip
            if role == "user" {
                self.last_user_prompt = Some(clean_user_prompt(&content));
            }

            let event = AiEvent {
                v: 1,
                ts: now_ts(),
                event: "turn".to_string(),
                tool: self.current_tool.map(|t| t.name().to_string()),
                pid: self.current_pid,
                role: Some(role),
                content: Some(content),
                content_raw,
                html_blocks,
                tools_visible: None,
                transcript_path: None,
                duration_s: None,
                cost_usd: None,
            };
            self.write_event(&event);
        }
        self.pending_lines.clear();
    }

    /// Split pending_lines into turn-sized chunks at role boundaries.
    ///
    /// Uses tool-specific markers to detect transitions between user and
    /// assistant. Each chunk becomes one turn event in ai.jsonl.
    fn split_into_turns(&self) -> Vec<Vec<String>> {
        let mut chunks: Vec<Vec<String>> = Vec::new();
        let mut current: Vec<String> = Vec::new();

        for line in &self.pending_lines {
            let role = self.classify_line(line);

            // Split on every definitive role marker — not just role transitions.
            // Each ❯ line is a new user prompt, even if the previous chunk was
            // also user (e.g., consecutive prompts with tool output between them
            // that was classified as None).
            if role != LineRole::None && !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }

            current.push(line.clone());
        }

        if !current.is_empty() {
            chunks.push(current);
        }

        chunks
    }

    /// Classify a single line's role based on tool-specific markers.
    fn classify_line(&self, line: &str) -> LineRole {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return LineRole::None;
        }

        match self.current_tool {
            Some(AiTool::Claude) => {
                // ❯ = shell prompt (user typed something)
                // Only classify as User if there's actual content after the marker —
                // a bare `❯` is chrome (the empty prompt line that Claude Code
                // renders alongside status rows like `⏵⏵ accept edits on`), not a
                // real prompt. Treating bare `❯` as User was causing entire screen
                // snapshots of UI chrome to be written as user turns.
                if trimmed.starts_with('\u{276F}') {
                    let rest = trimmed.trim_start_matches('\u{276F}').trim();
                    if rest.is_empty() {
                        return LineRole::None;
                    }
                    return LineRole::User;
                }
                // ⏺ (U+23FA RECORD BUTTON) = Claude response marker.
                // U+25CF (BLACK CIRCLE) also included for older captures.
                // · (U+00B7 MIDDLE DOT) = Claude thinking/working indicator.
                if trimmed.starts_with('\u{23FA}')
                    || trimmed.starts_with('\u{25CF}')
                    || trimmed.starts_with('\u{00B7}')
                {
                    return LineRole::Assistant;
                }
                // ⎿ (U+23BF LEFT PARENTHESIS LOWER HOOK) = tool/hook output.
                // U+239F kept for symmetry with older data.
                if trimmed.starts_with('\u{23BF}') || trimmed.starts_with('\u{239F}') {
                    return LineRole::Assistant;
                }
                LineRole::None // continuation of current role
            }
            Some(AiTool::Aider) => {
                if trimmed.contains("aider>") || trimmed.starts_with("> ") {
                    LineRole::User
                } else if trimmed.starts_with("Tokens:") || trimmed.starts_with("Edit:") {
                    LineRole::Assistant
                } else {
                    LineRole::None
                }
            }
            // Vendor TUIs that don't ship with a published prompt-glyph
            // table yet. Conservative heuristics:
            //   - User prompt: line begins with `❯ ` or `> ` (with content
            //     after the marker — bare markers are chrome).
            //   - Everything else: continuation (None) → defaults to
            //     assistant when the chunk has no user marker.
            // When we get a real grid.jsonl capture for a vendor with a
            // distinctive assistant glyph (e.g. Codex shows `▌`, opencode
            // ships its own theme), specialise the branch then. Until
            // then, the heuristic is intentionally narrow so we don't
            // mis-tag chrome as user input the way Claude's bare `❯` did.
            Some(
                AiTool::Codex
                | AiTool::Cursor
                | AiTool::Windsurf
                | AiTool::Cline
                | AiTool::Opencode
                | AiTool::Gemini
                | AiTool::Copilot
                | AiTool::Continue
                | AiTool::Cody,
            ) => {
                if trimmed.starts_with('\u{276F}') {
                    let rest = trimmed.trim_start_matches('\u{276F}').trim();
                    if rest.is_empty() {
                        return LineRole::None;
                    }
                    return LineRole::User;
                }
                if let Some(rest) = trimmed.strip_prefix("> ") {
                    if rest.trim().is_empty() {
                        return LineRole::None;
                    }
                    return LineRole::User;
                }
                LineRole::None
            }
            _ => {
                // Truly unknown / no current_tool: shell-prompt fallback.
                if trimmed.starts_with('\u{276F}') {
                    let rest = trimmed.trim_start_matches('\u{276F}').trim();
                    if rest.is_empty() {
                        return LineRole::None;
                    }
                    return LineRole::User;
                }
                if trimmed.starts_with("$ ") || trimmed.starts_with("% ") {
                    LineRole::User
                } else {
                    LineRole::None
                }
            }
        }
    }

    /// Classify the overall role of a chunk for the turn event.
    fn classify_role(&self, content: &str) -> String {
        // Check each line for a definitive role marker
        for line in content.lines() {
            match self.classify_line(line) {
                LineRole::User => return "user".to_string(),
                LineRole::Assistant => return "assistant".to_string(),
                LineRole::None => continue,
            }
        }
        // No definitive marker found — default to assistant
        "assistant".to_string()
    }

    /// Write an event to `.ai.jsonl` and notify the event sink.
    fn write_event(&mut self, event: &AiEvent) {
        // Write to local .ai.jsonl file
        if let Some(ref mut writer) = self.writer {
            match serde_json::to_string(event) {
                Ok(json) => {
                    if let Err(e) = writeln!(writer, "{}", json) {
                        warn!("AI log write error: {}", e);
                    }
                    let _ = writer.flush();
                }
                Err(e) => warn!("AI event serialization error: {}", e),
            }
        }

        // Notify event sink (non-blocking)
        if let Some(ref mut sink) = self.event_sink {
            sink.on_event(event);
        }
    }

    /// Final flush on shutdown.
    pub fn on_shutdown(&mut self) {
        self.flush_pending_turn();
        if let Some(ref mut w) = self.writer {
            let _ = w.flush();
        }
    }

    /// Last user prompt text (cleaned, truncated to 200 chars).
    pub fn last_user_prompt(&self) -> Option<&str> {
        self.last_user_prompt.as_deref()
    }
}

/// Clean a user prompt for tooltip display: strip common prompt prefixes
/// (`❯`, `>`, `$`, `%`), trim whitespace, and truncate to 200 chars.
fn clean_user_prompt(content: &str) -> String {
    let first_line = content.lines().next().unwrap_or(content);
    let cleaned = first_line
        .trim_start_matches('\u{276F}') // ❯
        .trim_start_matches('>')
        .trim_start_matches('$')
        .trim_start_matches('%')
        .trim();
    if cleaned.len() > 200 {
        let mut end = 200;
        // Don't split in the middle of a multi-byte char
        while !cleaned.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...", &cleaned[..end])
    } else {
        cleaned.to_string()
    }
}

/// Extract `<<html>>...<</html>>` blocks from content.
///
/// Returns `(stripped_content, original_if_had_html, html_blocks_if_any)`.
/// When no HTML blocks are found, `content_raw` and `html_blocks` are `None`
/// to avoid bloating the JSON for the common case.
fn extract_html_blocks(raw: &str) -> (String, Option<String>, Option<Vec<HtmlBlock>>) {
    // Lazy-compiled regex: matches <<html>>...<</html>> including newlines
    thread_local! {
        static RE: Regex = Regex::new(r"(?s)<<html>>(.*?)<</html>>").unwrap();
    }

    RE.with(|re| {
        let captures: Vec<_> = re.captures_iter(raw).collect();
        if captures.is_empty() {
            // No HTML blocks — content stays as-is, no raw/blocks overhead
            return (raw.to_string(), None, None);
        }

        let blocks: Vec<HtmlBlock> = captures
            .iter()
            .enumerate()
            .map(|(i, cap)| HtmlBlock {
                index: i,
                html: cap[1].trim().to_string(),
            })
            .collect();

        // Strip the markers and collapse resulting whitespace
        let stripped = re.replace_all(raw, "").to_string();
        // Collapse runs of 3+ newlines down to 2 (blank line separator)
        let cleaned = {
            thread_local! {
                static NL: Regex = Regex::new(r"\n{3,}").unwrap();
            }
            NL.with(|nl| nl.replace_all(&stripped, "\n\n").trim().to_string())
        };

        (cleaned, Some(raw.to_string()), Some(blocks))
    })
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Convert a Unix timestamp to ISO 8601 string (UTC).
pub fn format_iso_ts(ts: f64) -> String {
    let secs = ts as u64;
    let nanos = ((ts - secs as f64) * 1_000_000_000.0) as u32;
    let d = std::time::UNIX_EPOCH + std::time::Duration::new(secs, nanos);
    let elapsed = d
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = elapsed.as_secs();
    let rem = total_secs % 86400;
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    let seconds = rem % 60;

    let days = total_secs / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_html_blocks_passthrough() {
        let input = "Here is some plain text\nwith multiple lines";
        let (content, raw, blocks) = extract_html_blocks(input);
        assert_eq!(content, input);
        assert!(raw.is_none());
        assert!(blocks.is_none());
    }

    #[test]
    fn single_html_block_extracted() {
        let input = "Before text\n<<html>><div>hello</div><</html>>\nAfter text";
        let (content, raw, blocks) = extract_html_blocks(input);
        assert_eq!(content, "Before text\n\nAfter text");
        assert_eq!(raw.unwrap(), input);
        let blocks = blocks.unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].index, 0);
        assert_eq!(blocks[0].html, "<div>hello</div>");
    }

    #[test]
    fn multiple_html_blocks() {
        let input = "Start\n<<html>><p>one</p><</html>>\nMiddle\n<<html>><p>two</p><</html>>\nEnd";
        let (content, _raw, blocks) = extract_html_blocks(input);
        assert!(content.contains("Start"));
        assert!(content.contains("Middle"));
        assert!(content.contains("End"));
        assert!(!content.contains("<p>"));
        let blocks = blocks.unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].html, "<p>one</p>");
        assert_eq!(blocks[1].html, "<p>two</p>");
        assert_eq!(blocks[1].index, 1);
    }

    #[test]
    fn multiline_html_block() {
        let input = "Text\n<<html>>\n<div>\n  <span>multiline</span>\n</div>\n<</html>>\nMore";
        let (content, _raw, blocks) = extract_html_blocks(input);
        assert!(!content.contains("<div>"));
        let blocks = blocks.unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].html.contains("<span>multiline</span>"));
    }

    #[test]
    fn whitespace_collapsed() {
        // When HTML block is between text, stripping creates extra newlines
        let input = "Line1\n\n<<html>><div>x</div><</html>>\n\nLine2";
        let (content, _, _) = extract_html_blocks(input);
        // Should not have 3+ consecutive newlines
        assert!(!content.contains("\n\n\n"));
    }

    // ── classify_line / classify_role tests ───────────────────────────────

    fn claude_extractor() -> AiExtractor {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = AiExtractor::new("test", tmp.path(), None);
        e.current_tool = Some(AiTool::Claude);
        e
    }

    #[test]
    fn bare_prompt_marker_is_chrome_not_user() {
        // The core bug: Claude renders `❯` as empty prompt alongside status rows.
        // Must not classify it as a user turn.
        let e = claude_extractor();
        assert_eq!(e.classify_line("\u{276F}"), LineRole::None);
        assert_eq!(e.classify_line("\u{276F} "), LineRole::None);
        assert_eq!(e.classify_line("\u{276F}   "), LineRole::None);
        assert_eq!(e.classify_line("  \u{276F}  "), LineRole::None);
        // Real Claude emits ❯ followed by NBSP (U+00A0), not regular space.
        // Seen in production: `❯\u{a0} ` as the idle prompt marker.
        assert_eq!(e.classify_line("\u{276F}\u{00A0}"), LineRole::None);
        assert_eq!(e.classify_line("\u{276F}\u{00A0} "), LineRole::None);
        assert_eq!(e.classify_line("\u{276F}\u{00A0}   "), LineRole::None);
    }

    #[test]
    fn real_prompt_still_classifies_as_user() {
        let e = claude_extractor();
        assert_eq!(e.classify_line("\u{276F} hello"), LineRole::User);
        assert_eq!(e.classify_line("\u{276F} This is just a test"), LineRole::User);
        assert_eq!(e.classify_line("  \u{276F} indented prompt"), LineRole::User);
        // Unicode / emoji in prompt
        assert_eq!(e.classify_line("\u{276F} fix 🐛 bug"), LineRole::User);
        // Hebrew RTL
        assert_eq!(e.classify_line("\u{276F} בדיקה"), LineRole::User);
        // NBSP-separated prompt (matches real Claude output format)
        assert_eq!(e.classify_line("\u{276F}\u{00A0}What if ..."), LineRole::User);
        assert_eq!(
            e.classify_line("\u{276F}\u{00A0}I agree - we need a source project"),
            LineRole::User
        );
    }

    #[test]
    fn chrome_rows_are_not_user() {
        let e = claude_extractor();
        // The classic offenders seen in ai.jsonl
        assert_eq!(e.classify_line("\u{23F5}\u{23F5} accept edits on · 1 shell"), LineRole::None);
        assert_eq!(e.classify_line("★ medium · /effort"), LineRole::None);
        assert_eq!(e.classify_line("◐ medium · /effort"), LineRole::None);
        assert_eq!(e.classify_line("✶ Cooking… (31s · ↑ 90 tokens)"), LineRole::None);
        assert_eq!(e.classify_line("✻ Pollinating…"), LineRole::None);
        assert_eq!(e.classify_line("Read 1 file (ctrl+o to expand)"), LineRole::None);
    }

    #[test]
    fn assistant_markers_unchanged() {
        let e = claude_extractor();
        // Production glyphs — confirmed by scanning ~100MB of real grid.jsonl:
        // U+23FA appears 78k times, U+23BF appears 36k times, U+25CF only 8.
        assert_eq!(e.classify_line("\u{23FA} response"), LineRole::Assistant); // real ⏺
        assert_eq!(e.classify_line("\u{25CF} response"), LineRole::Assistant); // legacy
        assert_eq!(e.classify_line("\u{00B7} thinking"), LineRole::Assistant); // ·
        assert_eq!(e.classify_line("\u{23BF}  tool output"), LineRole::Assistant); // real ⎿
        assert_eq!(e.classify_line("\u{239F}  tool output"), LineRole::Assistant); // legacy
    }

    #[test]
    fn classify_role_skips_bare_prompt_and_finds_assistant() {
        // Reproduces the ai.jsonl bug:
        //   ❯
        //     Read 1 file (ctrl+o to expand)
        //   ⏺ Some response
        // Before fix: classified as "user" because of bare ❯.
        // After fix: bare ❯ is None, Read 1 file is None, ⏺ is Assistant → chunk is assistant.
        let e = claude_extractor();
        let chunk = "\u{276F}\n  Read 1 file (ctrl+o to expand)\n\u{25CF} Some response";
        assert_eq!(e.classify_role(chunk), "assistant");
    }

    #[test]
    fn classify_role_finds_user_when_prompt_has_content() {
        let e = claude_extractor();
        let chunk = "\u{276F} my real prompt\n⏵⏵ accept edits on · 1 shell";
        assert_eq!(e.classify_role(chunk), "user");
    }

    #[test]
    fn split_into_turns_no_longer_opens_user_chunk_on_bare_prompt() {
        // Direct regression test for the exact failure mode seen in production.
        // Pending lines from a single snapshot diff:
        //   [bare ❯ from prompt line, chrome status row, chrome status row]
        // Before fix: one chunk, classified user, content = chrome.
        // After fix: one chunk, classified assistant (default when no markers).
        let mut e = claude_extractor();
        e.pending_lines = vec![
            "\u{276F} ".to_string(),
            "\u{23F5}\u{23F5} accept edits on · 1 shell".to_string(),
            "◐ medium · /effort".to_string(),
        ];
        let chunks = e.split_into_turns();
        assert_eq!(chunks.len(), 1, "bare ❯ must not open a new chunk");
        assert_eq!(e.classify_role(&chunks[0].join("\n")), "assistant");
    }

    #[test]
    fn split_into_turns_still_splits_on_real_prompt() {
        let mut e = claude_extractor();
        e.pending_lines = vec![
            "\u{25CF} earlier response".to_string(),
            "\u{276F} real prompt one".to_string(),
            "\u{25CF} reply one".to_string(),
            "\u{276F} real prompt two".to_string(),
        ];
        let chunks = e.split_into_turns();
        assert_eq!(chunks.len(), 4);
        assert_eq!(e.classify_role(&chunks[0].join("\n")), "assistant");
        assert_eq!(e.classify_role(&chunks[1].join("\n")), "user");
        assert_eq!(e.classify_role(&chunks[2].join("\n")), "assistant");
        assert_eq!(e.classify_role(&chunks[3].join("\n")), "user");
    }

    #[test]
    fn clean_user_prompt_drops_marker() {
        assert_eq!(clean_user_prompt("\u{276F} hello"), "hello");
        assert_eq!(clean_user_prompt("\u{276F}"), "");
        assert_eq!(clean_user_prompt("\u{276F}   "), "");
        assert_eq!(clean_user_prompt("> $ % hello"), "$ % hello"); // only strips first marker
        // NBSP after ❯ — Rust's str::trim treats U+00A0 as whitespace
        assert_eq!(clean_user_prompt("\u{276F}\u{00A0}hello"), "hello");
        assert_eq!(clean_user_prompt("\u{276F}\u{00A0} "), "");
    }

    fn vendor_extractor(tool: AiTool) -> AiExtractor {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = AiExtractor::new("test", tmp.path(), None);
        e.current_tool = Some(tool);
        e
    }

    #[test]
    fn non_claude_vendor_user_prompt_with_gt_marker() {
        // Codex/Cursor/Windsurf/Gemini/opencode/Copilot all conventionally
        // render `> ` for user input in their TUIs. The vendor branch must
        // recognize that as a user line — the old generic fallback only
        // matched `❯`/`$`/`%`, so non-Claude prompts came through as None.
        for tool in [
            AiTool::Codex,
            AiTool::Cursor,
            AiTool::Windsurf,
            AiTool::Cline,
            AiTool::Opencode,
            AiTool::Gemini,
            AiTool::Copilot,
        ] {
            let e = vendor_extractor(tool);
            assert_eq!(
                e.classify_line("> hello"),
                LineRole::User,
                "{:?} should classify `> hello` as User",
                tool
            );
            assert_eq!(
                e.classify_line("> fix the bug"),
                LineRole::User,
                "{:?} should classify `> fix the bug` as User",
                tool
            );
        }
    }

    #[test]
    fn non_claude_vendor_bare_gt_marker_is_chrome() {
        // Same chrome-vs-real-prompt distinction as Claude's bare `❯`:
        // a `> ` line with no content after must NOT be classified user.
        for tool in [
            AiTool::Codex,
            AiTool::Cursor,
            AiTool::Windsurf,
            AiTool::Opencode,
            AiTool::Gemini,
            AiTool::Copilot,
        ] {
            let e = vendor_extractor(tool);
            assert_eq!(
                e.classify_line(">  "),
                LineRole::None,
                "{:?} should NOT classify bare `>` as User",
                tool
            );
            assert_eq!(
                e.classify_line(">"),
                LineRole::None,
                "{:?} should NOT classify standalone `>` as User",
                tool
            );
        }
    }

    #[test]
    fn non_claude_vendor_chrome_lines_are_none() {
        // Random output text (no marker) must be None so it inherits the
        // chunk's role from a definitive marker line.
        let e = vendor_extractor(AiTool::Codex);
        assert_eq!(e.classify_line("Reading file..."), LineRole::None);
        assert_eq!(e.classify_line("Tokens: 1.2k"), LineRole::None);
        assert_eq!(e.classify_line("  some indented output"), LineRole::None);
    }

    #[test]
    fn non_claude_vendor_arrow_marker_still_works() {
        // ❯ users (some vendor TUIs use it instead of `>`) still work.
        let e = vendor_extractor(AiTool::Gemini);
        assert_eq!(e.classify_line("\u{276F} prompt text"), LineRole::User);
        assert_eq!(e.classify_line("\u{276F}"), LineRole::None);
    }

    #[test]
    fn ai_tool_name_round_trip_covers_phase_a_vendors() {
        // DRY check: every Phase A vendor must have a stable `name()`
        // string, and that string is what hub session-link / registry
        // / classify_ai_process all use as the cross-codebase identifier.
        let pairs: &[(AiTool, &str)] = &[
            (AiTool::Claude, "claude"),
            (AiTool::Codex, "codex"),
            (AiTool::Cursor, "cursor"),
            (AiTool::Windsurf, "windsurf"),
            (AiTool::Cline, "cline"),
            (AiTool::Opencode, "opencode"),
            (AiTool::Gemini, "gemini"),
            (AiTool::Aider, "aider"),
            (AiTool::Copilot, "copilot"),
        ];
        for (tool, expected) in pairs {
            assert_eq!(tool.name(), *expected, "name() for {:?}", tool);
        }
    }

    #[test]
    fn generic_shell_prompts_only_match_with_content() {
        // Non-Claude tool: `$ ` / `% ` shell prompts still classified User.
        // Bare `❯` is None even without current_tool set.
        let tmp = tempfile::tempdir().unwrap();
        let mut e = AiExtractor::new("test", tmp.path(), None);
        e.current_tool = Some(AiTool::Unknown); // generic branch
        assert_eq!(e.classify_line("\u{276F}"), LineRole::None);
        assert_eq!(e.classify_line("\u{276F} real cmd"), LineRole::User);
        assert_eq!(e.classify_line("$ ls -la"), LineRole::User);
        assert_eq!(e.classify_line("% pwd"), LineRole::User);
    }

    // ── End-to-end on_snapshot pipeline tests ─────────────────────────────
    //
    // These exercise the full flow: build synthetic GridSnapshot records
    // that mimic real Claude UI, pipe them through on_snapshot, collect
    // emitted events via a sink, and assert roles + content.

    use immorterm_core::log::{CursorPos, GridSnapshot, RowRuns, SnapshotTrigger};
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone)]
    struct CollectedEvent {
        event: String,
        role: Option<String>,
        content: Option<String>,
    }

    struct CollectingSink {
        events: Arc<Mutex<Vec<CollectedEvent>>>,
    }

    impl LogEventSink for CollectingSink {
        fn on_event(&mut self, event: &AiEvent) {
            self.events.lock().unwrap().push(CollectedEvent {
                event: event.event.clone(),
                role: event.role.clone(),
                content: event.content.clone(),
            });
        }
    }

    /// Build a GridSnapshot from a list of text rows. Each row becomes
    /// a single run of default-color text.
    fn snapshot_from_rows(rows: &[&str]) -> GridSnapshot {
        use immorterm_core::log::{AttributeRun, DefaultColor, LogColor};
        let grid: Vec<RowRuns> = rows
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_empty())
            .map(|(i, s)| RowRuns {
                row: i,
                runs: vec![AttributeRun {
                    t: (*s).to_string(),
                    fg: LogColor::Default(DefaultColor),
                    bg: LogColor::Default(DefaultColor),
                    a: 0,
                    r: 0,
                }],
                wrapped: false,
            })
            .collect();
        GridSnapshot {
            v: 1,
            record_type: "snapshot".to_string(),
            ts: 0.0,
            trigger: SnapshotTrigger::Periodic,
            cols: 132,
            rows: rows.len(),
            cursor: CursorPos { col: 0, row: 0 },
            cwd: String::new(),
            exit_code: None,
            grid,
            sb_lines: 0,
            sb_hash: String::new(),
        }
    }

    fn collect_events_for(
        snapshots: &[Vec<&str>],
    ) -> Vec<CollectedEvent> {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink: Box<dyn LogEventSink> = Box::new(CollectingSink {
            events: events.clone(),
        });
        let tmp = tempfile::tempdir().unwrap();
        let mut e = AiExtractor::new("test", tmp.path(), Some(sink));
        e.on_ai_state_change(Some(AiTool::Claude), None, None, None);
        for rows in snapshots {
            e.on_snapshot(&snapshot_from_rows(rows));
        }
        e.on_ai_state_change(None, None, None, None);
        drop(e); // ensure sink is dropped before try_unwrap
        Arc::try_unwrap(events)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default()
    }

    const CHROME_PREFIXES: &[&str] = &[
        "\u{23F5}\u{23F5}", // ⏵⏵
        "\u{23F5}",         // ⏵
        "★",
        "◐",
        "◯",
        "✻",
        "✶",
        "✳",
        "✽",
    ];

    fn first_line_is_chrome(content: &str) -> bool {
        let first = content.lines().next().unwrap_or("").trim();
        if first.is_empty() {
            return true;
        }
        CHROME_PREFIXES.iter().any(|p| first.starts_with(p))
    }

    #[test]
    fn e2e_bare_prompt_chrome_snapshot_emits_no_user_turn() {
        // Simulates the exact production failure mode: Claude idle screen
        // with empty `❯` prompt row + status chrome rows. Before the fix,
        // this whole block was emitted as a user turn.
        //
        // Seed is a minimal non-empty snapshot so prev_text is populated,
        // then subsequent snapshots are diffed against it — otherwise the
        // "first snapshot = baseline" rule vacuously emits zero turns.
        let snaps = vec![
            // Baseline (bootstrap prev_text)
            vec!["some prior content"],
            // Claude re-renders: bare ❯ prompt + chrome + tool output + response
            vec![
                "\u{276F}                                                ",
                "  \u{23F5}\u{23F5} accept edits on \u{00B7} 1 shell",
                "  \u{25C0} medium \u{00B7} /effort",
                "Read 1 file (ctrl+o to expand)",
                "  \u{239F} file.rs",
                "\u{25CF} Looking at this now",
            ],
        ];
        let events = collect_events_for(&snaps);
        let turns: Vec<_> = events.iter().filter(|e| e.event == "turn").collect();
        // Must have emitted some turns (not vacuous)
        assert!(
            !turns.is_empty(),
            "expected some turns to be emitted — test is vacuous otherwise"
        );
        let bad: Vec<_> = turns
            .iter()
            .filter(|t| t.role.as_deref() == Some("user"))
            .filter(|t| first_line_is_chrome(t.content.as_deref().unwrap_or("")))
            .collect();
        assert!(
            bad.is_empty(),
            "user turns containing chrome: {:#?}",
            bad.iter()
                .map(|t| t.content.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
        // Also: no user turn should match our literal production failure patterns
        for t in turns.iter().filter(|t| t.role.as_deref() == Some("user")) {
            let c = t.content.as_deref().unwrap_or("");
            assert!(
                !c.contains("accept edits on"),
                "chrome leaked into user turn: {:?}",
                c
            );
            assert!(
                !c.starts_with("Read 1 file"),
                "tool-use display leaked into user turn: {:?}",
                c
            );
        }
    }

    #[test]
    fn e2e_real_prompt_becomes_user_turn() {
        // Sanity: a real typed prompt must still produce a user turn with
        // the prompt text as first line. Note: on_snapshot treats the FIRST
        // non-empty snapshot as the baseline (prev_text seed) and doesn't
        // emit anything from it — so the seed must contain only chrome.
        let snaps = vec![
            // Baseline: Claude idle UI (bare prompt + chrome)
            vec![
                "\u{276F}",
                "  \u{23F5}\u{23F5} accept edits on \u{00B7} 1 shell",
            ],
            // User has typed their prompt — diff picks this up
            vec![
                "\u{276F} please fix the scrollback bug",
                "  \u{23F5}\u{23F5} accept edits on \u{00B7} 1 shell",
            ],
            // Assistant response arrives + tool output (5+ new lines → flush)
            vec![
                "\u{276F} please fix the scrollback bug",
                "\u{25CF} Sure, let me look at terminal.rs",
                "  \u{239F} reading file...",
                "  \u{239F}   found the reflow call at line 633",
                "  \u{239F}   scrollback stays at original width",
                "\u{00B7} thinking about fix...",
                "  \u{23F5}\u{23F5} accept edits on \u{00B7} 1 shell",
            ],
        ];
        let events = collect_events_for(&snaps);
        let user_turns: Vec<_> = events
            .iter()
            .filter(|e| e.event == "turn" && e.role.as_deref() == Some("user"))
            .collect();
        assert!(
            !user_turns.is_empty(),
            "expected at least one user turn for real typed prompt"
        );
        let found = user_turns.iter().any(|t| {
            t.content
                .as_deref()
                .map(|c| c.contains("please fix the scrollback bug"))
                .unwrap_or(false)
        });
        assert!(found, "user turns: {:#?}", user_turns);
    }

    #[test]
    fn e2e_tool_output_snapshot_never_becomes_user_turn() {
        // Second bug from production: Claude tool-use display
        // "Read 1 file (ctrl+o to expand)" appearing on a row where the
        // user thinks nothing has been typed.
        let snaps = vec![
            vec!["baseline seed row"],
            vec![
                "\u{25CF} I'll read the file now",
                "  \u{239F} Read 1 file (ctrl+o to expand)",
                "  \u{239F}   apps/extension/src/gpu-terminal.ts",
                "\u{276F}  ", // bare prompt, user hasn't typed
                "  \u{23F5}\u{23F5} accept edits on \u{00B7} 1 shell",
            ],
        ];
        let events = collect_events_for(&snaps);
        let turns: Vec<_> = events.iter().filter(|e| e.event == "turn").collect();
        assert!(
            !turns.is_empty(),
            "expected at least one turn — pipeline is not being exercised"
        );
        for t in turns.iter() {
            let content = t.content.as_deref().unwrap_or("");
            if t.role.as_deref() == Some("user") {
                panic!(
                    "no user turn expected (no real ❯ prompt typed), got: {:?}",
                    content
                );
            }
        }
    }

    // ── Real grid.jsonl replay (ignored by default) ──────────────────────
    //
    // Scans `~/.immorterm/terminals/logs/*/grid.jsonl`, picks the richest
    // fixture, replays it through AiExtractor, asserts no user turns
    // contain chrome. Run with:
    //
    //   cargo test -p structured-logs --lib replay_real_grid \
    //     -- --ignored --nocapture
    //
    // Set GRID_FIXTURE=<path> to pin a specific file.

    fn find_grid_fixture() -> Option<std::path::PathBuf> {
        use std::path::PathBuf;
        if let Ok(explicit) = std::env::var("GRID_FIXTURE") {
            let p = PathBuf::from(explicit);
            if p.exists() {
                return Some(p);
            }
        }
        let bases: Vec<PathBuf> = vec![
            PathBuf::from(
                "/tmp/immorterm/terminals/logs",
            ),
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(format!("{}/.immorterm/terminals/logs", h)))
                .unwrap_or_default(),
        ];
        for base in bases.iter() {
            let Ok(entries) = std::fs::read_dir(base) else {
                continue;
            };
            let mut candidates: Vec<_> = entries
                .flatten()
                .filter_map(|e| {
                    let p = e.path().join("grid.jsonl");
                    let meta = std::fs::metadata(&p).ok()?;
                    let sz = meta.len();
                    if (20_000..5_000_000).contains(&sz) {
                        Some((sz, p))
                    } else {
                        None
                    }
                })
                .collect();
            candidates.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
            if let Some((_, p)) = candidates.into_iter().next() {
                return Some(p);
            }
        }
        None
    }

    #[test]
    #[ignore]
    fn replay_real_grid_no_chrome_user_turns() {
        let Some(grid) = find_grid_fixture() else {
            eprintln!("no grid.jsonl fixture found — skipping");
            return;
        };
        eprintln!("replaying {:?}", grid);

        let events = Arc::new(Mutex::new(Vec::new()));
        let sink: Box<dyn LogEventSink> = Box::new(CollectingSink {
            events: events.clone(),
        });
        let tmp = tempfile::tempdir().unwrap();
        let mut e = AiExtractor::new("test", tmp.path(), Some(sink));
        e.on_ai_state_change(Some(AiTool::Claude), None, None, None);

        let text = std::fs::read_to_string(&grid).expect("read grid.jsonl");
        let (mut ok, mut skip, mut pre_pending_max) = (0, 0, 0);
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<GridSnapshot>(line) {
                Ok(snap) => {
                    e.on_snapshot(&snap);
                    pre_pending_max = pre_pending_max.max(e.pending_lines.len());
                    ok += 1;
                }
                Err(err) => {
                    skip += 1;
                    eprintln!("skip line {}: {}", i, err);
                }
            }
        }
        eprintln!(
            "processed {} snapshots, skipped {}, max pending_lines seen: {}",
            ok, skip, pre_pending_max
        );
        e.on_ai_state_change(None, None, None, None);

        let collected: Vec<CollectedEvent> = Arc::try_unwrap(events)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default();
        let turns: Vec<_> = collected.iter().filter(|e| e.event == "turn").collect();
        let user_turns: Vec<_> = turns
            .iter()
            .filter(|e| e.role.as_deref() == Some("user"))
            .collect();
        eprintln!(
            "total turns: {}  user turns: {}",
            turns.len(),
            user_turns.len()
        );
        for (i, t) in user_turns.iter().enumerate() {
            let c = t.content.as_deref().unwrap_or("");
            let first = c.lines().next().unwrap_or("").trim();
            eprintln!(
                "user[{}]: {}",
                i,
                first.chars().take(120).collect::<String>()
            );
        }
        let bad: Vec<_> = user_turns
            .iter()
            .filter(|t| first_line_is_chrome(t.content.as_deref().unwrap_or("")))
            .collect();
        assert!(
            bad.is_empty(),
            "{} user turn(s) with chrome first line: {:#?}",
            bad.len(),
            bad.iter()
                .map(|t| t.content.as_deref().unwrap_or(""))
                .collect::<Vec<_>>()
        );
    }
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
