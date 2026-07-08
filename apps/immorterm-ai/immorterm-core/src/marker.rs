//! Inline marker parser for `<<express>>` text-styling tags in PTY byte
//! streams.
//!
//! Currently active grammar:
//!
//! - **Express** — `<<express mood=creative>>text<</express>>`. Inline anywhere,
//!   bounded by EXPRESS_AUTO_CLOSE_LIMIT chars to recover from forgotten closers.
//!
//! Disabled (kept for future re-enable):
//!
//! - **HTML overlay** `<<im-html [attrs]>>...<</im-html>>` in-prose path. The
//!   parser machinery is intact (HtmlBody/PostOpenerLookahead states, etc.),
//!   but `parse_tag_kind` no longer returns `Some(TagKind::Html)`. Reason:
//!   Claude Code's Ink renderer prefixes every response line with the bullet
//!   character + ANSI cursor-positioning escapes, so `<<im-html>>` from AI
//!   output is structurally never at SOL of the byte stream. Use the MCP
//!   `draw_html` IPC path for HTML overlays instead. Re-enable by uncommenting
//!   the html branch in `parse_tag_kind`.
//!
//! Marker bytes are stripped from visible output; events fire for the consumer.

use std::collections::HashMap;

use crate::expression::{Animation, DangerLevel, ExpressionState, Mood};

/// Maximum characters an `<<express>>` tag can be open before auto-closing.
/// Prevents runaway expression from a forgotten closing tag.
const EXPRESS_AUTO_CLOSE_LIMIT: usize = 500;

/// Maximum bytes buffered inside an `<<im-html>>` block before we give up.
const HTML_MAX_BUFFER: usize = 64 * 1024; // 64 KB

/// The unique tag name used for HTML overlay blocks. Chosen to never match
/// real-world prose: nobody writes "im-html" in casual English.
#[allow(dead_code)]
const HTML_TAG: &[u8] = b"im-html";

/// Events emitted by the marker parser.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkerEvent {
    /// `<<express mood=creative confidence=high>>` — start styling subsequent text.
    ExpressStart(HashMap<String, String>),
    /// `<</express>>` — stop styling.
    ExpressEnd,
    /// `<<express reset>>` — immediate full reset.
    ExpressReset,
    /// `<<im-html [attrs]>>` — start of HTML block (attrs may contain anchor, position, id).
    HtmlStart(HashMap<String, String>),
    /// `<</im-html>>` — end of HTML block with buffered content.
    HtmlEnd(String),
    /// Regular byte — pass through to VTE parser.
    PassThrough(u8),
}

