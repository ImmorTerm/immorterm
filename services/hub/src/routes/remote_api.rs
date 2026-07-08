//! Remote ImmorTerm hosts — listing + registry aggregation + SSH tunneling.
//!
//! The magic-UX backend for the Tauri picker dropdown. Registered remotes
//! live in `~/.immorterm/remotes.json` (written by
//! `immorterm-ai remote add ...`). The UI calls these endpoints to:
//!
//! - `GET /api/v1/remotes` — populate the picker dropdown
//! - `GET /api/v1/remotes/{name}/registry` — show projects + sessions on
//!   that remote, by SSHing into it and reading its registry.json
//! - `POST /api/v1/remotes/{name}/attach` — open an SSH tunnel forwarding
//!   the remote session's WebSocket port to a free local port; the
//!   webview then connects via the existing `ws://127.0.0.1:<port>`
//!   path with no other code changes.
//!
//! See also: `apps/immorterm-ai/immorterm-daemon/src/remote.rs` (CLI side).

use std::collections::{HashMap as StdHashMap, HashSet};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use futures_util::stream::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, OnceCell as TokioOnceCell};
use tracing::{info, warn};

// ── Schema duplicated from immorterm-daemon::remote ─────────────────
// We don't depend on the daemon crate from hub. The schema is tiny and
// stable; if it grows complex enough that drift becomes a risk, factor
// into a shared libs/ crate.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEntry {
    pub name: String,
    pub ssh_target: String,
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
    #[serde(default = "default_immorterm_home")]
    pub immorterm_home: String,
    /// Port the remote's own hub listens on (127.0.0.1, inside the
    /// remote's network namespace). Configurable per remote via
    /// remotes.json; defaults to the hub's standard port.
    #[serde(default = "default_hub_port")]
    pub hub_port: u16,
    #[serde(default)]
    pub created_at: i64,
    /// When true → `StrictHostKeyChecking=yes` for every SSH call on
    /// this remote. Default false (= accept-new TOFU).
    #[serde(default)]
    pub strict_known_hosts: bool,
}

/// Base URL of the remote's hub as seen FROM the remote itself (curl
/// runs over SSH inside the remote's namespace, so this is always
/// loopback — only the port varies).
fn remote_hub_base(entry: &RemoteEntry) -> String {
    format!("http://127.0.0.1:{}", entry.hub_port)
}

/// Pick the SSH StrictHostKeyChecking flag based on the remote's setting.
fn strict_flag(entry: &RemoteEntry) -> &'static str {
    if entry.strict_known_hosts {
        "StrictHostKeyChecking=yes"
    } else {
        "StrictHostKeyChecking=accept-new"
    }
}

fn default_ssh_port() -> u16 { 22 }
fn default_hub_port() -> u16 { crate::config::DEFAULT_HUB_PORT }
fn default_immorterm_home() -> String { "~/.immorterm".to_string() }

#[derive(Debug, Default, Serialize, Deserialize)]
struct RemotesFile {
    #[serde(default)]
    remotes: Vec<RemoteEntry>,
}

fn remotes_path() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    home.join(".immorterm").join("remotes.json")
}

fn load_remotes() -> std::io::Result<RemotesFile> {
    let path = remotes_path();
    if !path.exists() {
        return Ok(RemotesFile::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_json::from_str(&raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn lookup_remote(name: &str) -> Option<RemoteEntry> {
    load_remotes().ok()?.remotes.into_iter().find(|r| r.name == name)
}

/// Atomic save of remotes.json. Mirrors what the daemon CLI does so the
/// two writers stay byte-compatible. tmp + rename keeps a crash from
/// leaving a half-written file.
fn save_remotes(f: &RemotesFile) -> std::io::Result<()> {
    let path = remotes_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(f)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

// ── Routes ──────────────────────────────────────────────────────────

/// `GET /api/v1/remotes` — return the list of registered remotes for the
/// picker dropdown. Excludes `created_at` from the response since the UI
/// doesn't render it.
pub async fn list_remotes() -> (StatusCode, Json<Value>) {
    match load_remotes() {
        Ok(f) => (StatusCode::OK, Json(json!({ "remotes": f.remotes }))),
        Err(e) => {
            warn!("read remotes.json failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AddRemoteReq {
    pub name: String,
    pub ssh_target: String,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    #[serde(default)]
    pub immorterm_home: Option<String>,
    #[serde(default)]
    pub hub_port: Option<u16>,
}

/// `POST /api/v1/remotes` — register a new remote. Mirrors what
/// `immorterm-ai remote add` does on the CLI; the UI calls this from
/// the Remotes Manager modal.
pub async fn add_remote(
    Json(req): Json<AddRemoteReq>,
) -> (StatusCode, Json<Value>) {
    if !valid_name(&req.name) {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "name must be 1–64 alphanumerics, '-' or '_'"
        })));
    }
    if req.ssh_target.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "ssh_target required" })));
    }
    let mut f = match load_remotes() {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))),
    };
    if f.remotes.iter().any(|r| r.name == req.name) {
        return (StatusCode::CONFLICT, Json(json!({
            "error": format!("remote '{}' already exists", req.name)
        })));
    }
    let entry = RemoteEntry {
        name: req.name.clone(),
        ssh_target: req.ssh_target,
        ssh_port: req.ssh_port.unwrap_or(22),
        immorterm_home: req.immorterm_home.unwrap_or_else(|| "~/.immorterm".to_string()),
        hub_port: req.hub_port.unwrap_or_else(default_hub_port),
        created_at: now_unix(),
        strict_known_hosts: false,
    };
    f.remotes.push(entry.clone());
    if let Err(e) = save_remotes(&f) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
    }
    info!("[remotes] added '{}' → {}:{}", entry.name, entry.ssh_target, entry.ssh_port);
    (StatusCode::OK, Json(serde_json::to_value(&entry).unwrap()))
}

#[derive(Debug, Deserialize)]
pub struct EditRemoteReq {
    #[serde(default)]
    pub ssh_target: Option<String>,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    #[serde(default)]
    pub immorterm_home: Option<String>,
    #[serde(default)]
    pub hub_port: Option<u16>,
    #[serde(default)]
    pub strict_known_hosts: Option<bool>,
}

/// `PUT /api/v1/remotes/{name}` — mutate fields on an existing remote.
/// Each Optional field is "leave unchanged" when null. Rename is NOT
/// supported (would invalidate tabs.json + saved sessions); re-add
/// under a new name + remove the old one if rename is needed.
pub async fn edit_remote(
    Path(name): Path<String>,
    Json(req): Json<EditRemoteReq>,
) -> (StatusCode, Json<Value>) {
    let mut f = match load_remotes() {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))),
    };
    let Some(entry) = f.remotes.iter_mut().find(|r| r.name == name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };
    if let Some(v) = req.ssh_target { entry.ssh_target = v; }
    if let Some(v) = req.ssh_port { entry.ssh_port = v; }
    if let Some(v) = req.immorterm_home { entry.immorterm_home = v; }
    if let Some(v) = req.hub_port { entry.hub_port = v; }
    if let Some(v) = req.strict_known_hosts { entry.strict_known_hosts = v; }
    let updated = entry.clone();
    if let Err(e) = save_remotes(&f) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
    }
    info!("[remotes] edited '{}'", updated.name);
    (StatusCode::OK, Json(serde_json::to_value(&updated).unwrap()))
}

/// `DELETE /api/v1/remotes/{name}` — remove a remote. Doesn't kill any
/// active tunnels using it; those die naturally on next use.
pub async fn remove_remote(
    Path(name): Path<String>,
) -> (StatusCode, Json<Value>) {
    let mut f = match load_remotes() {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))),
    };
    let before = f.remotes.len();
    f.remotes.retain(|r| r.name != name);
    if f.remotes.len() == before {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    }
    if let Err(e) = save_remotes(&f) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
    }
    info!("[remotes] removed '{name}'");
    (StatusCode::OK, Json(json!({ "ok": true })))
}

/// `GET /api/v1/remotes/{name}/config?project_dir=...` — fetch the
/// REMOTE hub's /api/v1/config response by SSH-cating curl through the
/// tunnel. The remote's local hub runs on 127.0.0.1:1440 inside its own
/// process namespace, so we exec `curl -s` over SSH against that. Lets
/// the local Tauri webview render with the REMOTE's themes, menu items,
/// preferences, and character defs — matching what the user has set on
/// the box they're SSH'd into.
///
/// Gracefully returns the local hub's defaults on transport failure so
/// the webview always boots even when the remote is half-broken.
pub async fn get_remote_config(
    Path(name): Path<String>,
    Query(q): Query<RegistryQuery>,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };
    let qs = match q.project_dir.as_deref() {
        Some(d) if !d.is_empty() => format!("?project_dir={}", urlencoding(d)),
        _ => String::new(),
    };
    let entry_clone = entry.clone();
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_clone.ssh_port.to_string(),
                &entry_clone.ssh_target,
                // The remote hub binds loopback inside its container/host.
                // -s = silent, --fail = exit non-zero on HTTP error.
                // URL shell-quoted: `qs` carries caller-controlled params.
                &format!(
                    "curl -sf --max-time 5 {}",
                    shell_quote(&format!("{}/api/v1/config{qs}", remote_hub_base(&entry_clone))),
                ),
            ])
            .output()
    })
    .await;
    let out = match probe {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            warn!("[remote-config] ssh spawn failed for '{name}': {e}");
            return (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("ssh: {e}") })));
        }
        Err(e) => {
            warn!("[remote-config] join error for '{name}': {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("join: {e}") })));
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        warn!("[remote-config] curl exit={:?}: {stderr}", out.status.code());
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": "remote hub unreachable",
                "stderr": stderr,
                "exit": out.status.code(),
            })),
        );
    }
    match serde_json::from_slice::<Value>(&out.stdout) {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("remote config was not valid JSON: {e}") })),
        ),
    }
}

