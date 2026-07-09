//! Link detection for terminal output.
//!
//! Scans text rows for URLs and file paths. OSC 8 hyperlinks are handled
//! separately (stored on cells by the terminal core).
//!
//! Design notes:
//! - No regex crate dependency — keeps WASM binary small.
//! - URLs: require scheme prefix (http://, https://, ftp://, ssh://, file://, git://).
//! - File paths: require `/`, `~/`, `./`, or `../` prefix. Optional `:LINE[:COL]` suffix.
//! - Trailing punctuation trimmed heuristically (`.,;:!?)]}>` not inside balanced pairs).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkKind {
    Url(String),
    File {
        path: String,
        line: Option<u32>,
        col: Option<u32>,
    },
    /// CSS color literal. Either a hex form (`#rgb`, `#rgba`, `#rrggbb`,
    /// `#rrggbbaa`) or a function-notation form (`rgb(...)`, `rgba(...)`).
    /// Stored as the lowercased literal exactly as it appeared in the text,
    /// so the hover preview's copy action preserves the user's original syntax.
    HexColor(String),
    /// Claude Code image-paste placeholder: `[Image #N]`.
    /// Resolved by the host to `~/.claude/image-cache/<session>/<N>.png`.
    ClaudeImage(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub row: u32,
    pub start: u16,
    pub end: u16,
    pub kind: LinkKind,
}

const URL_SCHEMES: &[&str] = &["https://", "http://", "ftp://", "ssh://", "file://", "git://"];

/// Validates a candidate filename extension for bare-name detection.
/// Generic gate (1-10 ASCII alphanumeric chars, at least one letter) — no
/// curated list. Symmetry with the absolute-path branch, which accepts any
/// extension. False positives (e.g. `1.2.3` version strings) are filtered:
/// the all-letter requirement drops pure-digit "extensions", and the stat
/// gate at preview time drops the rest.
fn is_valid_bare_ext(ext: &str) -> bool {
    !ext.is_empty()
        && ext.len() <= 10
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        && ext.bytes().any(|b| b.is_ascii_alphabetic())
}

fn is_url_char(c: char) -> bool {
    match c {
        c if c.is_ascii_alphanumeric() => true,
        '-' | '_' | '.' | '~' | ':' | '/' | '?' | '#' | '[' | ']' | '@' | '!' | '$' | '&'
        | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' | '%' => true,
        _ => false,
    }
}

fn is_path_char(c: char) -> bool {
    match c {
        c if c.is_ascii_alphanumeric() => true,
        '/' | '.' | '_' | '-' | '~' | '+' | '@' | '%' => true,
        _ => false,
    }
}

fn trim_trailing(s: &str) -> &str {
    let mut end = s.len();
    let bytes = s.as_bytes();
    while end > 0 {
        let b = bytes[end - 1];
        let trim = matches!(b, b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']' | b'}' | b'>' | b'\'' | b'"');
        if !trim {
            break;
        }
        // Keep closing bracket if unmatched open exists in remaining text.
        if matches!(b, b')' | b']' | b'}') {
            let (open, close) = match b {
                b')' => (b'(', b')'),
                b']' => (b'[', b']'),
                b'}' => (b'{', b'}'),
                _ => unreachable!(),
            };
            let opens = s[..end - 1].bytes().filter(|&c| c == open).count();
            let closes = s[..end - 1].bytes().filter(|&c| c == close).count();
            if opens > closes {
                break;
            }
        }
        end -= 1;
    }
    &s[..end]
}

