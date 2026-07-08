//! Claude Code image-paste cache: serves files from
//! `~/.claude/image-cache/<session>/<N>.png`.
//!
//! When a user pastes an image into Claude Code's TUI, it stores the bytes
//! at this well-known path and renders `[Image #N]` in the terminal. This
//! endpoint lets the GPU terminal hover-preview those placeholders.

use axum::extract::{Path, Query};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;

use std::collections::HashMap;
use std::path::PathBuf;

/// Validate `<session>` looks like a UUID-ish path component (alphanumeric
///     + dash, ≤ 64 chars). Rejects `..`, slashes, dots — anything that could
///     escape `~/.claude/image-cache/`.
fn safe_segment(s: &str, max_len: usize) -> bool {
    !s.is_empty()
        && s.len() <= max_len
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// GET /api/v1/claude-cache/image/{session}/{n}
///
/// `n` is the image counter from `[Image #N]`. We append `.png` server-side
/// — Claude Code only writes PNGs to this directory.
///
/// Query: `?download=1` adds Content-Disposition: attachment so a webview
/// `<a href>` click triggers a Save dialog instead of inline display.
pub async fn image(
    Path((session, n)): Path<(String, u32)>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if !safe_segment(&session, 64) || n == 0 || n > 9999 {
        return (StatusCode::NOT_FOUND, "invalid path").into_response();
    }

    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => return (StatusCode::INTERNAL_SERVER_ERROR, "no HOME").into_response(),
    };

    let path = home
        .join(".claude")
        .join("image-cache")
        .join(&session)
        .join(format!("{}.png", n));

    let download = q.get("download").is_some_and(|v| v == "1" || v == "true");

    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, "image/png".parse().unwrap());
            headers.insert(
                header::CACHE_CONTROL,
                "private, max-age=31536000, immutable".parse().unwrap(),
            );
            if download {
                let disp = format!("attachment; filename=\"claude-image-{}.png\"", n);
                if let Ok(v) = disp.parse() {
                    headers.insert(header::CONTENT_DISPOSITION, v);
                }
            }
            (StatusCode::OK, headers, bytes).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "image not found").into_response(),
    }
}