fn urlencoding(s: &str) -> String {
    // Crude percent-encode — only escapes the chars libcurl objects to
    // in a URL query value. Fine for project_dir paths.
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// `PUT /api/v1/remotes/{name}/config/project` — write per-project config
/// on the REMOTE. SSH-execs `curl -X PUT` against the remote's hub on
/// 127.0.0.1:1440. Body is forwarded as-is. The remote hub's
/// inotifywait on the project's .immorterm/config.json will then fire
/// → our hub's watcher broadcasts config_changed → our webview
/// re-fetches + applies theme. Round-trip cost ~1 SSH RTT.
pub async fn put_remote_project_config(
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    proxy_remote_put(&name, "/api/v1/config/project", body).await
}

/// `PUT /api/v1/remotes/{name}/config/preferences` — same as above but
/// for global preferences (`~/.immorterm/config.json` → `preferences`).
pub async fn put_remote_preferences(
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    proxy_remote_put(&name, "/api/v1/config/preferences", body).await
}

async fn proxy_remote_put(
    name: &str,
    remote_path: &str,
    body: Value,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };
    let body_str = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    // Shell-escape the JSON body with single quotes; embedded ' becomes '\''.
    let body_esc = body_str.replace('\'', "'\\''");
    let entry_clone = entry.clone();
    // URL shell-quoted: `remote_path` carries caller-controlled params.
    let url = shell_quote(&format!("{}{remote_path}", remote_hub_base(&entry)));
    let cmd = format!(
        "curl -sS -f --max-time 5 -X PUT -H 'content-type: application/json' \
         -d '{body_esc}' {url}",
    );
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_clone.ssh_port.to_string(),
                &entry_clone.ssh_target,
                &cmd,
            ])
            .output()
    })
    .await;
    let out = match probe {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("ssh: {e}") }))),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": format!("join: {e}") }))),
    };
    if !out.status.success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": "remote hub PUT failed",
                "stderr": String::from_utf8_lossy(&out.stderr),
                "exit": out.status.code(),
            })),
        );
    }
    let body = String::from_utf8_lossy(&out.stdout);
    match serde_json::from_str::<Value>(&body) {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err(_) => (StatusCode::OK, Json(json!({ "success": true, "raw": body }))),
    }
}

/// `GET /api/v1/remotes/{name}/claude-cache/image/{session}/{n}` — fetch
/// a cached Claude paste-image (`~/.claude/image-cache/<session>/<n>.png`)
/// from the REMOTE filesystem via SSH+cat. Used by the Cmd+hover
/// preview on `[Image #N]` links inside a remote-bound tab.
///
/// HEAD requests answer existence via `ssh test -f` (cheap); GET streams
/// the PNG bytes back with `image/png` content-type.
///
/// **`session=auto` fallback**: when the webview doesn't know the Claude
/// session UUID (e.g. on a remote tab where the daemon's jsonl-tail
/// tracker couldn't find a matching `~/.claude/projects/*/<uuid>.jsonl`),
/// pass `session=auto` and the server scans `~/.claude/image-cache/*/<n>.png`
/// for any match. Picks the most-recently-modified one if multiple UUIDs
/// have a file with that index. Lets `[Image #N]` previews work even
/// when UUID resolution is broken upstream.
pub async fn get_remote_claude_image(
    Path((name, session, n)): Path<(String, String, String)>,
    method: axum::http::Method,
) -> axum::response::Response {
    use axum::body::Body;
    use axum::http::{header, StatusCode as Sc};
    use axum::response::Response;

    let Some(entry) = lookup_remote(&name) else {
        return Response::builder()
            .status(Sc::NOT_FOUND)
            .body(Body::from(format!(r#"{{"error":"no remote named '{name}'"}}"#)))
            .unwrap();
    };
    // Build the remote path. Mirror the local hub's layout — files at
    // ~/.claude/image-cache/<uuid>/<n>.png. When session=='auto', scan
    // the cache directory and resolve the most-recently-modified PNG
    // matching the requested index — see endpoint doc above.
    let entry_clone = entry.clone();
    let is_head = method == axum::http::Method::HEAD;
    let cmd_str = if session == "auto" {
        // `ls -t` orders newest first; pick the first match. base64 -w0
        // encodes the file inline (or test -f for HEAD).
        if is_head {
            format!(
                "P=$(ls -t ~/.claude/image-cache/*/{n}.png 2>/dev/null | head -n1); [ -n \"$P\" ] && echo ok"
            )
        } else {
            format!(
                "P=$(ls -t ~/.claude/image-cache/*/{n}.png 2>/dev/null | head -n1); [ -n \"$P\" ] && base64 -w0 \"$P\" 2>/dev/null"
            )
        }
    } else {
        let remote_path = format!("~/.claude/image-cache/{session}/{n}.png");
        if is_head {
            format!("test -f {remote_path} && echo ok")
        } else {
            format!("base64 -w0 {remote_path} 2>/dev/null")
        }
    };
    let entry_for_run = entry_clone.clone();
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_for_run.ssh_port.to_string(),
                &entry_for_run.ssh_target,
                &cmd_str,
            ])
            .output()
    })
    .await;
    let out = match probe {
        Ok(Ok(o)) => o,
        _ => return Response::builder()
            .status(Sc::BAD_GATEWAY)
            .body(Body::empty())
            .unwrap(),
    };
    if !out.status.success() {
        return Response::builder().status(Sc::NOT_FOUND).body(Body::empty()).unwrap();
    }
    if is_head {
        let body = String::from_utf8_lossy(&out.stdout);
        if body.trim() == "ok" {
            return Response::builder().status(Sc::OK).body(Body::empty()).unwrap();
        }
        return Response::builder().status(Sc::NOT_FOUND).body(Body::empty()).unwrap();
    }
    // GET — decode base64 + return as PNG.
    use base64::Engine;
    let png = match base64::engine::general_purpose::STANDARD.decode(out.stdout.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect::<Vec<_>>()) {
        Ok(b) => b,
        Err(_) => return Response::builder().status(Sc::BAD_GATEWAY).body(Body::empty()).unwrap(),
    };
    Response::builder()
        .status(Sc::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .body(Body::from(png))
        .unwrap()
}

/// `GET /api/v1/remotes/{name}/link-exists?path=<abs>` — remote-aware
/// twin of the local hub's `/api/link-exists`. SSH-stats the file and
/// returns the SAME shape the VS Code extension's `check-link-exists`
/// produces: `{ exists, kind, ext, imageDataUrl, preview,
/// previewStartLine }`. Used by the in-terminal file-link hover
/// preview when the tab is remote — the webview's standalone adapter
/// routes here instead of `/api/link-exists` when `_remoteName` is set.
///
/// Implementation strategy: a single SSH invocation runs a small shell
/// program that emits framed sections (`EXISTS=...`, `IMAGE_B64_START`
/// ... `IMAGE_B64_END`, `TEXT_START` ... `TEXT_END`). The hub parses
/// the frames into the JSON shape. One SSH per hover keeps latency in
/// check; the daemon-side cost is a `stat` + at most one `cat`.
///
/// Image dataURL is built as `data:image/<ext>;base64,<bytes>` so the
/// existing `<img class="link-tooltip-image" src="...">` rendering in
/// the webview tooltip works unchanged. Text preview is utf-8 decoded,
/// capped at 512KB (matches the local pipeline's threshold).
pub async fn get_remote_link_exists(
    Path(name): Path<String>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("no remote '{name}'")})),
        );
    };
    let Some(raw_path) = query.get("path").cloned() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing path"})),
        );
    };
    // Resolve relative paths against the session's cwd on the REMOTE.
    // Matches the local `/api/link-exists` and VS Code extension's
    // resolveLinkPathAsync — bare filenames like `index.html` from
    // Claude's output need to resolve to `<cwd>/index.html`.
    let cwd = query.get("cwd").cloned();
    let path = if raw_path.starts_with('/') {
        raw_path.clone()
    } else if let Some(c) = cwd.as_deref() {
        let mut joined = String::from(c.trim_end_matches('/'));
        joined.push('/');
        joined.push_str(&raw_path);
        joined
    } else {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "relative path requires cwd"})),
        );
    };
    if path.contains("..") || !path.starts_with('/') {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "path must resolve to absolute, no .."})),
        );
    }
    // Shell-quote so spaces / specials in path don't break the script.
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    // The script emits a stable framed format. Keeping it inline keeps
    // the deployment simple — no extra file to ship to the remote.
    let script = format!(
        r#"P={quoted}
if [ ! -f "$P" ] && [ ! -d "$P" ]; then echo "EXISTS=0"; exit; fi
if [ -d "$P" ]; then echo "EXISTS=1"; echo "KIND=dir"; exit; fi
echo "EXISTS=1"
echo "KIND=file"
SZ=$(wc -c < "$P" | tr -d ' ')
echo "SIZE=$SZ"
EXT=$(printf '%s' "$P" | awk -F. '{{print tolower($NF)}}')
echo "EXT=$EXT"
case "$EXT" in
  png|jpg|jpeg|gif|webp|svg|ico|bmp)
    if [ "$SZ" -le 5242880 ]; then
      echo "IMAGE_B64_START"
      base64 -w0 "$P" 2>/dev/null
      echo
      echo "IMAGE_B64_END"
    fi ;;
  *)
    if [ "$SZ" -le 524288 ]; then
      echo "TEXT_START"
      head -c 524288 "$P"
      echo
      echo "TEXT_END"
    fi ;;