/// From position `pos`, scan forward through a candidate path continuation
/// (allowing single spaces between word chunks) and check whether any token
/// ends with a valid extension (see `is_valid_bare_ext`). Stops at double-space,
/// end of bytes, or a non-path non-space char.
///
/// Used to decide whether a lowercase chunk after a space in an absolute path
/// is *part* of the filename (e.g. "immorterm logo black terminal.png") or
/// trailing prose that should stop the match (e.g. "/usr/local/bin/foo now").
fn has_known_ext_ahead(bytes: &[u8], pos: usize) -> bool {
    let mut i = pos;
    let end_cap = (pos + 200).min(bytes.len());
    while i < end_cap {
        let c = bytes[i] as char;
        if c == ' ' {
            // Double-space or trailing space terminates the scan.
            if i + 1 >= bytes.len() || bytes[i + 1] == b' ' {
                return false;
            }
            i += 1;
            continue;
        }
        if c == '.' {
            let ext_start = i + 1;
            let mut j = ext_start;
            while j < end_cap {
                let cj = bytes[j] as char;
                if cj.is_ascii_alphanumeric() { j += 1; } else { break; }
            }
            if j > ext_start && j - ext_start <= 10 {
                // Treat as extension only if followed by word-boundary.
                let at_boundary = j >= bytes.len()
                    || matches!(bytes[j], b' ' | b'\t' | b'\n' | b')' | b']' | b'}'
                        | b',' | b';' | b':' | b'!' | b'?' | b'.' | b'>' | b'"' | b'\'');
                if at_boundary {
                    // SAFETY: ASCII alphanumeric range is valid UTF-8.
                    let ext = std::str::from_utf8(&bytes[ext_start..j]).unwrap_or("");
                    if is_valid_bare_ext(ext) {
                        return true;
                    }
                }
            }
            i = if j > i { j } else { i + 1 };
            continue;
        }
        if !is_path_char(c) {
            return false;
        }
        i += 1;
    }
    false
}

/// Parse trailing `:LINE[:COL]` suffix. Returns (path_without_suffix, line, col).
fn split_line_col(s: &str) -> (&str, Option<u32>, Option<u32>) {
    let bytes = s.as_bytes();
    let mut i = bytes.len();
    // Scan backwards for digits
    let mut tail2_digits_end = i;
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == bytes.len() || i == 0 || bytes[i - 1] != b':' {
        return (s, None, None);
    }
    let tail2: u32 = match s[i..tail2_digits_end].parse() {
        Ok(n) => n,
        Err(_) => return (s, None, None),
    };
    let after_first_colon = i - 1;
    tail2_digits_end = after_first_colon;

    let mut j = after_first_colon;
    while j > 0 && bytes[j - 1].is_ascii_digit() {
        j -= 1;
    }
    if j < after_first_colon && j > 0 && bytes[j - 1] == b':' {
        let tail1: u32 = match s[j..tail2_digits_end].parse() {
            Ok(n) => n,
            Err(_) => return (&s[..after_first_colon], Some(tail2), None),
        };
        (&s[..j - 1], Some(tail1), Some(tail2))
    } else {
        (&s[..after_first_colon], Some(tail2), None)
    }
}

