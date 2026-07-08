//! Legacy API shim — matches the /api/* endpoints gpu-terminal.html expects
//! in standalone mode. Wraps the canonical /api/v1/registry/* endpoints and
//! translates snake_case → camelCase for browser/Tauri clients.
//!
//! Do NOT add new endpoints here. New work should target /api/v1/* directly.

use axum::Json;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{json, Value};

use crate::routes::registry::{spawn_session, SpawnRequest};

/// GET /api/gpu-probe — one-shot diagnostic. Hub logs whether navigator.gpu
/// was defined in the calling webview. Used to verify WKWebView/WebView2
/// WebGPU availability without needing dev tools access.
pub async fn gpu_probe(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let has = q.get("has").cloned().unwrap_or_else(|| "unknown".into());
    let adapter = q.get("adapter").cloned().unwrap_or_default();
    let ua = q.get("ua").cloned().unwrap_or_default();
    tracing::info!("[gpu-probe] navigator.gpu={} adapter={} ua={}", has, adapter, ua);
    Json(json!({ "ok": true, "has": has }))
}

/// POST /api/dev-log — arbitrary diagnostic messages from webview runtime.
/// Use for capturing JS stack traces out-of-band when devtools automation
/// is flaky. Accepts {level, msg, stack} JSON.
pub async fn dev_log(Json(payload): Json<Value>) -> Json<Value> {
    let level = payload.get("level").and_then(|v| v.as_str()).unwrap_or("info");
    let msg = payload.get("msg").and_then(|v| v.as_str()).unwrap_or("");
    let stack = payload.get("stack").and_then(|v| v.as_str()).unwrap_or("");
    tracing::info!("[dev-log] [{}] {} | stack: {}", level, msg, stack);
    Json(json!({ "ok": true }))
}

/// GET /api/info — minimal project metadata for standalone bootstrap.
pub async fn info() -> Json<Value> {
    let cwd = std::env::current_dir().ok();
    let project_dir = cwd
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let project_name = cwd
        .as_ref()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "standalone".to_string());
    Json(json!({
        "projectName": project_name,
        "projectDir": project_dir,
    }))
}

/// POST /api/open-file — open a file/URL in the host's default handler.
/// Body: { path: string, reveal?: bool }. When reveal=true on macOS we pass
/// `-R` to show the file in Finder instead of opening it.
pub async fn open_file(Json(payload): Json<Value>) -> Json<Value> {
    let raw = payload
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if raw.is_empty() {
        return Json(json!({ "error": "missing path" }));
    }
    // Expand `~/` and bare `~` — `open` and `xdg-open` get the path verbatim
    // (no shell), so tilde expansion has to happen here.
    let path = if raw == "~" {
        std::env::var("HOME").unwrap_or(raw.clone())
    } else if let Some(rest) = raw.strip_prefix("~/") {
        match std::env::var("HOME") {
            Ok(h) if !h.is_empty() => format!("{}/{}", h.trim_end_matches('/'), rest),
            _ => raw.clone(),
        }
    } else {
        raw.clone()
    };
    // `reveal` is only consumed in the macOS cfg branch — linux/windows builds see it unused.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let reveal = payload.get("reveal").and_then(|v| v.as_bool()).unwrap_or(false);

    #[cfg(target_os = "macos")]
    let result = {
        let mut cmd = std::process::Command::new("open");
        if reveal { cmd.arg("-R"); }
        cmd.arg(&path).spawn().map(|_| ())
    };
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", &path])
        .spawn()
        .map(|_| ());
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let result = std::process::Command::new("xdg-open")
        .arg(&path)
        .spawn()
        .map(|_| ());

    match result {
        Ok(_) => Json(json!({ "ok": true })),
        Err(e) => Json(json!({ "error": format!("{}", e) })),
    }
}