esac
"#
    );

    let entry_for_run = entry.clone();
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_for_run.ssh_port.to_string(),
                &entry_for_run.ssh_target,
                &script,
            ])
            .output()
    })
    .await;
    let out = match probe {
        Ok(Ok(o)) if o.status.success() => o,
        _ => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"exists": false, "error": "ssh failed"})),
            );
        }
    };

    let mut exists = false;
    let mut kind: Option<String> = None;
    let mut ext: Option<String> = None;
    let mut image_b64: Option<String> = None;
    let mut text: Option<String> = None;
    let stdout = out.stdout;

    // Walk lines. For framed sections (IMAGE_B64_*/TEXT_*) we collect
    // the bytes between START/END markers without splitting on newlines
    // in the payload itself. Text frames are utf-8 lossy-decoded.
    let mut i = 0usize;
    while i < stdout.len() {
        let line_end = stdout[i..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|p| i + p)
            .unwrap_or(stdout.len());
        let line = &stdout[i..line_end];
        i = line_end + 1;
        let line_str = String::from_utf8_lossy(line);
        if let Some(rest) = line_str.strip_prefix("EXISTS=") {
            exists = rest.trim() == "1";
        } else if let Some(rest) = line_str.strip_prefix("KIND=") {
            kind = Some(rest.trim().to_string());
        } else if let Some(rest) = line_str.strip_prefix("EXT=") {
            let v = rest.trim().to_string();
            if !v.is_empty() {
                ext = Some(v);
            }
        } else if line_str.trim() == "IMAGE_B64_START" {
            let end_marker = b"\nIMAGE_B64_END";
            if let Some(rel_end) = stdout[i..]
                .windows(end_marker.len())
                .position(|w| w == end_marker)
            {
                let payload = &stdout[i..i + rel_end];
                let b64: String = payload
                    .iter()
                    .filter(|b| !b.is_ascii_whitespace())
                    .map(|b| *b as char)
                    .collect();
                image_b64 = Some(b64);
                i += rel_end + end_marker.len() + 1;
            }
        } else if line_str.trim() == "TEXT_START" {
            let end_marker = b"\nTEXT_END";
            if let Some(rel_end) = stdout[i..]
                .windows(end_marker.len())
                .position(|w| w == end_marker)
            {
                let payload = &stdout[i..i + rel_end];
                // Skip if binary (NUL in first 4KB) — matches the VS Code
                // extension's check.
                let head = &payload[..payload.len().min(4096)];
                if !head.contains(&0u8) {
                    text = Some(String::from_utf8_lossy(payload).to_string());
                }
                i += rel_end + end_marker.len() + 1;
            }
        }
    }

    let mut resp = json!({
        "exists": exists,
        "kind": kind.unwrap_or_else(|| "file".to_string()),
        "ext": ext,
    });
    if let Some(b64) = image_b64 {
        let mime = match resp["ext"].as_str() {
            Some("svg") => "image/svg+xml".to_string(),
            Some("jpg") => "image/jpeg".to_string(),
            Some(e) => format!("image/{e}"),
            None => "image/png".to_string(),
        };
        resp["imageDataUrl"] = json!(format!("data:{mime};base64,{b64}"));
    }
    if let Some(t) = text {
        resp["preview"] = json!(t);
        resp["previewStartLine"] = json!(1);
    }
    (StatusCode::OK, Json(resp))
}

/// `GET /api/v1/remotes/{name}/ls?path=<absolute-dir>` — remote dir
/// listing for the cmd-hover Reveal tree. Mirrors local `/api/ls`
/// shape (`{path, entries: [{name, kind, size, mtime}]}`) so the
/// webview's tree code stays unified across local + remote.
///
/// SSH-side: `find -mindepth 1 -maxdepth 1 -printf '%y\t%s\t%T@\t%f\n'`
/// emits one row per entry — typed (`f`/`d`/`l`), size in bytes, mtime
/// as epoch seconds, name. Single tab-delimited columns avoid the
/// shell-quoting pitfalls of `ls -l`'s human-formatted output.
pub async fn get_remote_ls(
    Path(name): Path<String>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("no remote '{name}'")})),
        );
    };
    let Some(path) = query.get("path").cloned() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing path"})),
        );
    };
    if path.contains("..") || !path.starts_with('/') {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "path must be absolute, no .."})),
        );
    }
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    // Mac+busybox `find` rejects `-printf`; the GNU-only flag is fine
    // here because the only remotes we support are Linux containers /
    // Linux VPSes. Cap with `-maxdepth 1` so we never recurse — the
    // tree expands lazily one dir at a time.
    let script = format!(
        r#"find {quoted} -mindepth 1 -maxdepth 1 -printf '%y\t%s\t%T@\t%f\n' 2>/dev/null"#
    );
    let entry_for_run = entry.clone();
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_for_run.ssh_port.to_string(),
                &entry_for_run.ssh_target,
                &script,
            ])
            .output()
    })
    .await;
    let out = match probe {
        Ok(Ok(o)) if o.status.success() => o,
        Ok(Ok(o)) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": String::from_utf8_lossy(&o.stderr).to_string(),
                    "entries": []
                })),
            );
        }
        _ => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "ssh failed", "entries": []})),
            );
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut entries: Vec<Value> = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() != 4 {
            continue;
        }
        let kind = match parts[0] {
            "d" => "dir",
            "l" => "link",
            _ => "file",
        };
        let size: u64 = parts[1].parse().unwrap_or(0);
        // `%T@` is float seconds since epoch. Truncate to whole seconds.
        let mtime: i64 = parts[2]
            .split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let name = parts[3].to_string();
        entries.push(json!({
            "name": name,
            "kind": kind,
            "size": size,
            "mtime": mtime,
        }));
    }
    entries.sort_by(|a, b| {
        let ad = a["kind"].as_str() == Some("dir");
        let bd = b["kind"].as_str() == Some("dir");
        match (ad, bd) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a["name"].as_str().cmp(&b["name"].as_str()),
        }
    });
    (StatusCode::OK, Json(json!({ "path": path, "entries": entries })))
}