pub fn scan_row(text: &str, row: u32, out: &mut Vec<LinkSpan>) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let byte_to_col: Vec<u16> = {
        let mut v = vec![0u16; text.len() + 1];
        for (col, (b, _)) in chars.iter().enumerate() {
            v[*b] = col as u16;
        }
        v[text.len()] = chars.len() as u16;
        v
    };

    let mut i = 0usize;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        // Claude Code image placeholder: `[Image #N]`. Match exactly — Claude
        // Code emits this verbatim when an image gets pasted via NSPasteboard.
        // N is small in practice (one-shot session counter), cap at 4 digits
        // so a stray `[Image #12345]` in prose can't masquerade as a placeholder.
        if bytes[i] == b'['
            && bytes[i..].len() >= b"[Image #1]".len()
            && bytes[i..].starts_with(b"[Image #")
        {
            let num_start = i + b"[Image #".len();
            let mut num_end = num_start;
            while num_end < bytes.len()
                && num_end - num_start < 4
                && bytes[num_end].is_ascii_digit()
            {
                num_end += 1;
            }
            if num_end > num_start
                && num_end < bytes.len()
                && bytes[num_end] == b']'
            {
                let close = num_end + 1;
                // SAFETY: ASCII digits are valid UTF-8.
                let n: u32 = std::str::from_utf8(&bytes[num_start..num_end])
                    .unwrap_or("")
                    .parse()
                    .unwrap_or(0);
                if n > 0 {
                    out.push(LinkSpan {
                        row,
                        start: byte_to_col[i],
                        end: byte_to_col[close],
                        kind: LinkKind::ClaudeImage(n),
                    });
                    i = close;
                    continue;
                }
            }
        }

        // URL scheme match
        let mut matched_url = false;
        for scheme in URL_SCHEMES {
            if bytes[i..].starts_with(scheme.as_bytes()) {
                let start = i;
                let mut end = i + scheme.len();
                while end < bytes.len() {
                    let c = bytes[end] as char;
                    if !is_url_char(c) {
                        break;
                    }
                    end += 1;
                }
                let raw = &text[start..end];
                let trimmed = trim_trailing(raw);
                if trimmed.len() > scheme.len() {
                    let end_byte = start + trimmed.len();
                    out.push(LinkSpan {
                        row,
                        start: byte_to_col[start],
                        end: byte_to_col[end_byte],
                        kind: LinkKind::Url(trimmed.to_string()),
                    });
                    i = end_byte;
                    matched_url = true;
                    break;
                }
            }
        }
        if matched_url {
            continue;
        }

        // CSS function-notation colors: rgb(...) / rgba(...). Permissive about
        // arg count and separators — modern browsers accept `rgba(r,g,b)` as
        // equivalent to `rgb(r,g,b,1)`, and the user's hover-preview is the
        // ultimate validator (iro.js will reject garbage). Word boundary on
        // the left so `argb(...)` and similar don't trigger.
        if (bytes[i] == b'r' || bytes[i] == b'R')
            && (bytes[i..].len() >= 4)
        {
            let lower3 = [bytes[i] | 0x20, bytes[i + 1] | 0x20, bytes[i + 2] | 0x20];
            let is_rgb = lower3 == *b"rgb";
            if is_rgb {
                // Determine prefix: "rgb(" or "rgba(".
                let prefix_len = if bytes[i..].len() >= 5
                    && (bytes[i + 3] == b'a' || bytes[i + 3] == b'A')
                    && bytes[i + 4] == b'('
                {
                    5
                } else if bytes[i + 3] == b'(' {
                    4
                } else {
                    0
                };
                if prefix_len > 0 {
                    let left_ok = i == 0
                        || !matches!(bytes[i - 1],
                            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
                    if left_ok {
                        // Scan to matching `)`. Cap at 80 chars to avoid runaway.
                        let scan_start = i + prefix_len;
                        let cap = (scan_start + 76).min(bytes.len());
                        let mut end = scan_start;
                        let mut found_close = false;
                        while end < cap {
                            let c = bytes[end];
                            if c == b')' {
                                end += 1;
                                found_close = true;
                                break;
                            }
                            // Only chars that legitimately appear inside CSS
                            // rgb()/rgba(): digits, separators (comma/space/slash),
                            // decimal point, percent.
                            if !matches!(c,
                                b'0'..=b'9' | b',' | b' ' | b'\t' | b'.' | b'%' | b'/')
                            {
                                break;
                            }
                            end += 1;
                        }
                        if found_close {
                            // Sanity: must contain at least one digit so we
                            // don't match `rgb()` or `rgb(   )`.
                            let inner = &bytes[scan_start..end - 1];
                            let has_digit = inner.iter().any(|b| b.is_ascii_digit());
                            // Right word boundary so `rgb(0,0,0)foo` doesn't
                            // grab adjacent identifiers via the swatch.
                            let right_ok = end >= bytes.len()
                                || !matches!(bytes[end],
                                    b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
                            if has_digit && right_ok {
                                let raw = &text[i..end];
                                out.push(LinkSpan {
                                    row,
                                    start: byte_to_col[i],
                                    end: byte_to_col[end],
                                    kind: LinkKind::HexColor(raw.to_ascii_lowercase()),
                                });
                                i = end;
                                continue;
                            }
                        }
                    }
                }
            }
        }

        // Hex color detection: #rgb / #rgba / #rrggbb / #rrggbbaa at a word
        // boundary. Sits between URL and path detection — URLs are matched
        // first so anchors like `example.com#section` don't trigger; bare
        // hashes in shell prompts (`# comment`) need a hex-only continuation.
        if bytes[i] == b'#' {
            let left_ok = i == 0
                || !matches!(bytes[i - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
            if left_ok {
                let mut end = i + 1;
                while end < bytes.len() && (bytes[end] as char).is_ascii_hexdigit() {
                    end += 1;
                }
                let hex_len = end - i - 1;
                let valid_len = matches!(hex_len, 3 | 4 | 6 | 8);
                let right_ok = end >= bytes.len()
                    || !matches!(bytes[end], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
                if valid_len && right_ok {
                    let raw = &text[i..end];
                    out.push(LinkSpan {
                        row,
                        start: byte_to_col[i],
                        end: byte_to_col[end],
                        kind: LinkKind::HexColor(raw.to_ascii_lowercase()),
                    });
                    i = end;
                    continue;
                }
            }
        }

        // Bare URL detection: `www.` prefix at word boundary. Auto-prepend https:// when opening.
        let prev_boundary = i == 0
            || matches!(bytes[i - 1], b' ' | b'\t' | b'(' | b'[' | b'{' | b'\'' | b'"' | b'<' | b'=');
        if prev_boundary && bytes[i..].starts_with(b"www.") {
            let start = i;
            let mut end = i + 4;
            while end < bytes.len() {
                let c = bytes[end] as char;
                if !is_url_char(c) {
                    break;
                }
                end += 1;
            }
            let raw = &text[start..end];
            let trimmed = trim_trailing(raw);
            // Require a dot after "www." (e.g. www.example.com) so we don't match "www.")
            if trimmed.len() > 5 && trimmed[4..].contains('.') {
                let end_byte = start + trimmed.len();
                out.push(LinkSpan {
                    row,
                    start: byte_to_col[start],
                    end: byte_to_col[end_byte],
                    kind: LinkKind::Url(format!("https://{}", trimmed)),
                });
                i = end_byte;
                continue;
            }
        }

        // Path detection: must start with / ~/ ./ ../ and be preceded by whitespace/start/quote.
        let prev_ok = i == 0
            || matches!(bytes[i - 1], b' ' | b'\t' | b'(' | b'[' | b'{' | b'\'' | b'"' | b'<' | b'=');
        let starts_path = prev_ok
            && (bytes[i] == b'/'
                || bytes[i..].starts_with(b"~/")
                || bytes[i..].starts_with(b"./")
                || bytes[i..].starts_with(b"../"));
        if starts_path {
            let start = i;
            let mut end = i;
            while end < bytes.len() {
                let c = bytes[end] as char;
                if c == ':' {
                    // Tentatively include for :line:col
                    end += 1;
                    continue;
                }
                if c == ' ' && end + 1 < bytes.len() {
                    let nxt = bytes[end + 1] as char;
                    // Uppercase/digit: assume directory-like name (e.g. "Desktop Pictures",
                    // "ScreenRecording 00-03-58"). Stat gate drops false positives.
                    let upper_or_digit = nxt.is_ascii_uppercase() || nxt.is_ascii_digit();
                    // Lowercase: ambiguous — only extend if a known file extension appears
                    // later in the row before a double-space or end. This keeps
                    // "/Users/.../immorterm logo black terminal.png" working without
                    // eating "/usr/local/bin/foo now" or "/tmp/x.png and more".
                    let lowercase_with_ext = nxt.is_ascii_lowercase()
                        && has_known_ext_ahead(bytes, end + 1);
                    if upper_or_digit || lowercase_with_ext {
                        end += 1;
                        continue;
                    }
                }
                if !is_path_char(c) {
                    break;
                }
                end += 1;
            }
            let raw = &text[start..end];
            let trimmed = trim_trailing(raw);
            // Minimum: must contain at least one non-prefix char
            if trimmed.len() > 2 && trimmed.contains('/') {
                let (path, line, col) = split_line_col(trimmed);
                if path.len() > 1 {
                    let end_byte = start + trimmed.len();
                    out.push(LinkSpan {
                        row,
                        start: byte_to_col[start],
                        end: byte_to_col[end_byte],
                        kind: LinkKind::File {
                            path: path.to_string(),
                            line,
                            col,
                        },
                    });
                    i = end_byte;
                    continue;
                }
            }
        }

        // Bare filename detection: `name.ext` preceded by boundary char.
        // Stat gate at preview time drops false positives.
        if prev_ok {
            let c0 = bytes[i] as char;
            // Allow leading '.' for dotfiles/dotdirs (.claude/CLAUDE.md, .env, etc.)
            // only when followed by an alphanumeric to avoid matching "." or "..".
            let dotfile_start = c0 == '.'
                && i + 1 < bytes.len()
                && (bytes[i + 1] as char).is_ascii_alphanumeric();
            if c0.is_ascii_alphanumeric() || c0 == '_' || dotfile_start {
                let start = i;
                let mut end = i;
                while end < bytes.len() {
                    let c = bytes[end] as char;
                    if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/') {
                        end += 1;
                    } else {
                        break;
                    }
                }
                // Include optional :line[:col]
                let mut scan_end = end;
                if scan_end < bytes.len() && bytes[scan_end] == b':' {
                    let mut j = scan_end + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() { j += 1; }
                    if j > scan_end + 1 {
                        scan_end = j;
                        if scan_end < bytes.len() && bytes[scan_end] == b':' {
                            let mut k = scan_end + 1;
                            while k < bytes.len() && bytes[k].is_ascii_digit() { k += 1; }
                            if k > scan_end + 1 { scan_end = k; }
                        }
                    }
                }
                let raw = &text[start..scan_end];
                let trimmed = trim_trailing(raw);
                if let Some(dot) = trimmed.rfind('.') {
                    let name_part = &trimmed[..dot];
                    let after_dot = &trimmed[dot + 1..];
                    let (ext_str, _, _) = split_line_col(after_dot);
                    if is_valid_bare_ext(ext_str) && !name_part.is_empty() {
                        let (path, line, col) = split_line_col(trimmed);
                        let end_byte = start + trimmed.len();
                        out.push(LinkSpan {
                            row,
                            start: byte_to_col[start],
                            end: byte_to_col[end_byte],
                            kind: LinkKind::File {
                                path: path.to_string(),
                                line,
                                col,
                            },
                        });
                        i = end_byte;
                        continue;
                    }
                }
            }
        }

        i += 1;
    }
}

pub fn hit_test(spans: &[LinkSpan], row: u32, col: u16) -> Option<&LinkSpan> {
    spans
        .iter()
        .find(|s| s.row == row && col >= s.start && col < s.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(text: &str) -> Vec<LinkSpan> {
        let mut out = Vec::new();
        scan_row(text, 0, &mut out);
        out
    }

    #[test]
    fn url_basic() {
        let r = scan("visit https://example.com/foo today");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, LinkKind::Url("https://example.com/foo".into()));
    }

    #[test]
    fn url_trailing_punct() {
        let r = scan("see https://example.com.");
        assert_eq!(r[0].kind, LinkKind::Url("https://example.com".into()));
    }

    #[test]
    fn url_paren_balanced() {
        let r = scan("(https://en.wikipedia.org/wiki/Rust_(programming_language))");
        assert_eq!(
            r[0].kind,
            LinkKind::Url("https://en.wikipedia.org/wiki/Rust_(programming_language)".into())
        );
    }

    #[test]
    fn path_absolute() {
        let r = scan("edit /usr/local/bin/foo now");
        assert_eq!(r.len(), 1);
        if let LinkKind::File { path, line, col } = &r[0].kind {
            assert_eq!(path, "/usr/local/bin/foo");
            assert!(line.is_none() && col.is_none());
        } else {
            panic!("not file");
        }
    }

    #[test]
    fn path_with_line_col() {
        let r = scan("  at /src/app.rs:42:7 crashed");
        if let LinkKind::File { path, line, col } = &r[0].kind {
            assert_eq!(path, "/src/app.rs");
            assert_eq!(*line, Some(42));
            assert_eq!(*col, Some(7));
        } else {
            panic!();
        }
    }

    #[test]
    fn path_tilde() {
        let r = scan("open ~/.config/app.toml");
        if let LinkKind::File { path, .. } = &r[0].kind {
            assert_eq!(path, "~/.config/app.toml");
        } else {
            panic!();
        }
    }

    #[test]
    fn path_relative() {
        let r = scan("see ./src/main.rs:10");
        if let LinkKind::File { path, line, .. } = &r[0].kind {
            assert_eq!(path, "./src/main.rs");
            assert_eq!(*line, Some(10));
        } else {
            panic!();
        }
    }

    #[test]
    fn no_bare_word_path() {
        let r = scan("just some text without paths");
        assert!(r.is_empty());
    }

    #[test]
    fn mixed_url_and_path() {
        let r = scan("https://x.com and /tmp/y");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn claude_style_update_parens() {
        let r = scan("⏺ Update(src/gpu-terminal.ts)");
        println!("{:?}", r);
        assert_eq!(r.len(), 1, "expected 1 link, got {:?}", r);
        if let LinkKind::File { path, .. } = &r[0].kind {
            assert_eq!(path, "src/gpu-terminal.ts");
        } else { panic!("not file"); }
    }

    #[test]
    fn bare_relative_with_slash() {
        let r = scan("resources/gpu-terminal.css");
        println!("{:?}", r);
        assert_eq!(r.len(), 1, "expected 1 link, got {:?}", r);
        if let LinkKind::File { path, .. } = &r[0].kind {
            assert_eq!(path, "resources/gpu-terminal.css");
        } else { panic!("not file"); }
    }

    #[test]
    fn claude_style_update_css() {
        let r = scan("⏺ Update(resources/gpu-terminal.css)");
        println!("{:?}", r);
        assert_eq!(r.len(), 1, "expected 1 link, got {:?}", r);
        if let LinkKind::File { path, .. } = &r[0].kind {
            assert_eq!(path, "resources/gpu-terminal.css");
        } else { panic!("not file"); }
    }

    #[test]
    fn absolute_mov_bare() {
        let r = scan("video /home/user/Downloads/Arthur.mov ok");
        println!("{:?}", r);
        assert_eq!(r.len(), 1);
        if let LinkKind::File { path, .. } = &r[0].kind {
            assert_eq!(path, "/home/user/Downloads/Arthur.mov");
        } else { panic!(); }
    }

    #[test]
    fn tmp_path_with_digits_and_dashes() {
        let r = scan("saved to /var/folders/9v/6rjwyfq57g73c_yfc3fzl48h0000gn/T/immorterm-paste-1776695919495.png");
        assert_eq!(r.len(), 1, "got {:?}", r);
        if let LinkKind::File { path, line, col } = &r[0].kind {
            assert_eq!(path, "/var/folders/9v/6rjwyfq57g73c_yfc3fzl48h0000gn/T/immorterm-paste-1776695919495.png");
            assert!(line.is_none() && col.is_none(), "line/col should be None, got {:?}/{:?}", line, col);
        } else { panic!("not file"); }
    }

    #[test]
    fn absolute_path_with_lowercase_spaces() {
        // Regression: paths like "/Users/x/LinkedIn/Immorterm/immorterm logo black terminal.png"
        // where chunks after spaces are lowercase must still be detected as one path.
        let r = scan("open /home/user/Pictures/logo.png now");
        assert_eq!(r.len(), 1, "got {:?}", r);
        if let LinkKind::File { path, .. } = &r[0].kind {
            assert_eq!(path, "/home/user/Pictures/logo.png");
        } else { panic!("not file"); }
    }

    #[test]
    fn dotdir_bare_path() {
        let r = scan("edit .claude/CLAUDE.md now");
        assert_eq!(r.len(), 1, "got {:?}", r);
        if let LinkKind::File { path, .. } = &r[0].kind {
            assert_eq!(path, ".claude/CLAUDE.md");
        } else { panic!("not file"); }
    }

    #[test]
    fn hex_color_six_digit() {
        let r = scan("color: #C9A961;");
        assert_eq!(r.len(), 1, "got {:?}", r);
        assert_eq!(r[0].kind, LinkKind::HexColor("#c9a961".into()));
    }

    #[test]
    fn hex_color_three_digit() {
        let r = scan("bg #fff and");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, LinkKind::HexColor("#fff".into()));
    }

    #[test]
    fn hex_color_eight_digit_alpha() {
        let r = scan("rgba #11223344 here");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, LinkKind::HexColor("#11223344".into()));
    }

    #[test]
    fn hex_color_invalid_length_skipped() {
        // 5-char hex is not a valid CSS color length
        let r = scan("foo #abcde bar");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn hex_color_inside_url_not_matched() {
        // URL detection runs first and consumes the whole span including the fragment
        let r = scan("see https://example.com#section");
        assert_eq!(r.len(), 1);
        assert!(matches!(r[0].kind, LinkKind::Url(_)));
    }

    #[test]
    fn hex_color_left_word_boundary_required() {
        // `foo#fff` has alpha char before # — should not match
        let r = scan("foo#fff bar");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn hex_color_right_word_boundary_required() {
        // `#abcz` is not a 3-char hex with alpha tail — should not match
        let r = scan("foo #abcz bar");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn hex_color_at_line_start() {
        let r = scan("#f9c74f");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, LinkKind::HexColor("#f9c74f".into()));
    }

    #[test]
    fn rgba_three_arg_user_format() {
        // The exact format the user asked us to support: rgba() with only 3
        // values. Modern CSS treats this as equivalent to rgb(r,g,b).
        let r = scan("background: rgba(107,63,214);");
        assert_eq!(r.len(), 1, "got {:?}", r);
        assert_eq!(r[0].kind, LinkKind::HexColor("rgba(107,63,214)".into()));
    }

    #[test]
    fn rgb_basic() {
        let r = scan("color: rgb(255, 0, 128);");
        assert_eq!(r.len(), 1, "got {:?}", r);
        assert_eq!(r[0].kind, LinkKind::HexColor("rgb(255, 0, 128)".into()));
    }

    #[test]
    fn rgba_four_arg_with_alpha() {
        let r = scan("border: rgba(20, 30, 40, 0.5);");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, LinkKind::HexColor("rgba(20, 30, 40, 0.5)".into()));
    }

    #[test]
    fn rgb_uppercase() {
        // Function name is case-insensitive in CSS.
        let r = scan("RGB(10, 20, 30)");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, LinkKind::HexColor("rgb(10, 20, 30)".into()));
    }

    #[test]
    fn rgb_modern_slash_alpha() {
        // CSS Color 4 syntax: space-separated with `/` for alpha.
        let r = scan("c rgb(107 63 214 / 0.5) end");
        assert_eq!(r.len(), 1, "got {:?}", r);
        assert_eq!(r[0].kind, LinkKind::HexColor("rgb(107 63 214 / 0.5)".into()));
    }

    #[test]
    fn rgb_left_word_boundary_required() {
        // `argb(...)` / `srgb(...)` etc. should not match.
        let r = scan("foo argb(1,2,3) bar");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn rgb_right_word_boundary_required() {
        // Suffix would extend the literal beyond `)` — reject so we don't
        // paint a swatch over a half-identifier.
        let r = scan("foo rgb(1,2,3)abc bar");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn rgb_empty_args_rejected() {
        let r = scan("foo rgb() bar");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn rgb_unclosed_rejected() {
        let r = scan("foo rgb(1,2,3 oops");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn claude_image_basic() {
        let r = scan("attached: [Image #1] for review");
        assert_eq!(r.len(), 1, "got {:?}", r);
        assert_eq!(r[0].kind, LinkKind::ClaudeImage(1));
    }

    #[test]
    fn claude_image_multi_digit() {
        let r = scan("see [Image #42] above");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, LinkKind::ClaudeImage(42));
    }

    #[test]
    fn claude_image_zero_rejected() {
        let r = scan("[Image #0]");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn claude_image_too_many_digits_rejected() {
        let r = scan("[Image #12345]");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn claude_image_missing_close_rejected() {
        let r = scan("[Image #1 oops");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn bare_relative_mp4() {
        // Regression: video extensions like mp4/mov/webm must work for bare
        // relative paths. Previously gated on a curated list that excluded
        // video formats; absolute paths accepted them, bare relatives didn't.
        let r = scan("see demos/out/color-palette/final.mp4 for ref");
        assert_eq!(r.len(), 1, "got {:?}", r);
        if let LinkKind::File { path, .. } = &r[0].kind {
            assert_eq!(path, "demos/out/color-palette/final.mp4");
        } else { panic!("not file"); }
    }

    #[test]
    fn bare_relative_arbitrary_ext() {
        // Generic ext gate: any 1-10 alphanumeric chars with at least one
        // letter is accepted. Stat gate at preview time filters non-files.
        let r = scan("check src/data.bin and bar");
        assert_eq!(r.len(), 1, "got {:?}", r);
        if let LinkKind::File { path, .. } = &r[0].kind {
            assert_eq!(path, "src/data.bin");
        } else { panic!("not file"); }
    }

    #[test]
    fn bare_digit_only_ext_rejected() {
        // Version-string false positive: `1.2.3` would otherwise be matched
        // as filename `1.2` with ext `3`. The letter requirement rejects it.
        let r = scan("upgraded to version 1.2.3 today");
        assert!(r.is_empty(), "got {:?}", r);
    }

    #[test]
    fn hit_test_works() {
        let mut spans = Vec::new();
        scan_row("a https://x.com b", 5, &mut spans);
        let s = &spans[0];
        assert!(hit_test(&spans, 5, s.start).is_some());
        assert!(hit_test(&spans, 5, s.end).is_none());
        assert!(hit_test(&spans, 6, s.start).is_none());
    }
}
