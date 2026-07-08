//! Markdown + code-fence syntax highlighting — port of the bundled
//! `marked` UMD and `shiki` calls in gpu-terminal.html.
//!
//! Contract:
//!   POST /api/v1/markdown
//!   { text: string, flavor?: "gfm", theme?: "dark"|"light" }
//!   → { html: string }
//!
//! Highlighter instances are expensive to build, so we memoize the
//! SyntaxSet + ThemeSet in OnceLock and reuse them for the lifetime of
//! the process.

use std::sync::OnceLock;

use axum::Json;
use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use syntect::highlighting::ThemeSet;
use syntect::html::highlighted_html_for_string;
use syntect::parsing::SyntaxSet;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<ThemeSet> = OnceLock::new();

fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}
fn themes() -> &'static ThemeSet {
    THEMES.get_or_init(ThemeSet::load_defaults)
}

#[derive(Debug, Deserialize)]
pub struct MarkdownRequest {
    pub text: String,
    #[serde(default)]
    pub theme: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MarkdownResponse {
    pub html: String,
}

/// POST /api/v1/markdown
pub async fn render_markdown(Json(req): Json<MarkdownRequest>) -> Json<MarkdownResponse> {
    let theme_name = match req.theme.as_deref() {
        Some("light") => "InspiredGitHub",
        // Anything else (including None) defaults to the dark VS Code
        // terminal palette the webview always uses.
        _ => "base16-ocean.dark",
    };
    Json(MarkdownResponse { html: render(&req.text, theme_name) })
}

/// Core renderer — exposed separately so link-tooltip + task-board code
/// can call it without an HTTP round-trip when they already own the hub
/// process. Matches `marked.parse(text)` semantics (GFM: tables,
/// strikethrough, task lists, autolinks).
pub fn render(text: &str, theme_name: &str) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_SMART_PUNCTUATION);

    let parser = Parser::new_ext(text, opts);
    let ss = syntaxes();
    let ts = themes();
    let theme = ts
        .themes
        .get(theme_name)
        .or_else(|| ts.themes.get("base16-ocean.dark"))
        .expect("at least the default dark theme must be present");

    // Intercept fenced code blocks and inline-highlight them via syntect,
    // passing the rest straight through to the default HTML renderer.
    let mut out_events: Vec<Event> = Vec::new();
    let mut in_fence: Option<String> = None;
    let mut buf = String::new();
    for ev in parser {
        match ev {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang))) => {
                in_fence = Some(lang.to_string());
                buf.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                let lang = in_fence.take().unwrap_or_default();
                let syntax = if lang.is_empty() {
                    ss.find_syntax_plain_text()
                } else {
                    ss.find_syntax_by_token(&lang)
                        .unwrap_or_else(|| ss.find_syntax_plain_text())
                };
                let html = highlighted_html_for_string(&buf, ss, syntax, theme)
                    .unwrap_or_else(|_| format!("<pre><code>{}</code></pre>", html_escape(&buf)));
                out_events.push(Event::Html(html.into()));
            }
            Event::Text(t) if in_fence.is_some() => {
                buf.push_str(&t);
            }
            other => out_events.push(other),
        }
    }
    let mut html_out = String::new();
    html::push_html(&mut html_out, out_events.into_iter());
    html_out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paragraph_renders() {
        let out = render("hello **world**", "base16-ocean.dark");
        assert!(out.contains("<strong>world</strong>"));
    }

    #[test]
    fn gfm_task_list_renders() {
        let out = render("- [x] done\n- [ ] todo", "base16-ocean.dark");
        assert!(out.contains("type=\"checkbox\""));
    }

    #[test]
    fn fenced_code_highlights() {
        let out = render("```rust\nfn x() {}\n```", "base16-ocean.dark");
        // syntect wraps in <pre style="..."> with color spans.
        assert!(out.contains("<pre"));
        assert!(out.contains("color:"));
    }

    #[test]
    fn unknown_lang_falls_back_to_plain() {
        let out = render("```xyzlang\nhello\n```", "base16-ocean.dark");
        assert!(out.contains("hello"));
    }
}