/// `GET /api/v1/remotes/{name}/paste-image?path=<abs>` — fetch a
/// daemon-saved paste image (uploaded via Cmd+V / Cmd+Option+V / Cmd+E
/// modal flush) from the REMOTE filesystem via SSH+cat. Used by the
/// Cmd+hover image-preview on path links inside a remote-bound tab.
///
/// **Path allowlist**: the SSH side only accepts paths under
/// `~/.immorterm/paste/` (the daemon's per-session paste dir) — this
/// endpoint must not become a generic remote-file reader. Caller should
/// pass an absolute path that begins with that prefix; we still re-
/// validate server-side via a shell pattern check before `cat`-ing.
///
/// HEAD answers existence cheaply; GET streams the PNG bytes back.
pub async fn get_remote_paste_image(
    Path(name): Path<String>,
    method: axum::http::Method,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::body::Body;
    use axum::http::{header, StatusCode as Sc};
    use axum::response::Response;

    let Some(entry) = lookup_remote(&name) else {
        return Response::builder()
            .status(Sc::NOT_FOUND)
            .body(Body::from(format!(r#"{{"error":"no remote named '{name}'"}}"#)))
            .unwrap();
    };
    let Some(path) = query.get("path").cloned() else {
        return Response::builder()
            .status(Sc::BAD_REQUEST)
            .body(Body::from(r#"{"error":"missing path query param"}"#))
            .unwrap();
    };
    // Defense in depth: shell-side wildcard guards against path traversal.
    // Accept absolute paths matching `*/.immorterm/paste/*.png` only.
    // (Cheap check, real safety comes from the remote-side path test.)
    if path.contains("..") || !path.contains("/.immorterm/paste/") || !path.ends_with(".png") {
        return Response::builder()
            .status(Sc::FORBIDDEN)
            .body(Body::from(r#"{"error":"path must be under ~/.immorterm/paste/ and end .png"}"#))
            .unwrap();
    }
    let is_head = method == axum::http::Method::HEAD;

    // Single-quote the path so shell expansion doesn't bite. The path
    // can contain spaces in window/session names.
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    let cmd_str = if is_head {
        format!("test -f {quoted} && echo ok")
    } else {
        format!("base64 -w0 {quoted} 2>/dev/null")
    };
    let entry_for_run = entry.clone();
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_for_run.ssh_port.to_string(),
                &entry_for_run.ssh_target,
                &cmd_str,
            ])
            .output()
    })
    .await;
    let out = match probe {
        Ok(Ok(o)) => o,
        _ => return Response::builder()
            .status(Sc::BAD_GATEWAY)
            .body(Body::empty())
            .unwrap(),
    };
    if !out.status.success() {
        return Response::builder().status(Sc::NOT_FOUND).body(Body::empty()).unwrap();
    }
    if is_head {
        let body = String::from_utf8_lossy(&out.stdout);
        if body.trim() == "ok" {
            return Response::builder().status(Sc::OK).body(Body::empty()).unwrap();
        }
        return Response::builder().status(Sc::NOT_FOUND).body(Body::empty()).unwrap();
    }
    use base64::Engine;
    let png = match base64::engine::general_purpose::STANDARD.decode(
        out.stdout.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect::<Vec<_>>(),
    ) {
        Ok(b) => b,
        Err(_) => return Response::builder().status(Sc::BAD_GATEWAY).body(Body::empty()).unwrap(),
    };
    Response::builder()
        .status(Sc::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::CACHE_CONTROL, "private, max-age=60")
        .body(Body::from(png))
        .unwrap()
}

/// Generic proxy: forward an arbitrary hub method to the remote's
/// `/api/v1/tasks*` endpoints via SSH+curl. Used by the Cmd+hover task
/// list (and the Task modal) when in a remote-bound tab.
///
/// Path is everything after `/api/v1/remotes/{name}/tasks` — empty
/// string for the listing endpoint, or `/<task_id>` / `/reorder` etc.
/// Query string is forwarded verbatim. Body is forwarded for POST/PUT.
pub async fn proxy_remote_tasks_root(
    Path(name): Path<String>,
    method: axum::http::Method,
    query: axum::extract::RawQuery,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    proxy_remote_tasks_inner(name, String::new(), method, query, body).await
}

pub async fn proxy_remote_tasks(
    Path((name, rest)): Path<(String, String)>,
    method: axum::http::Method,
    query: axum::extract::RawQuery,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    proxy_remote_tasks_inner(name, rest, method, query, body).await
}

/// Generic SSH-proxy for `/api/v1/registry/*` writes. Local hub has a
/// dozen registry actions (shelve, close, reattach, rename, reorder,
/// title-lock, speak-mode, …) — instead of mirroring each as its own
/// remote endpoint, proxy the whole subtree through one route.
///
/// Path is the action after `/registry/` (e.g. `shelve`, `close`,
/// `reattach`). Body is forwarded verbatim. Without this, in-tab
/// session-management actions on a remote tab silently no-op against
/// the LOCAL registry — sessions reappear on reload (the user's bug
/// report this fix was driven by).
pub async fn proxy_remote_registry(
    Path((name, rest)): Path<(String, String)>,
    method: axum::http::Method,
    query: axum::extract::RawQuery,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    proxy_remote_registry_inner(name, rest, method, query, body).await
}

/// Single-quote a string for safe interpolation into a remote shell
/// command (`'…'` with embedded single quotes as `'\''`). Inside single
/// quotes POSIX shells treat every byte literally, so this neutralizes
/// command injection via query strings / path segments forwarded over
/// SSH (the remote side runs the command through the login shell).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

async fn proxy_remote_registry_inner(
    name: String,
    rest: String,
    method: axum::http::Method,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };
    let qs = query.map(|s| format!("?{s}")).unwrap_or_default();
    let rest_path = if rest.is_empty() { String::new() } else { format!("/{rest}") };
    let method_str = method.as_str().to_string();
    let body_str = String::from_utf8_lossy(&body).to_string();
    let body_esc = body_str.replace('\'', "'\\''");

    // URL is shell-quoted: `rest_path` and `qs` are caller-controlled and
    // would otherwise be interpreted by the remote shell.
    let url = shell_quote(&format!("{}/api/v1/registry{rest_path}{qs}", remote_hub_base(&entry)));
    let curl_cmd = if matches!(method_str.as_str(), "POST" | "PUT" | "DELETE") {
        format!(
            "curl -sS -f --max-time 8 -X {method_str} -H 'content-type: application/json' \
             -d '{body_esc}' {url}",
        )
    } else {
        format!("curl -sS -f --max-time 8 {url}")
    };
    let entry_clone = entry.clone();
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_clone.ssh_port.to_string(),
                &entry_clone.ssh_target,
                &curl_cmd,
            ])
            .output()
    })
    .await;
    let out = match probe {
        Ok(Ok(o)) => o,
        _ => return (StatusCode::BAD_GATEWAY, Json(json!({ "error": "ssh proxy failed" }))),
    };
    if !out.status.success() {
        return (StatusCode::BAD_GATEWAY, Json(json!({
            "error": "remote registry endpoint failed",
            "stderr": String::from_utf8_lossy(&out.stderr),
        })));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "raw": raw }));
    (StatusCode::OK, Json(v))
}

async fn proxy_remote_tasks_inner(
    name: String,
    rest: String,
    method: axum::http::Method,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };
    let qs = query.map(|s| format!("?{s}")).unwrap_or_default();
    let rest_path = if rest.is_empty() { String::new() } else { format!("/{rest}") };
    let method_str = method.as_str().to_string();
    let body_str = String::from_utf8_lossy(&body).to_string();
    let body_esc = body_str.replace('\'', "'\\''");

    // URL is shell-quoted — see proxy_remote_registry_inner.
    let url = shell_quote(&format!("{}/api/v1/tasks{rest_path}{qs}", remote_hub_base(&entry)));
    let curl_cmd = if matches!(method_str.as_str(), "POST" | "PUT" | "DELETE") {
        format!(
            "curl -sS -f --max-time 8 -X {method_str} -H 'content-type: application/json' \
             -d '{body_esc}' {url}",
        )
    } else {
        format!("curl -sS -f --max-time 8 {url}")
    };
    let entry_clone = entry.clone();
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_clone.ssh_port.to_string(),
                &entry_clone.ssh_target,
                &curl_cmd,
            ])
            .output()
    })
    .await;
    let out = match probe {
        Ok(Ok(o)) => o,
        _ => return (StatusCode::BAD_GATEWAY, Json(json!({ "error": "ssh proxy failed" }))),
    };
    if !out.status.success() {
        return (StatusCode::BAD_GATEWAY, Json(json!({
            "error": "remote tasks endpoint failed",
            "stderr": String::from_utf8_lossy(&out.stderr),
        })));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "raw": raw }));
    (StatusCode::OK, Json(v))
}

/// `GET /api/v1/remotes/{name}/files/index` — SSH-proxy the file-browser
/// index endpoint to the remote hub. Forwards the original query string
/// (`root`, `limit`) verbatim to `/api/v1/files/index` on the remote's
/// local hub at 127.0.0.1:1440.
pub async fn get_remote_files_index(
    Path(name): Path<String>,
    query: axum::extract::RawQuery,
) -> (StatusCode, Json<Value>) {
    proxy_remote_get(name, "/api/v1/files/index", query).await
}

/// `GET /api/v1/remotes/{name}/files/grep` — SSH-proxy the file-browser
/// content-search endpoint to the remote hub. Forwards the original query
/// string (`root`, `q`, `limit`) verbatim to `/api/v1/files/grep`.
pub async fn get_remote_files_grep(
    Path(name): Path<String>,
    query: axum::extract::RawQuery,
) -> (StatusCode, Json<Value>) {
    proxy_remote_get(name, "/api/v1/files/grep", query).await
}

/// `GET /api/v1/remotes/{name}/files/status` — SSH-proxy the file-browser
/// git-status endpoint to the remote hub (dirty indicators for a remote
/// project tab). Forwards `root` verbatim to `/api/v1/files/status`.
pub async fn get_remote_files_status(
    Path(name): Path<String>,
    query: axum::extract::RawQuery,
) -> (StatusCode, Json<Value>) {
    proxy_remote_get(name, "/api/v1/files/status", query).await
}