/// GET /api/url-preview?url=... — fetches a URL with a browser-ish User-Agent,
/// scrapes og:title/og:description/og:image/og:site_name/og:video from the
/// <head>, follows up to 3 redirects, caps payload at 128 KiB. Returns null
/// fields when the page has no og: tags so the tooltip falls back to raw URL.
pub async fn url_preview(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let url = q.get("url").cloned().unwrap_or_default();
    if url.is_empty() || !(url.starts_with("http://") || url.starts_with("https://")) {
        return Json(json!(null));
    }
    let client = match reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (ImmorTerm link preview)")
        .timeout(std::time::Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Json(json!(null)),
    };
    let resp = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return Json(json!(null)),
    };
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !ctype.contains("text/html") && !ctype.contains("application/xhtml") {
        return Json(json!(null));
    }
    // Cap at 128 KiB to match extension behavior. reqwest's .bytes() buffers
    // the whole body; we just truncate before parsing.
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => return Json(json!(null)),
    };
    let cap = bytes.len().min(131072);
    let html = String::from_utf8_lossy(&bytes[..cap]).to_string();
    let head_end = html.to_ascii_lowercase().find("</head>")
        .map(|i| i + 7)
        .unwrap_or_else(|| html.len().min(50000));
    let head = &html[..head_end.min(html.len())];

    fn get_meta(head: &str, prop: &str) -> Option<String> {
        let a = format!(r#"<meta[^>]+(?:property|name)=["']{0}["'][^>]+content=["']([^"']+)["']"#, regex_escape(prop));
        let b = format!(r#"<meta[^>]+content=["']([^"']+)["'][^>]+(?:property|name)=["']{0}["']"#, regex_escape(prop));
        let re_a = regex::Regex::new(&a).ok()?;
        let re_b = regex::Regex::new(&b).ok()?;
        re_a.captures(head)
            .or_else(|| re_b.captures(head))
            .and_then(|c| c.get(1))
            .map(|m| decode_html_entities(m.as_str()))
    }
    fn decode_html_entities(s: &str) -> String {
        s.replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&#x27;", "'")
            .replace("&apos;", "'")
    }
    fn regex_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            if ".+*?()[]{}|^$\\".contains(c) { out.push('\\'); }
            out.push(c);
        }
        out
    }
    fn absolute(base: &str, candidate: Option<String>) -> Option<String> {
        let c = candidate?;
        if c.starts_with("http://") || c.starts_with("https://") { return Some(c); }
        url::Url::parse(base).ok().and_then(|b| b.join(&c).ok()).map(|u| u.to_string())
    }

    let title = get_meta(head, "og:title").or_else(|| {
        regex::Regex::new(r"(?i)<title[^>]*>([^<]+)</title>")
            .ok()
            .and_then(|re| re.captures(head))
            .and_then(|c| c.get(1))
            .map(|m| decode_html_entities(m.as_str().trim()))
    });
    let description = get_meta(head, "og:description").or_else(|| get_meta(head, "description"));
    let image = absolute(&url, get_meta(head, "og:image"));
    let site_name = get_meta(head, "og:site_name");
    let video = get_meta(head, "og:video:secure_url")
        .or_else(|| get_meta(head, "og:video:url"))
        .or_else(|| get_meta(head, "og:video"));
    let video_type = get_meta(head, "og:video:type");
    let playable = video_type.as_deref()
        .map(|t| t.starts_with("video/mp4") || t.starts_with("video/webm") || t.starts_with("video/ogg"))
        .unwrap_or(true);

    Json(json!({
        "title": title,
        "description": description,
        "image": image,
        "siteName": site_name,
        "video": if playable { absolute(&url, video) } else { None },
        "videoType": if playable { video_type } else { None },
    }))
}

/// GET /api/ls?path=<absolute-dir> — directory listing for the cmd-hover
/// Reveal tree view. Returns one row per entry: name, kind (dir|file),
/// size (bytes), mtime (unix seconds). Hidden entries (`.foo`) included
/// — the file-tree widget filters them via a toggle. Matches the shape
/// of `/api/v1/remotes/<name>/ls` so the webview's tree code is one
/// path for both hosts.
pub async fn ls(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let path = q.get("path").cloned().unwrap_or_default();
    if path.is_empty() {
        return Json(json!({"error": "missing path", "entries": []}));
    }
    let dir = std::path::Path::new(&path);
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            return Json(json!({
                "error": format!("read_dir: {e}"),
                "entries": []
            }));
        }
    };
    let mut entries: Vec<Value> = read
        .filter_map(|res| res.ok())
        .filter_map(|de| {
            let name = de.file_name().to_string_lossy().to_string();
            let md = de.metadata().ok()?;
            let kind = if md.is_dir() { "dir" } else { "file" };
            let size = md.len();
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Some(json!({
                "name": name,
                "kind": kind,
                "size": size,
                "mtime": mtime,
            }))
        })
        .collect();
    entries.sort_by(|a, b| {
        // Directories first, then alphabetic by name.
        let ad = a["kind"].as_str() == Some("dir");
        let bd = b["kind"].as_str() == Some("dir");
        match (ad, bd) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a["name"].as_str().cmp(&b["name"].as_str()),
        }
    });
    Json(json!({ "path": path, "entries": entries }))
}