/// Internal parser state.
#[derive(Debug, Clone)]
enum State {
    /// Normal passthrough — looking for `<`.
    Normal,
    /// Saw first `<`. The bool snapshots whether `at_line_start` was true when
    /// the `<` arrived; only HTML opener checks this (express tags are inline).
    Open1 { at_sol: bool },
    /// Saw `<<` — reading tag name. The bool tracks whether the opener was at SOL.
    TagName { at_sol: bool, name: Vec<u8> },
    /// Reading attributes (after tag name + space).
    Attributes(TagKind, Vec<u8>),
    /// Saw `<<` then `/` — closing tag (top-level, only express closes here).
    CloseName(Vec<u8>),
    /// Inside `<<im-html>>` body — buffering content until SOL `<</im-html>>`.
    /// `at_sol` tracks whether the next byte is at start of a body line — the
    /// closer is only honored when `at_sol` is true, so mid-line `<</im-html>>`
    /// inside HTML content is preserved as content.
    /// `skip_initial_newline` swallows a single `\n` (or `\r\n`) immediately
    /// after the opener `>>`, so `<<im-html>>\n<div>...</div>\n<</im-html>>`
    /// yields content `<div>...</div>\n` (markdown-fence convention: the
    /// opener's trailing newline is part of the opener, not the body).
    HtmlBody {
        attrs: HashMap<String, String>,
        buf: Vec<u8>,
        at_sol: bool,
        skip_initial_newline: bool,
    },
    /// Inside HtmlBody, saw `<` at SOL — potential close opener.
    HtmlBodyOpen1 {
        attrs: HashMap<String, String>,
        buf: Vec<u8>,
    },
    /// Inside HtmlBody, saw `<<` at SOL.
    HtmlBodyOpen2 {
        attrs: HashMap<String, String>,
        buf: Vec<u8>,
    },
    /// Inside HtmlBody, saw `<</` at SOL — reading close tag name.
    HtmlBodyCloseName {
        attrs: HashMap<String, String>,
        buf: Vec<u8>,
        name: Vec<u8>,
    },
    /// Just emitted HtmlEnd — consume up to one optional `\n` or `\r\n` before
    /// returning to Normal. Mirrors the leading-newline swallow on the opener,
    /// so the closer's trailing newline doesn't leak into the visible grid.
    PostHtmlClose,
    /// Saw `\r` after `<</im-html>>` — consume an optional `\n` next.
    PostHtmlCloseExpectLf,
    /// Saw `<<im-html [attrs]>>` opener — but not yet committed. The opener
    /// line must contain ONLY whitespace between `>>` and the next `\n`. This
    /// guards against word-wrapped prose mentions where `<<im-html>>` happens
    /// to land at column 0 followed by more sentence text. If a non-whitespace
    /// byte arrives, the entire `<<im-html ...>>` plus any consumed whitespace
    /// is flushed as literal PassThrough — no HtmlStart fires.
    PostOpenerLookahead {
        attrs: HashMap<String, String>,
        /// The literal opener bytes (`<<im-html ...>>`) for replay on rejection.
        literal: Vec<u8>,
        /// Whitespace bytes consumed since `>>` (space/tab) — also replayed.
        pending_ws: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum TagKind {
    Express,
    Html,
}

/// Byte-by-byte inline marker parser.
///
/// Feed PTY bytes one at a time via `feed()`. Each call returns a `MarkerEvent`
/// telling the caller whether to pass the byte through to VTE or handle a marker event.
#[derive(Debug, Clone)]
pub struct MarkerParser {
    state: State,
    /// Characters processed since last `<<express>>` open (for auto-close safety).
    express_char_count: usize,
    /// Whether we're currently inside an `<<express>>` range.
    in_express: bool,
    /// True when the next byte will land at the start of a line (initial state
    /// or just after `\n` / `\r`). Drives the SOL-anchored HTML opener detection.
    at_line_start: bool,
    /// When false, all bytes pass through unchanged (no marker detection).
    /// Default: false. The daemon enables this; the WASM frontend leaves it off.
    enabled: bool,
}

impl Default for MarkerParser {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkerParser {
    pub fn new() -> Self {
        Self {
            state: State::Normal,
            express_char_count: 0,
            in_express: false,
            at_line_start: true,
            enabled: false,
        }
    }

    /// Enable marker parsing. Call this on the daemon side only —
    /// the WASM frontend should leave parsing disabled.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Whether we're inside an `<<express>>` range (text is being styled).
    pub fn in_express(&self) -> bool {
        self.in_express
    }

    /// Force-reset parser state (e.g. on OSC 133;A prompt detection).
    pub fn reset(&mut self) -> Option<MarkerEvent> {
        let was_in_express = self.in_express;
        self.state = State::Normal;
        self.express_char_count = 0;
        self.in_express = false;
        self.at_line_start = true;
        if was_in_express {
            Some(MarkerEvent::ExpressEnd)
        } else {
            None
        }
    }

    /// Feed a single byte. Returns one or more events.
    ///
    /// Most bytes produce a single `PassThrough`. Marker boundaries produce
    /// `ExpressStart`/`ExpressEnd`/`HtmlStart`/`HtmlEnd`. Marker bytes themselves
    /// are consumed (not passed through).
    pub fn feed(&mut self, byte: u8) -> Vec<MarkerEvent> {
        // When disabled, all bytes pass through unchanged
        if !self.enabled {
            return vec![MarkerEvent::PassThrough(byte)];
        }

        // Auto-close safety: if express is open too long, force close
        if self.in_express {
            self.express_char_count += 1;
            if self.express_char_count >= EXPRESS_AUTO_CLOSE_LIMIT {
                self.in_express = false;
                self.express_char_count = 0;
                let mut events = vec![MarkerEvent::ExpressEnd];
                events.extend(self.feed_inner(byte));
                return events;
            }
        }

        self.feed_inner(byte)
    }

    fn feed_inner(&mut self, byte: u8) -> Vec<MarkerEvent> {
        let state = std::mem::replace(&mut self.state, State::Normal);

        match state {
            State::Normal => {
                if byte == b'<' {
                    let at_sol = self.at_line_start;
                    self.state = State::Open1 { at_sol };
                    self.at_line_start = false;
                    vec![] // consume, wait for second <
                } else {
                    // \n and \r both mean "next byte is at column 0"
                    self.at_line_start = byte == b'\n' || byte == b'\r';
                    vec![MarkerEvent::PassThrough(byte)]
                }
            }

            State::Open1 { at_sol } => {
                if byte == b'<' {
                    // << — start of marker tag
                    self.state = State::TagName {
                        at_sol,
                        name: Vec::with_capacity(16),
                    };
                    vec![] // consume
                } else {
                    // False alarm — emit the buffered '<' and this byte
                    self.at_line_start = byte == b'\n' || byte == b'\r';
                    vec![
                        MarkerEvent::PassThrough(b'<'),
                        MarkerEvent::PassThrough(byte),
                    ]
                }
            }

            State::TagName { at_sol, mut name } => {
                if byte == b'/' && name.is_empty() {
                    // <</ — closing tag (top-level — only express closes inline;
                    // html closes from inside HtmlBody only)
                    self.state = State::CloseName(Vec::with_capacity(16));
                    vec![]
                } else if byte == b' ' || byte == b'\t' {
                    // End of tag name, start attributes
                    match Self::parse_tag_kind(&name, at_sol) {
                        Some(kind) => {
                            self.state = State::Attributes(kind, Vec::with_capacity(64));
                            vec![]
                        }
                        None => {
                            // Unknown tag, or html opener not at SOL — flush as literal
                            self.at_line_start = byte == b'\n' || byte == b'\r';
                            Self::flush_as_literal(b"<<", &name, Some(byte))
                        }
                    }
                } else if byte == b'>' {
                    // First '>' of opening-tag close — wait for the second
                    match Self::parse_tag_kind(&name, at_sol) {
                        Some(kind) => {
                            self.state = State::Attributes(kind, vec![b'>']);
                            vec![]
                        }
                        None => {
                            self.at_line_start = false;
                            Self::flush_as_literal(b"<<", &name, Some(byte))
                        }
                    }
                } else if name.len() > 20 {
                    // Tag name too long — not a marker
                    self.at_line_start = byte == b'\n';
                    Self::flush_as_literal(b"<<", &name, Some(byte))
                } else {
                    name.push(byte);
                    self.state = State::TagName { at_sol, name };
                    vec![]
                }
            }

            State::Attributes(kind, mut buf) => {
                if byte == b'>' && buf.last() == Some(&b'>') {
                    // >> — end of opening tag
                    buf.pop(); // remove the first '>'
                    let attrs_str = String::from_utf8_lossy(&buf).to_string();
                    let attrs = Self::parse_attrs(&attrs_str);

                    match kind {
                        TagKind::Express => {
                            if attrs.contains_key("reset") {
                                vec![MarkerEvent::ExpressReset]
                            } else {
                                self.in_express = true;
                                self.express_char_count = 0;
                                vec![MarkerEvent::ExpressStart(attrs)]
                            }
                        }
                        TagKind::Html => {
                            // Don't fire HtmlStart yet — we require the opener
                            // to be alone on its line (only whitespace between
                            // `>>` and `\n`). PostOpenerLookahead enforces this.
                            // Reconstruct literal `<<im-html [attrs]>>` for
                            // replay if rejection is needed.
                            let mut literal = Vec::with_capacity(buf.len() + 12);
                            literal.extend_from_slice(b"<<im-html");
                            if !buf.is_empty() {
                                // Attr form: prefix with space, append attrs.
                                literal.push(b' ');
                                literal.extend_from_slice(&buf);
                            }
                            literal.extend_from_slice(b">>");
                            self.state = State::PostOpenerLookahead {
                                attrs,
                                literal,
                                pending_ws: Vec::new(),
                            };
                            vec![]
                        }
                    }
                } else if buf.len() > 1024 {
                    // Attributes too long — not a marker
                    let prefix = match kind {
                        TagKind::Express => b"<<express " as &[u8],
                        TagKind::Html => b"<<im-html " as &[u8],
                    };
                    let mut events = Vec::with_capacity(prefix.len() + buf.len() + 1);
                    for &b in prefix {
                        events.push(MarkerEvent::PassThrough(b));
                    }
                    for &b in &buf {
                        events.push(MarkerEvent::PassThrough(b));
                    }
                    events.push(MarkerEvent::PassThrough(byte));
                    self.at_line_start = false;
                    events
                } else {
                    buf.push(byte);
                    self.state = State::Attributes(kind, buf);
                    vec![]
                }
            }

            State::CloseName(mut name) => {
                if byte == b'>' && name.last() == Some(&b'>') {
                    name.pop(); // remove the '>'
                    let tag_name = String::from_utf8_lossy(&name).to_lowercase();
                    match tag_name.as_str() {
                        "express" => {
                            self.in_express = false;
                            self.express_char_count = 0;
                            vec![MarkerEvent::ExpressEnd]
                        }
                        "im-html" => {
                            // Closing im-html OUTSIDE body — preserve as literal
                            // so prose that mentions `<</im-html>>` survives.
                            // (The actual closer is matched inside HtmlBody only.)
                            // Re-add the popped `>` so the flush emits both
                            // closing brackets.
                            let mut full = name.clone();
                            full.push(b'>');
                            Self::flush_as_literal(b"<</", &full, Some(b'>'))
                        }
                        _ => Self::flush_as_literal(b"<</", &name, Some(b'>')),
                    }
                } else if name.len() > 20 {
                    self.at_line_start = byte == b'\n' || byte == b'\r';
                    Self::flush_as_literal(b"<</", &name, Some(byte))
                } else {
                    name.push(byte);
                    self.state = State::CloseName(name);
                    vec![]
                }
            }

            State::HtmlBody { attrs, mut buf, at_sol, skip_initial_newline } => {
                if skip_initial_newline && (byte == b'\n' || byte == b'\r') {
                    // Markdown-fence convention: opener line ends with `\n`,
                    // so swallow exactly one initial `\n` or `\r\n` after `>>`.
                    let next_skip = byte == b'\r'; // `\r` may be followed by `\n` to also drop
                    self.state = State::HtmlBody {
                        attrs,
                        buf,
                        at_sol: true,
                        skip_initial_newline: next_skip,
                    };
                    vec![]
                } else if byte == b'<' && at_sol {
                    // SOL `<` — potential close-opener
                    self.state = State::HtmlBodyOpen1 { attrs, buf };
                    vec![]
                } else if buf.len() >= HTML_MAX_BUFFER {
                    // Too large — flush and abort
                    let content = String::from_utf8_lossy(&buf).to_string();
                    self.at_line_start = false;
                    vec![MarkerEvent::HtmlEnd(content)]
                } else {
                    buf.push(byte);
                    let next_at_sol = byte == b'\n' || byte == b'\r';
                    self.state = State::HtmlBody {
                        attrs,
                        buf,
                        at_sol: next_at_sol,
                        skip_initial_newline: false,
                    };
                    vec![]
                }
            }

            State::HtmlBodyOpen1 { attrs, mut buf } => {
                if byte == b'<' {
                    self.state = State::HtmlBodyOpen2 { attrs, buf };
                    vec![]
                } else {
                    // False alarm — `<` at SOL but no second `<`. Add `<` and the
                    // current byte to buffer; track SOL based on the byte.
                    buf.push(b'<');
                    buf.push(byte);
                    let at_sol = byte == b'\n' || byte == b'\r';
                    self.state = State::HtmlBody {
                        attrs,
                        buf,
                        at_sol,
                        skip_initial_newline: false,
                    };
                    vec![]
                }
            }

            State::HtmlBodyOpen2 { attrs, mut buf } => {
                if byte == b'/' {
                    // <</ at SOL — potential closing tag
                    self.state = State::HtmlBodyCloseName {
                        attrs,
                        buf,
                        name: Vec::with_capacity(8),
                    };
                    vec![]
                } else {
                    // `<<` at SOL but not a close — preserve as content
                    buf.push(b'<');
                    buf.push(b'<');
                    buf.push(byte);
                    let at_sol = byte == b'\n' || byte == b'\r';
                    self.state = State::HtmlBody {
                        attrs,
                        buf,
                        at_sol,
                        skip_initial_newline: false,
                    };
                    vec![]
                }
            }

            State::PostOpenerLookahead { attrs, literal, mut pending_ws } => {
                if byte == b' ' || byte == b'\t' {
                    // Trailing whitespace allowed — accumulate, stay in state.
                    pending_ws.push(byte);
                    self.state = State::PostOpenerLookahead { attrs, literal, pending_ws };
                    vec![]
                } else if byte == b'\n' || byte == b'\r' {
                    // Opener line confirmed. Fire HtmlStart and enter HtmlBody.
                    // The `\n` (or `\r`) here is the opener's trailing LF — we
                    // consume it (don't pass through, don't put in body).
                    let attrs_emit = attrs.clone();
                    self.state = State::HtmlBody {
                        attrs,
                        buf: Vec::with_capacity(1024),
                        at_sol: true,
                        // Already consumed the LF here; if the byte was `\r`,
                        // the next byte may be `\n` (CRLF) — swallow it too.
                        skip_initial_newline: byte == b'\r',
                    };
                    vec![MarkerEvent::HtmlStart(attrs_emit)]
                } else {
                    // Non-whitespace after `>>` — NOT an opener line.
                    // Flush literal opener bytes, accumulated whitespace, and
                    // process this byte through Normal so further parsing
                    // (e.g. another `<<` later) still works.
                    let mut events = Vec::with_capacity(literal.len() + pending_ws.len() + 4);
                    for &b in &literal {
                        events.push(MarkerEvent::PassThrough(b));
                    }
                    for &b in &pending_ws {
                        events.push(MarkerEvent::PassThrough(b));
                    }
                    self.at_line_start = false;
                    events.extend(self.feed_inner(byte));
                    events
                }
            }

            State::PostHtmlClose => {
                if byte == b'\r' {
                    self.state = State::PostHtmlCloseExpectLf;
                    vec![] // consume; expect optional \n next
                } else if byte == b'\n' {
                    self.at_line_start = true;
                    vec![] // consume the closer's trailing LF
                } else {
                    // No trailing newline — process this byte as Normal
                    self.feed_inner(byte)
                }
            }

            State::PostHtmlCloseExpectLf => {
                if byte == b'\n' {
                    self.at_line_start = true;
                    vec![] // consume the LF that completes \r\n
                } else {
                    // \r alone moved cursor to col 0 already; reprocess byte
                    self.at_line_start = true;
                    self.feed_inner(byte)
                }
            }

            State::HtmlBodyCloseName { attrs, buf, mut name } => {
                if byte == b'>' && name.last() == Some(&b'>') {
                    name.pop(); // remove the '>'
                    let tag_name = String::from_utf8_lossy(&name).to_lowercase();
                    if tag_name == "im-html" {
                        // <</im-html>> at SOL — end of HTML block. Trailing
                        // newline before the closer remains part of the body
                        // (matches the prior fence convention of preserving
                        // body whitespace except the opener's own trailing LF).
                        // Transition to PostHtmlClose to swallow the closer's
                        // OWN trailing newline so it doesn't render as a blank
                        // grid row.
                        let content = String::from_utf8_lossy(&buf).to_string();
                        self.state = State::PostHtmlClose;
                        self.at_line_start = false;
                        vec![MarkerEvent::HtmlEnd(content)]
                    } else {
                        // Not closing im-html — treat as content
                        let mut buf = buf;
                        buf.extend_from_slice(b"<</");
                        buf.extend_from_slice(&name);
                        buf.push(b'>');
                        buf.push(byte);
                        let at_sol = byte == b'\n' || byte == b'\r';
                        self.state = State::HtmlBody {
                            attrs,
                            buf,
                            at_sol,
                            skip_initial_newline: false,
                        };
                        vec![]
                    }
                } else if name.len() > 20 {
                    // Too long — not a closing tag, add to buffer
                    let mut buf = buf;
                    buf.extend_from_slice(b"<</");
                    buf.extend_from_slice(&name);
                    buf.push(byte);
                    let at_sol = byte == b'\n' || byte == b'\r';
                    self.state = State::HtmlBody {
                        attrs,
                        buf,
                        at_sol,
                        skip_initial_newline: false,
                    };
                    vec![]
                } else {
                    name.push(byte);
                    self.state = State::HtmlBodyCloseName { attrs, buf, name };
                    vec![]
                }
            }
        }
    }

    /// Parse tag kind from name bytes. The HTML opener is gated on SOL — if
    /// `<<im-html>>` appears mid-line (e.g. in tutorial prose), it's literal.
    fn parse_tag_kind(name: &[u8], _at_sol: bool) -> Option<TagKind> {
        let lower: Vec<u8> = name.iter().map(|b| b.to_ascii_lowercase()).collect();
        if lower.as_slice() == b"express" {
            Some(TagKind::Express)
        // The in-prose `<<im-html>>` path is intentionally disabled.
        // Claude Code's Ink renderer prefixes every response line with the
        // bullet character + ANSI cursor-positioning escapes, so `<<im-html>>`
        // never lands at SOL of the byte stream — the parser could never fire
        // on AI output regardless of how strict the SOL anchor is. Use the MCP
        // `draw_html` IPC path instead. Re-enable by uncommenting the branch:
        //
        // } else if lower.as_slice() == HTML_TAG && _at_sol {
        //     Some(TagKind::Html)
        } else {
            None
        }
    }

    /// Parse `key=value key2=value2` attribute string into a HashMap.
    fn parse_attrs(s: &str) -> HashMap<String, String> {
        let mut attrs = HashMap::new();
        let s = s.trim();
        if s.is_empty() {
            return attrs;
        }

        for token in s.split_whitespace() {
            if let Some(eq_pos) = token.find('=') {
                let key = token[..eq_pos].to_lowercase();
                let val = token[eq_pos + 1..].trim_matches('"').trim_matches('\'');
                attrs.insert(key, val.to_string());
            } else {
                attrs.insert(token.to_lowercase(), String::new());
            }
        }
        attrs
    }

    /// Flush buffered bytes as literal PassThrough events (when a potential marker
    /// turns out to not be one).
    fn flush_as_literal(prefix: &[u8], name: &[u8], extra: Option<u8>) -> Vec<MarkerEvent> {
        let mut events = Vec::with_capacity(prefix.len() + name.len() + 1);
        for &b in prefix {
            events.push(MarkerEvent::PassThrough(b));
        }
        for &b in name {
            events.push(MarkerEvent::PassThrough(b));
        }
        if let Some(b) = extra {
            events.push(MarkerEvent::PassThrough(b));
        }
        events
    }
}

/// Build an `ExpressionState` from parsed marker attributes.
///
/// Used by the terminal when an `<<express>>` opening tag is detected.
pub fn expression_from_attrs(attrs: &HashMap<String, String>) -> ExpressionState {
    let mut state = ExpressionState::new();

    if let Some(mood_str) = attrs.get("mood") {
        state.mood = Mood::from_str_loose(mood_str);
    }

    if let Some(conf_str) = attrs.get("confidence") {
        match conf_str.to_lowercase().as_str() {
            "low" => state.confidence = Some(0.3),
            "medium" | "med" => state.confidence = Some(0.5),
            "high" => state.confidence = Some(0.8),
            "full" | "max" => state.confidence = Some(1.0),
            _ => {
                if let Ok(v) = conf_str.parse::<f32>() {
                    state.confidence = Some(v.clamp(0.0, 1.0));
                }
            }
        }
    }

    if let Some(danger_str) = attrs.get("danger") {
        state.danger = DangerLevel::from_str_loose(danger_str);
    }

    if let Some(anim_str) = attrs.get("animation") {
        state.animation = Animation::from_str_loose(anim_str);
    }

    if let Some(color_str) = attrs.get("color") {
        state.color_override = parse_hex_color(color_str);
    }

    state
}

/// Parse a hex color string (#RGB, #RRGGBB, #RRGGBBAA) into [f32; 4] RGBA.
fn parse_hex_color(s: &str) -> Option<[f32; 4]> {
    let s = s.strip_prefix('#').unwrap_or(s);
    match s.len() {
        3 => {
            let r = u8::from_str_radix(&s[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&s[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&s[2..3], 16).ok()? * 17;
            Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
        }
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
        }
        8 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            let a = u8::from_str_radix(&s[6..8], 16).ok()?;
            Some([
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0,
                a as f32 / 255.0,
            ])
        }
        _ => None,
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_str(parser: &mut MarkerParser, s: &str) -> Vec<MarkerEvent> {
        let mut events = Vec::new();
        for &byte in s.as_bytes() {
            events.extend(parser.feed(byte));
        }
        events
    }

    fn passthrough_text(events: &[MarkerEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                MarkerEvent::PassThrough(b) => Some(*b as char),
                _ => None,
            })
            .collect()
    }

    fn parser() -> MarkerParser {
        let mut p = MarkerParser::new();
        p.enable();
        p
    }

    #[test]
    fn plain_text_passes_through() {
        let mut parser = parser();
        let events = feed_str(&mut parser, "hello world");
        assert_eq!(passthrough_text(&events), "hello world");
    }

    #[test]
    fn single_angle_bracket_passes_through() {
        let mut parser = parser();
        let events = feed_str(&mut parser, "a < b > c");
        assert_eq!(passthrough_text(&events), "a < b > c");
    }

    #[test]
    fn express_start_end() {
        let mut parser = parser();
        let events = feed_str(
            &mut parser,
            "before<<express mood=creative>>styled<</express>>after",
        );

        let text = passthrough_text(&events);
        assert_eq!(text, "beforestyledafter");

        let start = events.iter().find(|e| matches!(e, MarkerEvent::ExpressStart(_)));
        assert!(start.is_some());
        if let Some(MarkerEvent::ExpressStart(attrs)) = start {
            assert_eq!(attrs.get("mood").unwrap(), "creative");
        }
        assert!(events.iter().any(|e| matches!(e, MarkerEvent::ExpressEnd)));
    }

    #[test]
    fn express_mid_line_still_works() {
        // Express is not SOL-anchored — it can fire mid-line for inline styling.
        let mut parser = parser();
        let events = feed_str(&mut parser, "prefix <<express mood=success>>ok<</express>>");
        assert!(events.iter().any(|e| matches!(e, MarkerEvent::ExpressStart(_))));
        assert_eq!(passthrough_text(&events), "prefix ok");
    }

    #[test]
    fn im_html_opener_passes_through_as_literal() {
        // The in-prose `<<im-html>>` path is currently disabled. Even a
        // properly-formed opener at SOL with newline after `>>` MUST pass
        // through as literal text — overlays go through MCP `draw_html`.
        let mut parser = parser();
        let events = feed_str(
            &mut parser,
            "<<im-html>>\n<div>hello</div>\n<</im-html>>\n",
        );
        let text = passthrough_text(&events);
        assert!(text.contains("<<im-html>>"), "got {:?}", text);
        assert!(text.contains("<</im-html>>"));
        assert!(!events.iter().any(|e| matches!(e, MarkerEvent::HtmlStart(_))));
        assert!(!events.iter().any(|e| matches!(e, MarkerEvent::HtmlEnd(_))));
    }

    #[test]
    fn im_html_with_attrs_also_literal() {
        let mut parser = parser();
        let events = feed_str(
            &mut parser,
            "<<im-html anchor=fixed top=10>>\ncontent\n<</im-html>>\n",
        );
        let text = passthrough_text(&events);
        assert!(text.contains("<<im-html"), "got {:?}", text);
        assert!(!events.iter().any(|e| matches!(e, MarkerEvent::HtmlStart(_))));
    }

    #[test]
    fn im_html_in_prose_is_literal() {
        let mut parser = parser();
        let events = feed_str(
            &mut parser,
            "the <<im-html>> opener won't fire mid-prose",
        );
        let text = passthrough_text(&events);
        assert!(text.contains("<<im-html>>"), "got {:?}", text);
        assert!(!events.iter().any(|e| matches!(e, MarkerEvent::HtmlStart(_))));
    }

    #[test]
    fn unknown_tag_passes_through() {
        let mut parser = parser();
        let events = feed_str(&mut parser, "<<unknown>>text");
        let text = passthrough_text(&events);
        assert!(text.contains("<<unknown"));
    }

    #[test]
    fn auto_close_express_at_limit() {
        let mut parser = parser();
        let mut events = feed_str(&mut parser, "<<express mood=creative>>");
        let long_text = "a".repeat(EXPRESS_AUTO_CLOSE_LIMIT + 10);
        events.extend(feed_str(&mut parser, &long_text));

        assert!(!parser.in_express());
        assert!(events.iter().any(|e| matches!(e, MarkerEvent::ExpressEnd)));
    }

    #[test]
    fn osc_133_reset() {
        let mut parser = parser();
        feed_str(&mut parser, "<<express mood=creative>>");
        assert!(parser.in_express());

        let event = parser.reset();
        assert_eq!(event, Some(MarkerEvent::ExpressEnd));
        assert!(!parser.in_express());
    }

    #[test]
    fn expression_from_attrs_basic() {
        let mut attrs = HashMap::new();
        attrs.insert("mood".into(), "creative".into());
        attrs.insert("confidence".into(), "high".into());
        attrs.insert("danger".into(), "low".into());
        attrs.insert("color".into(), "#ff0000".into());

        let state = expression_from_attrs(&attrs);
        assert_eq!(state.mood, Mood::Creative);
        assert_eq!(state.confidence, Some(0.8));
        assert_eq!(state.danger, DangerLevel::Low);
        assert!(state.color_override.is_some());
        let c = state.color_override.unwrap();
        assert!((c[0] - 1.0).abs() < 0.01);
        assert!((c[1] - 0.0).abs() < 0.01);
    }

    #[test]
    fn parse_hex_colors() {
        assert_eq!(parse_hex_color("#fff"), Some([1.0, 1.0, 1.0, 1.0]));
        assert_eq!(parse_hex_color("#000000"), Some([0.0, 0.0, 0.0, 1.0]));
        assert_eq!(
            parse_hex_color("#ff000080"),
            Some([1.0, 0.0, 0.0, 128.0 / 255.0])
        );
        assert_eq!(parse_hex_color("invalid"), None);
    }

    #[test]
    fn consecutive_markers() {
        let mut parser = parser();
        let events = feed_str(
            &mut parser,
            "<<express mood=success>>good<</express>> <<express mood=error>>bad<</express>>",
        );

        let text = passthrough_text(&events);
        assert_eq!(text, "good bad");

        let starts: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, MarkerEvent::ExpressStart(_)))
            .collect();
        assert_eq!(starts.len(), 2);
    }

    #[test]
    fn express_still_works_when_html_disabled() {
        // Disabling the HTML opener must not affect the express styling tag.
        let mut parser = parser();
        let events = feed_str(
            &mut parser,
            "<<express mood=creative>>styled<</express>>\nplain\n<<im-html>>\nbody\n<</im-html>>\nafter",
        );
        let text = passthrough_text(&events);
        assert!(text.contains("styled"));
        assert!(text.contains("plain"));
        // im-html block is now literal — body and tags all pass through.
        assert!(text.contains("<<im-html>>"), "got {:?}", text);
        assert!(text.contains("body"));
        assert!(text.contains("<</im-html>>"));
        assert!(events.iter().any(|e| matches!(e, MarkerEvent::ExpressStart(_))));
        assert!(!events.iter().any(|e| matches!(e, MarkerEvent::HtmlStart(_))));
    }

    #[test]
    fn legacy_double_angle_html_in_prose_is_literal() {
        // The legacy `<<html>>` syntax never fires — only `<<im-html>>` does.
        let mut parser = parser();
        let events = feed_str(&mut parser, "the legacy <<html>> opener no longer fires");
        let text = passthrough_text(&events);
        assert!(text.contains("<<html>>"), "got {:?}", text);
        assert!(!events.iter().any(|e| matches!(e, MarkerEvent::HtmlStart(_))));
        assert!(!events.iter().any(|e| matches!(e, MarkerEvent::HtmlEnd(_))));
    }

    #[test]
    fn legacy_backtick_fence_passes_through() {
        // The previous backtick-fence form is no longer recognized.
        let mut parser = parser();
        let events = feed_str(
            &mut parser,
            "```im-html\n<div>x</div>\n```\n",
        );
        let text = passthrough_text(&events);
        assert!(text.contains("```im-html"), "got {:?}", text);
        assert!(!events.iter().any(|e| matches!(e, MarkerEvent::HtmlStart(_))));
    }
}