/// Shared GET proxy: SSH into the remote and `curl` a local hub path,
/// forwarding the caller's query string verbatim. Mirrors the registry /
/// tasks proxy plumbing (same SSH args, same 127.0.0.1:1440 target, same
/// JSON-or-raw response handling) but for read-only GETs with no body.
async fn proxy_remote_get(
    name: String,
    path: &str,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };
    let qs = query.map(|s| format!("?{s}")).unwrap_or_default();
    // URL is shell-quoted: the query string is caller-controlled (e.g. the
    // grep `q=` parameter) and the remote runs this through a login shell.
    let curl_cmd = format!(
        "curl -sS -f --max-time 8 {}",
        shell_quote(&format!("{}{path}{qs}", remote_hub_base(&entry))),
    );
    let entry_clone = entry.clone();
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_clone.ssh_port.to_string(),
                &entry_clone.ssh_target,
                &curl_cmd,
            ])
            .output()
    })
    .await;
    let out = match probe {
        Ok(Ok(o)) => o,
        _ => return (StatusCode::BAD_GATEWAY, Json(json!({ "error": "ssh proxy failed" }))),
    };
    if !out.status.success() {
        return (StatusCode::BAD_GATEWAY, Json(json!({
            "error": "remote files endpoint failed",
            "stderr": String::from_utf8_lossy(&out.stderr),
        })));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "raw": raw }));
    (StatusCode::OK, Json(v))
}

/// `GET /api/v1/ssh-config-hosts` — parse `~/.ssh/config` and return
/// the list of `Host` aliases the user already has set up. Lets the
/// Remotes Manager modal suggest them in the "+ Add Remote" form so
/// users with corporate ProxyJump / cert / custom IdentityFile setups
/// don't re-type all that — they just pick the existing alias.
///
/// Skips wildcard hosts (`*`, `*.example.com`) because they're config
/// fragments, not directly connectable. Returns an empty list if
/// ~/.ssh/config doesn't exist (fresh systems).
pub async fn list_ssh_config_hosts() -> (StatusCode, Json<Value>) {
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => return (StatusCode::OK, Json(json!({ "hosts": [] }))),
    };
    let path = home.join(".ssh").join("config");
    if !path.exists() {
        return (StatusCode::OK, Json(json!({ "hosts": [] })));
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::OK, Json(json!({ "hosts": [] }))),
    };
    // Crude parse — good enough for the autocomplete UX. Each `Host`
    // line can declare multiple aliases space-separated. Indented lines
    // belong to the previous block. We ignore the body (HostName, Port,
    // User, ProxyJump, etc.) because the modal flow just needs the
    // alias string; ssh resolves the rest when called.
    let mut hosts: Vec<String> = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("host ") {
            // Aliases are on the original-case line after "Host "; reparse.
            let original_rest = trimmed.split_once(char::is_whitespace).map(|x| x.1)
                .unwrap_or("")
                .trim();
            for alias in original_rest.split_ascii_whitespace() {
                if !alias.contains('*') && !alias.contains('?') && !alias.is_empty() {
                    hosts.push(alias.to_string());
                }
            }
            let _ = rest;
        }
    }
    hosts.sort();
    hosts.dedup();
    (StatusCode::OK, Json(json!({ "hosts": hosts })))
}

/// `POST /api/v1/remotes/{name}/test` — non-interactive SSH probe.
/// Returns `{ok, latency_ms, session_count, error?}`. Used by the
/// Remotes Manager modal to render liveness + a session-count badge
/// per row.
pub async fn test_remote(
    Path(name): Path<String>,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };
    let started = std::time::Instant::now();
    let entry_for_probe = entry.clone();
    let probe = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_for_probe.ssh_port.to_string(),
                &entry_for_probe.ssh_target,
                &format!("cat {}/registry.json 2>/dev/null || echo NO_REGISTRY", entry_for_probe.immorterm_home),
            ])
            .output()
    })
    .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let out = match probe {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return (StatusCode::OK, Json(json!({
                "ok": false,
                "latency_ms": latency_ms,
                "error": format!("ssh: {e}"),
            })));
        }
        Err(e) => {
            return (StatusCode::OK, Json(json!({
                "ok": false,
                "latency_ms": latency_ms,
                "error": format!("join: {e}"),
            })));
        }
    };
    if !out.status.success() {
        return (StatusCode::OK, Json(json!({
            "ok": false,
            "latency_ms": latency_ms,
            "error": String::from_utf8_lossy(&out.stderr).to_string(),
        })));
    }
    // Count sessions + project_dirs from the registry response.
    let mut session_count = 0usize;
    let mut project_dirs: HashSet<String> = HashSet::new();
    if let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) {
        if let Some(arr) = v.get("sessions").and_then(|s| s.as_array()) {
            session_count = arr.len();
            for s in arr {
                let pd = s.get("project_dir").and_then(|v| v.as_str()).unwrap_or("").to_string();
                project_dirs.insert(pd);
            }
        }
    }
    (StatusCode::OK, Json(json!({
        "ok": true,
        "latency_ms": latency_ms,
        "session_count": session_count,
        "project_count": project_dirs.len(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct RegistryQuery {
    /// Optional project_dir filter — mirrors the local /api/v1/registry?project_dir=
    /// behaviour so the UI can use one code path.
    pub project_dir: Option<String>,
}

/// `GET /api/v1/remotes/{name}/registry` — SSH to the remote, `cat` its
/// registry.json, return as JSON. Sessions are tagged with the
/// `remote: "<name>"` field so the UI can render them with the right
/// origin badge.
///
/// Falls back to an empty registry on transport failure (after logging)
/// — keeps the picker responsive even when a remote is offline.
pub async fn get_remote_registry(
    Path(name): Path<String>,
    Query(q): Query<RegistryQuery>,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };

    // SSH + cat. BatchMode=yes keeps auth non-interactive — relies on
    // ssh-agent or pre-loaded keys. ConnectTimeout caps the picker
    // hang to 5s when the remote is down.
    let remote_registry_path = format!("{}/registry.json", entry.immorterm_home);
    let out = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry.ssh_port.to_string(),
                &entry.ssh_target,
                &format!("cat {remote_registry_path}"),
            ])
            .output()
    })
    .await;

    let out = match out {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            warn!("ssh spawn failed for '{name}': {e}");
            return (StatusCode::BAD_GATEWAY, Json(json!({ "error": e.to_string() })));
        }
        Err(e) => {
            warn!("join error fetching remote '{name}': {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        warn!("ssh remote '{name}' failed: {stderr}");
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": "SSH command failed",
                "stderr": stderr,
                "exit": out.status.code(),
            })),
        );
    }

    let body = String::from_utf8_lossy(&out.stdout);
    let mut value: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("remote '{name}' registry.json was not valid JSON: {e}");
            return (StatusCode::BAD_GATEWAY, Json(json!({ "error": e.to_string() })));
        }
    };

    // Tag every session with its origin so the UI can badge it, AND inject
    // `alive: true` because the webview's session-restore filter requires
    // it (mirrors what /api/v1/registry's enrich step does via local
    // `is_process_alive` — we can't check remote PIDs from here without an
    // extra SSH RTT per session). Trusting the registry is fine for the
    // MVP: the daemon self-prunes dead entries.
    if let Some(sessions) = value.get_mut("sessions").and_then(|s| s.as_array_mut()) {
        for s in sessions.iter_mut() {
            if let Some(obj) = s.as_object_mut() {
                obj.insert("remote".to_string(), json!(name));
                obj.entry("alive").or_insert(json!(true));
            }
        }
        // Optional project_dir filter, mirrors local behaviour.
        if let Some(filter) = q.project_dir.as_deref() {
            sessions.retain(|s| {
                s.get("project_dir")
                    .and_then(|v| v.as_str())
                    .map(|d| d == filter)
                    .unwrap_or(false)
            });
        }
    }

    info!("served remote registry for '{name}'");
    (StatusCode::OK, Json(value))
}

// ── Tunnel manager ───────────────────────────────────────────────────
//
// Reuses one `ssh -L <local>:127.0.0.1:<remote> <target>` per
// (remote, ws_port) pair so reconnects don't spawn new tunnels.

use std::collections::HashMap;
use std::sync::OnceLock;

struct TunnelHandle {
    local_port: u16,
    child: std::process::Child,
}

fn tunnels() -> &'static Mutex<HashMap<(String, u16), TunnelHandle>> {
    static T: OnceLock<Mutex<HashMap<(String, u16), TunnelHandle>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