/// GET /api/link-exists?path=...&cwd=... — stat check used by in-terminal
/// file-link tooltips before offering "open in editor". Optional `cwd`
/// resolves relative `path` values (bare filenames in Claude's output)
/// against the session's working directory — matches the VS Code
/// extension's `resolveLinkPathAsync` behavior so standalone hub
/// clients get the same relative-path resolution.
pub async fn link_exists(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let raw = q.get("path").cloned().unwrap_or_default();
    let cwd = q.get("cwd").cloned().unwrap_or_default();
    if raw.is_empty() {
        return Json(json!({ "exists": false }));
    }
    let p = std::path::Path::new(&raw);
    let candidates = if p.is_absolute() {
        vec![p.to_path_buf()]
    } else if !cwd.is_empty() {
        // Try cwd-joined first, then literal (in case it really is just
        // a bare-existing-in-pwd file the server's running from).
        vec![std::path::Path::new(&cwd).join(&raw), p.to_path_buf()]
    } else {
        vec![p.to_path_buf()]
    };
    let exists = candidates.iter().any(|c| c.exists());
    Json(json!({ "exists": exists }))
}

/// GET /api/font — returns a base64-encoded monospace font file for the WASM
/// terminal renderer. Mirrors the extension's findPlatformDefaultFont() so
/// standalone Tauri/browser clients get the same typeface VS Code users see.
pub async fn font() -> Json<Value> {
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[
        ("Menlo", &["/System/Library/Fonts/Menlo.ttc"]),
        (
            "SF Mono",
            &[
                "/System/Library/Fonts/SFMono.ttf",
                "/Library/Fonts/SF-Mono-Regular.otf",
            ],
        ),
        ("Monaco", &["/System/Library/Fonts/Monaco.ttf"]),
    ];
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, &[&str])] = &[
        (
            "Cascadia Mono",
            &[r"C:\Windows\Fonts\CascadiaMono.ttf"],
        ),
        ("Consolas", &[r"C:\Windows\Fonts\consola.ttf"]),
        ("Lucida Console", &[r"C:\Windows\Fonts\lucon.ttf"]),
    ];
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let candidates: &[(&str, &[&str])] = &[
        (
            "DejaVu Sans Mono",
            &["/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"],
        ),
        (
            "Liberation Mono",
            &["/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf"],
        ),
        (
            "Ubuntu Mono",
            &["/usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf"],
        ),
    ];

    for (name, paths) in candidates {
        for p in *paths {
            if let Ok(bytes) = std::fs::read(p) {
                let data = B64.encode(&bytes);
                tracing::info!("[font] serving {} ({} bytes) from {}", name, bytes.len(), p);
                return Json(json!({ "name": name, "data": data, "path": p }));
            }
        }
    }

    tracing::warn!("[font] no platform default monospace font found");
    Json(json!({ "name": null, "data": null }))
}

/// POST /api/new-session — spawn a daemon, return camelCase shape the
/// standalone HTML adapter expects: `{sessionName, wsPort, displayName}` or `{error}`.
/// Body is optional JSON `{project_dir}` — Tauri tabs send the tab's project
/// dir; standalone/legacy callers send no body and fall back to the hub's cwd.
pub async fn new_session(body: Option<Json<Value>>) -> Json<Value> {
    let project_dir = body
        .as_ref()
        .and_then(|Json(v)| v.get("project_dir"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".to_string())
        });

    let req = SpawnRequest {
        project_dir,
        display_name: None,
        shell: None,
    };

    let Json(resp) = spawn_session(Json(req)).await;

    if let Some(err) = resp.get("error") {
        return Json(json!({ "error": err }));
    }

    Json(json!({
        "sessionName": resp.get("session_name"),
        "windowId": resp.get("window_id"),
        "wsPort": resp.get("ws_port"),
        "displayName": resp.get("display_name"),
    }))
}