fn pick_free_port() -> std::io::Result<u16> {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

#[derive(Debug, Deserialize)]
pub struct AttachRequest {
    pub ws_port: u16,
}

#[derive(Debug, Serialize)]
pub struct AttachResponse {
    pub local_port: u16,
    pub remote: String,
    pub remote_ws_port: u16,
}

/// `POST /api/v1/remotes/{name}/attach` — open (or reuse) an SSH tunnel
/// forwarding the remote's session WebSocket to a free local port.
/// Returns `{local_port}` which the UI uses to construct
/// `ws://127.0.0.1:<local_port>` — same shape as a local session, so
/// the existing webview WS code path works unchanged.
/// Poll until the SSH tunnel's local port accepts a TCP connection, the ssh
/// child exits, or we hit `max`. SSH (with `ExitOnForwardFailure=yes`) only
/// binds the local listener after auth + forward setup succeed, so a
/// successful connect is a reliable "tunnel is live" signal. If ssh exits
/// first (auth/forward failure) we drain its stderr and return the real
/// reason, so the picker can show "Permission denied (publickey)" rather
/// than silently handing back a dead port.
fn wait_for_tunnel(
    child: &mut std::process::Child,
    local_port: u16,
    max: std::time::Duration,
) -> Result<(), String> {
    use std::io::Read;
    use std::net::{SocketAddr, TcpStream};
    use std::time::{Duration, Instant};

    let addr: SocketAddr = ([127, 0, 0, 1], local_port).into();
    let deadline = Instant::now() + max;
    loop {
        // ssh died before binding (auth or forward failure)?
        if let Ok(Some(status)) = child.try_wait() {
            let mut err = String::new();
            if let Some(mut se) = child.stderr.take() {
                let _ = se.read_to_string(&mut err);
            }
            let err = err.trim();
            return Err(if err.is_empty() {
                format!("ssh exited ({status}) before binding local port {local_port}")
            } else {
                format!("ssh tunnel failed: {err}")
            });
        }
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "ssh did not bind local port {local_port} within {max:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub async fn attach_remote(
    Path(name): Path<String>,
    Json(req): Json<AttachRequest>,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };

    // Reuse an existing tunnel if one is already open for the same
    // (remote, ws_port). Avoids ssh-process leak on rapid re-attach.
    {
        let map = tunnels().lock().unwrap();
        if let Some(h) = map.get(&(name.clone(), req.ws_port)) {
            info!("reusing tunnel {} :{} → {}:{}", name, h.local_port, entry.ssh_target, req.ws_port);
            return (StatusCode::OK, Json(serde_json::to_value(&AttachResponse {
                local_port: h.local_port,
                remote: name.clone(),
                remote_ws_port: req.ws_port,
            }).unwrap()));
        }
    }

    let local_port = match pick_free_port() {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))),
    };

    // `-N` skip remote command, `-T` no pty, `-o ExitOnForwardFailure=yes`
    // make the child die if it couldn't open the forward (so we don't
    // silently hand back a port pointing nowhere). Run in background.
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-N", "-T",
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=5",
        "-o", "StrictHostKeyChecking=accept-new",
        "-o", "ExitOnForwardFailure=yes",
        "-o", "ServerAliveInterval=30",
        "-p", &entry.ssh_port.to_string(),
        "-L", &format!("127.0.0.1:{local_port}:127.0.0.1:{}", req.ws_port),
        &entry.ssh_target,
    ]);

    // Capture stderr so an auth/forward failure surfaces the real ssh
    // message (e.g. "Permission denied (publickey)") instead of a port
    // that points nowhere — see wait_for_tunnel().
    cmd.stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!("ssh tunnel spawn failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
        }
    };

    // Wait for ssh to actually bind the local forward (or fail), instead of
    // a fixed sleep. Returns in ~25ms on a fast LAN, waits up to 5s on a
    // cold/slow SSH, and reports ssh's stderr on auth/forward failure.
    if let Err(e) = wait_for_tunnel(&mut child, local_port, std::time::Duration::from_secs(5)) {
        warn!("tunnel {} :{} → {}:{} failed: {e}", name, local_port, entry.ssh_target, req.ws_port);
        let _ = child.kill();
        return (StatusCode::BAD_GATEWAY, Json(json!({ "error": e })));
    }

    info!("opened tunnel {} :{} → {}:{} (ssh pid {})",
          name, local_port, entry.ssh_target, req.ws_port, child.id());

    {
        let mut map = tunnels().lock().unwrap();
        map.insert((name.clone(), req.ws_port), TunnelHandle {
            local_port,
            child,
        });
    }

    (
        StatusCode::OK,
        Json(serde_json::to_value(&AttachResponse {
            local_port,
            remote: name,
            remote_ws_port: req.ws_port,
        }).unwrap()),
    )
}

// ─── spawn a new session on the remote ───────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SpawnRequest {
    /// Optional session name. If omitted, the remote daemon picks one
    /// (`immorterm-ai-<pid>-<rand>` per its default naming).
    #[serde(default)]
    pub session_name: Option<String>,
    /// Shell to spawn. Defaults to /bin/bash (debian-slim's default — the
    /// daemon's `default_shell()` reads $SHELL or falls back to /bin/zsh
    /// which the container doesn't ship).
    #[serde(default)]
    pub shell: Option<String>,
    /// Directory the new session's shell should `cd` into BEFORE the
    /// daemon double-forks. Without this the SSH command runs in
    /// `$HOME` (e.g. /root) on the remote even when the launching tab
    /// is bound to a project. Webview passes the tab's `_projectDir`.
    #[serde(default)]
    pub project_dir: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SpawnResponse {
    pub session_name: String,
    pub ws_port: u16,
    pub local_port: u16,
}

/// `POST /api/v1/remotes/{name}/spawn` — start a brand-new session on the
/// remote via SSH, wait for it to register, then open a tunnel and return
/// the local port the webview should connect to.
///
/// Mirrors what local `/api/new-session` does for local hubs, so the
/// gpu-terminal.html `'new-session'` handler can branch on `_remoteName`
/// and end up at the same shape of response either way.
pub async fn spawn_remote_session(
    Path(name): Path<String>,
    Json(req): Json<SpawnRequest>,
) -> (StatusCode, Json<Value>) {
    let Some(entry) = lookup_remote(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("no remote named '{name}'") })));
    };

    // Derive a session name client-side so we can immediately look it up
    // in the remote registry after spawn. Uses a hub-side timestamp to
    // sidestep clock drift across boxes.
    let session_name = req.session_name.unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // Match the daemon's own naming roughly: short-pid-rand
        format!("im-{ts:x}")
    });
    let shell = req.shell.unwrap_or_else(|| "/bin/bash".to_string());

    // Generate the per-session window id the SAME way local spawns do
    // (registry.rs `spawn_session` → generate_window_id() → cmd.env(
    // "IMMORTERM_WINDOW_ID", …)). The remote spawn historically OMITTED this,
    // so every remote session got window_id="" / empty IMMORTERM_ID.
    let window_id = super::registry::generate_window_id();

    // Step 1: ssh -dmS <name> on the remote. Daemon double-forks so this
    // command returns quickly. SHELL env is set explicitly because the
    // daemon's default_shell() reads $SHELL.
    let target = entry.ssh_target.clone();
    let ssh_port = entry.ssh_port;
    let session_for_cmd = session_name.clone();
    let shell_for_cmd = shell.clone();
    let window_id_for_cmd = window_id.clone();
    let project_dir_for_cmd = req.project_dir.clone().unwrap_or_default();
    let spawn_cmd = tokio::task::spawn_blocking(move || {
        // cd into the tab's project_dir first so the new shell's cwd
        // matches what the user sees in the tab. Default to $HOME when
        // unset. The `cd … 2>/dev/null || cd ~` chain falls back
        // gracefully if the project_dir doesn't exist on the remote
        // (e.g. user fat-fingered or the remote box hasn't checked
        // out the repo yet).
        let cd_clause = if project_dir_for_cmd.is_empty() {
            "cd ~".to_string()
        } else {
            // Shell-escape the path by single-quoting; embedded ' becomes '\''.
            let escaped = project_dir_for_cmd.replace('\'', "'\\''");
            format!("cd '{escaped}' 2>/dev/null || cd ~")
        };
        // Ensure a project .mcp.json exists BEFORE claude starts, or the box's
        // CC has no ImmorTerm Memory/tool MCP servers — the attachment
        // injection's explain_change()/get_code_diff()/immorterm_update_task()
        // tools simply don't exist there. slug = sanitized project basename,
        // used as the memory partition (matches what the hooks derive from the
        // .mcp.json URL). Runs after cd, so it writes into the project dir.
        let slug = std::path::Path::new(&project_dir_for_cmd)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("remote")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect::<String>();
        let slug = if slug.is_empty() { "remote".to_string() } else { slug };
        let ensure_mcp = format!(
            "[ -f .mcp.json ] || printf '%s' '{{\"mcpServers\":{{\"immorterm-memory\":{{\"type\":\"http\",\"url\":\"http://127.0.0.1:8765/mcp/claude-code/{slug}\"}},\"immorterm\":{{\"type\":\"stdio\",\"command\":\"immorterm-ai\",\"args\":[\"mcp\",\"serve\"]}}}}}}' > .mcp.json 2>/dev/null"
        );
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &ssh_port.to_string(),
                &target,
                // `< /dev/null` + redirected FDs prevent the double-forked
                // daemon from holding the SSH session open — otherwise the
                // ssh command waits indefinitely for the daemon's stdout
                // to close. Pipe to `:` so a non-zero exit on the
                // `(daemon …) &` path still produces success.
                // SCREEN_PROJECT_DIR is what the daemon writes into the
                // session's registry entry under `project_dir`. Without it
                // even a `cd /foo && immorterm-ai -dmS …` pair leaves
                // project_dir="" in the registry — defeating the picker's
                // grouping and the docker-demo→Cmd+Shift+A inheritance.
                // IMMORTERM_WINDOW_ID is what register_session() reads to set
                // the session's window_id; pty.rs then inherits it so the shell
                // (via shell-init) exports IMMORTERM_ID. Without it EVERY remote
                // session gets window_id="" / empty IMMORTERM_ID — which makes
                // all UserPromptSubmit hooks no-op (the [-n "$IMMORTERM_ID"]
                // guard) and the webview refuse attach/share (no session.windowId).
                // Use the session name as the stable per-session id.
                &format!(
                    "{cd_clause} && {ensure_mcp}; SCREEN_PROJECT_DIR='{esc_pd}' IMMORTERM_WINDOW_ID='{esc_wid}' SHELL={shell_for_cmd} immorterm-ai -dmS {session_for_cmd} < /dev/null > /dev/null 2>&1; true",
                    esc_pd = project_dir_for_cmd.replace('\'', "'\\''"),
                    esc_wid = window_id_for_cmd.replace('\'', "'\\''"),
                ),
            ])
            .output()
    })
    .await;

    let out = match spawn_cmd {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return (StatusCode::BAD_GATEWAY, Json(json!({ "error": format!("ssh: {e}") }))),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))),
    };
    if !out.status.success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": "remote daemon spawn failed",
                "stderr": String::from_utf8_lossy(&out.stderr),
                "exit": out.status.code(),
            })),
        );
    }

    // Step 2: poll the remote registry for the new session's ws_port. The
    // daemon writes it after double-fork; usually visible within a second
    // but we give it up to 5 polling attempts at 300ms each.
    let mut ws_port: Option<u16> = None;
    for _ in 0..15 {
        let entry_for_poll = entry.clone();
        let path = format!("{}/registry.json", entry_for_poll.immorterm_home);
        let probe = tokio::task::spawn_blocking(move || {
            Command::new("ssh")
                .args([
                    "-o", "BatchMode=yes",
                    "-o", "ConnectTimeout=5",
                    "-o", "StrictHostKeyChecking=accept-new",
                    "-p", &entry_for_poll.ssh_port.to_string(),
                    &entry_for_poll.ssh_target,
                    &format!("cat {path}"),
                ])
                .output()
        })
        .await;
        if let Ok(Ok(o)) = probe {
            if o.status.success() {
                if let Ok(v) = serde_json::from_slice::<Value>(&o.stdout) {
                    let sessions = v.get("sessions").and_then(|s| s.as_array());
                    if let Some(arr) = sessions {
                        for s in arr {
                            let n = s.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let p = s.get("ws_port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                            if n == session_name && p > 0 {
                                ws_port = Some(p);
                                break;
                            }
                        }
                    }
                }
            }
        }
        if ws_port.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    let Some(ws_port) = ws_port else {
        return (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({
                "error": "remote session spawned but ws_port did not register within 4.5s",
                "session_name": session_name,
            })),
        );
    };

    // Step 3: open the tunnel just like /attach. Reuse the attach logic
    // path by calling it inline — same in-process tunnel cache.
    let local_port = match attach_inner(&entry, ws_port).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))),
    };

    info!("spawned remote session {session_name} on {} → ws_port={ws_port}, local_port={local_port}",
          entry.ssh_target);

    (
        StatusCode::OK,
        Json(serde_json::to_value(&SpawnResponse {
            session_name,
            ws_port,
            local_port,
        }).unwrap()),
    )
}

/// Inline attach used by `spawn_remote_session` to avoid a second HTTP
/// round-trip back through the hub. Same logic as `attach_remote` body
/// but works against a `RemoteEntry` directly.
async fn attach_inner(entry: &RemoteEntry, ws_port: u16) -> Result<u16, String> {
    {
        let map = tunnels().lock().unwrap();
        if let Some(h) = map.get(&(entry.name.clone(), ws_port)) {
            return Ok(h.local_port);
        }
    }
    let local_port = pick_free_port().map_err(|e| e.to_string())?;
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-N", "-T",
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=5",
        "-o", "StrictHostKeyChecking=accept-new",
        "-o", "ExitOnForwardFailure=yes",
        "-o", "ServerAliveInterval=30",
        "-p", &entry.ssh_port.to_string(),
        "-L", &format!("127.0.0.1:{local_port}:127.0.0.1:{ws_port}"),
        &entry.ssh_target,
    ]);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    std::thread::sleep(std::time::Duration::from_millis(400));
    info!("opened tunnel (via spawn) {} :{} → {}:{} (ssh pid {})",
          entry.name, local_port, entry.ssh_target, ws_port, child.id());
    {
        let mut map = tunnels().lock().unwrap();
        map.insert(
            (entry.name.clone(), ws_port),
            TunnelHandle { local_port, child },
        );
    }
    Ok(local_port)
}

// ─── Registry watcher — push events to webviews ──────────────────────
//
// `inotifywait -m -e close_write` on the remote's registry.json. One
// persistent ssh per remote, started lazily the first time anyone
// subscribes via the events WebSocket. Each `close_write` triggers a
// re-fetch of registry.json over the same SSH channel; the diff vs the
// last known state is broadcast as JSON events on a tokio broadcast
// channel. The WebSocket handler forwards those events to subscribed
// webviews. Local fan-out only; the WS is the LOCAL hub serving the
// LOCAL webview — SSH→remote is one-way push (remote → hub via stdout
// of inotifywait).

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum RegistryEvent {
    #[serde(rename = "session_added")]
    SessionAdded { session: Value },
    #[serde(rename = "session_removed")]
    SessionRemoved { name: String },
    #[serde(rename = "snapshot")]
    Snapshot { sessions: Vec<Value> },
    /// Fired when the remote's `~/.immorterm/config.json` changes. The
    /// webview re-fetches /api/v1/remotes/<name>/config and re-emits the
    /// theme + menu + preferences messages to the WASM renderer — live
    /// theme switches with no reload.
    #[serde(rename = "config_changed")]
    ConfigChanged,
    #[serde(rename = "remote_disconnected")]
    RemoteDisconnected { reason: String },
}

struct WatcherHandle {
    tx: broadcast::Sender<RegistryEvent>,
}

fn watchers() -> &'static tokio::sync::Mutex<StdHashMap<String, WatcherHandle>> {
    static W: TokioOnceCell<tokio::sync::Mutex<StdHashMap<String, WatcherHandle>>> = TokioOnceCell::const_new();
    // We block_on init only in a non-async context — but this getter is
    // only called from async, so use try_get_or_init() pattern via leak.
    // OnceLock works fine for Mutex<HashMap> — no async init needed.
    static SYNC_W: std::sync::OnceLock<tokio::sync::Mutex<StdHashMap<String, WatcherHandle>>> = std::sync::OnceLock::new();
    let _ = &W; // silence unused
    SYNC_W.get_or_init(|| tokio::sync::Mutex::new(StdHashMap::new()))
}

/// Spawn (or return existing) a watcher for the given remote. The first
/// caller wins; subsequent calls reuse the same broadcast channel.
async fn ensure_watcher(name: &str, entry: RemoteEntry) -> broadcast::Sender<RegistryEvent> {
    let mut map = watchers().lock().await;
    if let Some(h) = map.get(name) {
        return h.tx.clone();
    }
    let (tx, _rx) = broadcast::channel(64);
    let tx_for_task = tx.clone();
    let name_owned = name.to_string();
    map.insert(name.to_string(), WatcherHandle { tx: tx.clone() });
    drop(map);

    tokio::spawn(async move {
        run_watcher(name_owned, entry, tx_for_task).await;
    });

    tx
}

async fn run_watcher(
    name: String,
    entry: RemoteEntry,
    tx: broadcast::Sender<RegistryEvent>,
) {
    // Watch the global `~/.immorterm/` dir PLUS every per-project
    // `<project_dir>/.immorterm/` enumerated from the remote registry.
    // Per-project theme changes write to those files, so a watcher
    // rooted only at `~/.immorterm/` misses them. We re-enumerate
    // every time a session_added event lands with a new project_dir,
    // killing + respawning the inotifywait subprocess with an expanded
    // watch set. Keeps watches tight (no `-r` blow-up from
    // node_modules) while still catching projects created after the
    // watcher started.
    let watch_dir = entry.immorterm_home.clone();

    // Helper: build the inotifywait command given the current set of
    // project_dirs we need to track.
    let build_cmd = |project_dirs: &[String]| -> String {
        let extra: Vec<String> = project_dirs
            .iter()
            .filter(|pd| !pd.is_empty())
            .map(|pd| format!("'{}/.immorterm/'", pd.replace('\'', "'\\''")))
            .collect();
        let extra_args = if extra.is_empty() {
            String::new()
        } else {
            extra.join(" ")
        };
        format!(
            "stdbuf -oL inotifywait -m -q -e close_write --format '%w%f' \
                {watch_dir}/ {extra_args} \
                2>&1 || echo WATCH_FAILED",
        )
    };
    let remote_cmd = build_cmd(&[]);

    info!("[watcher:{name}] starting inotifywait on {} via ssh {}", watch_dir, entry.ssh_target);

    // Need `let mut` so we can respawn the child later with a wider
    // watch set when a new project_dir shows up.
    #[allow(unused_mut)]
    let mut child = match tokio::process::Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", "ServerAliveInterval=30",
            "-p", &entry.ssh_port.to_string(),
            &entry.ssh_target,
            &remote_cmd,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!("[watcher:{name}] ssh spawn failed: {e}");
            let _ = tx.send(RegistryEvent::RemoteDisconnected {
                reason: format!("ssh spawn failed: {e}"),
            });
            return;
        }
    };
    let stdout = child.stdout.take().expect("piped");
    #[allow(unused_mut)]
    let mut reader = BufReader::new(stdout).lines();

    // Initial snapshot — emit it BEFORE any inotify events so webviews
    // can build their baseline. (Doubles as a sanity check the SSH path
    // works.)
    let mut last: HashMap<String, Value> = HashMap::new();
    let mut watched_project_dirs: HashSet<String> = HashSet::new();
    if let Some(initial) = fetch_remote_sessions(&entry).await {
        for s in &initial {
            if let Some(n) = s.get("name").and_then(|v| v.as_str()) {
                last.insert(n.to_string(), s.clone());
            }
            if let Some(pd) = s.get("project_dir").and_then(|v| v.as_str()).map(|s| s.to_string()) {
                if !pd.is_empty() {
                    watched_project_dirs.insert(pd);
                }
            }
        }
        // If we discovered project_dirs in the initial snapshot, restart
        // inotifywait with them included.
        if !watched_project_dirs.is_empty() {
            let new_cmd = build_cmd(&watched_project_dirs.iter().cloned().collect::<Vec<_>>());
            let _ = child.kill().await;
            child = match tokio::process::Command::new("ssh")
                .args([
                    "-o", "BatchMode=yes",
                    "-o", "ConnectTimeout=5",
                    "-o", "StrictHostKeyChecking=accept-new",
                    "-o", "ServerAliveInterval=30",
                    "-p", &entry.ssh_port.to_string(),
                    &entry.ssh_target,
                    &new_cmd,
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    warn!("[watcher:{name}] ssh respawn failed: {e}");
                    let _ = tx.send(RegistryEvent::RemoteDisconnected {
                        reason: format!("respawn failed: {e}"),
                    });
                    return;
                }
            };
            let stdout = child.stdout.take().expect("piped");
            reader = BufReader::new(stdout).lines();
            info!("[watcher:{name}] expanded watch set: {} project dir(s)", watched_project_dirs.len());
        }
        let _ = tx.send(RegistryEvent::Snapshot { sessions: initial });
    }

    loop {
        tokio::select! {
            line = reader.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        if l == "WATCH_FAILED" || l.contains("WATCH_FAILED") {
                            warn!("[watcher:{name}] remote inotifywait failed");
                            let _ = tx.send(RegistryEvent::RemoteDisconnected {
                                reason: l,
                            });
                            break;
                        }
                        // Demux by basename. inotifywait prints the full
                        // path (we passed --format '%w%f'). config.json
                        // fires a separate event so the webview can hot-
                        // reload theme/menu/prefs; registry.json triggers
                        // session add/remove diffing.
                        let basename = std::path::Path::new(&l)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("");
                        if basename == "config.json" {
                            info!("[watcher:{name}] config.json changed → broadcasting config_changed");
                            let _ = tx.send(RegistryEvent::ConfigChanged);
                            continue;
                        }
                        if basename != "registry.json" {
                            // Ignore other files in ~/.immorterm/ that
                            // we don't care about (logs, sockets, etc.).
                            continue;
                        }
                        // Re-fetch registry and diff
                        if let Some(now) = fetch_remote_sessions(&entry).await {
                            let now_names: HashSet<String> = now.iter()
                                .filter_map(|s| s.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()))
                                .collect();
                            let prev_names: HashSet<String> = last.keys().cloned().collect();
                            // Track new project_dirs to decide if we need
                            // to expand the watch set.
                            let mut new_project_dirs: Vec<String> = Vec::new();
                            for added in now_names.difference(&prev_names) {
                                if let Some(s) = now.iter().find(|s| s.get("name").and_then(|v| v.as_str()) == Some(added.as_str())) {
                                    let _ = tx.send(RegistryEvent::SessionAdded { session: s.clone() });
                                    if let Some(pd) = s.get("project_dir").and_then(|v| v.as_str()).map(|s| s.to_string()) {
                                        if !pd.is_empty() && !watched_project_dirs.contains(&pd) {
                                            new_project_dirs.push(pd);
                                        }
                                    }
                                }
                            }
                            for removed in prev_names.difference(&now_names) {
                                let _ = tx.send(RegistryEvent::SessionRemoved { name: removed.clone() });
                            }
                            last.clear();
                            for s in now {
                                if let Some(n) = s.get("name").and_then(|v| v.as_str()) {
                                    last.insert(n.to_string(), s);
                                }
                            }
                            // Respawn inotifywait if new project_dirs are
                            // here. Avoids missing per-project config
                            // changes on projects created after watcher
                            // start.
                            if !new_project_dirs.is_empty() {
                                for pd in new_project_dirs {
                                    watched_project_dirs.insert(pd);
                                }
                                let new_cmd = build_cmd(&watched_project_dirs.iter().cloned().collect::<Vec<_>>());
                                let _ = child.kill().await;
                                match tokio::process::Command::new("ssh")
                                    .args([
                                        "-o", "BatchMode=yes",
                                        "-o", "ConnectTimeout=5",
                                        "-o", "StrictHostKeyChecking=accept-new",
                                        "-o", "ServerAliveInterval=30",
                                        "-p", &entry.ssh_port.to_string(),
                                        &entry.ssh_target,
                                        &new_cmd,
                                    ])
                                    .stdout(Stdio::piped())
                                    .stderr(Stdio::piped())
                                    .spawn()
                                {
                                    Ok(c) => {
                                        child = c;
                                        let stdout = child.stdout.take().expect("piped");
                                        reader = BufReader::new(stdout).lines();
                                        info!("[watcher:{name}] watch set expanded to {} project dir(s)", watched_project_dirs.len());
                                    }
                                    Err(e) => {
                                        warn!("[watcher:{name}] respawn after new project_dir failed: {e}");
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        info!("[watcher:{name}] ssh stdout closed — exiting watcher");
                        let _ = tx.send(RegistryEvent::RemoteDisconnected {
                            reason: "ssh inotify stream ended".to_string(),
                        });
                        break;
                    }
                    Err(e) => {
                        warn!("[watcher:{name}] read error: {e}");
                        break;
                    }
                }
            }
        }
    }
    // Clean up child + drop watcher entry so a future subscribe restarts.
    let _ = child.kill().await;
    let mut map = watchers().lock().await;
    map.remove(&name);
    info!("[watcher:{name}] exited");
}

/// One-shot SSH fetch of the remote registry, returning the sessions
/// array (tagged with remote name + alive:true to match the rest of the
/// API). None on transport failure.
async fn fetch_remote_sessions(entry: &RemoteEntry) -> Option<Vec<Value>> {
    let path = format!("{}/registry.json", entry.immorterm_home);
    let entry_clone = entry.clone();
    let out = tokio::task::spawn_blocking(move || {
        Command::new("ssh")
            .args([
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=5",
                "-o", "StrictHostKeyChecking=accept-new",
                "-p", &entry_clone.ssh_port.to_string(),
                &entry_clone.ssh_target,
                &format!("cat {path}"),
            ])
            .output()
    })
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    let mut sessions = v
        .get("sessions")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    for s in &mut sessions {
        if let Some(obj) = s.as_object_mut() {
            obj.insert("remote".to_string(), json!(entry.name));
            obj.entry("alive").or_insert(json!(true));
        }
    }
    Some(sessions)
}

/// `GET /api/v1/remotes/{name}/events` — WebSocket upgrade. Each connected
/// webview receives a `snapshot` event first, then `session_added` /
/// `session_removed` per change. Connection stays open until the webview
/// closes it or the watcher dies.
pub async fn remote_events_ws(
    Path(name): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_remote_events(name, socket))
}

async fn handle_remote_events(name: String, mut socket: WebSocket) {
    let Some(entry) = lookup_remote(&name) else {
        let _ = socket
            .send(WsMessage::Text(
                serde_json::to_string(&json!({"error":format!("no remote named '{name}'")}))
                    .unwrap()
                    .into(),
            ))
            .await;
        return;
    };
    let tx = ensure_watcher(&name, entry).await;
    let mut rx = tx.subscribe();

    info!("[events-ws:{name}] client connected");

    loop {
        tokio::select! {
            ev = rx.recv() => {
                match ev {
                    Ok(e) => {
                        let msg = serde_json::to_string(&e).unwrap_or_else(|_| "{}".to_string());
                        if socket.send(WsMessage::Text(msg.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[events-ws:{name}] lagged {n} events — refreshing snapshot");
                        // Recover by re-sending a snapshot.
                        if let Some(entry) = lookup_remote(&name) {
                            if let Some(sessions) = fetch_remote_sessions(&entry).await {
                                let msg = serde_json::to_string(&RegistryEvent::Snapshot { sessions }).unwrap();
                                if socket.send(WsMessage::Text(msg.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            client_msg = socket.next() => {
                match client_msg {
                    Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(WsMessage::Ping(p))) => { let _ = socket.send(WsMessage::Pong(p)).await; }
                    _ => {}
                }
            }
        }
    }
    info!("[events-ws:{name}] client disconnected");
}
