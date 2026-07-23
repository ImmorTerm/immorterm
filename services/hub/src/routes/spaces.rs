//! Spaces API — read/write of ~/.immorterm/spaces/<project_id>/index.json,
//! the per-project docking-grid model (SP2). Unlike plans (daemon-written,
//! hub read-only), the WEBVIEW owns spaces, so the hub both lists AND saves.
//!
//! Project-id resolution MIRRORS the daemon's get_stable_project_id and the
//! plans route (plans.rs:72) — the webview/extension must derive the identical
//! id so a space created in VS Code is found by the hub-served tab and vice
//! versa: git remote user-repo → sanitized .claude/project-id → basename.
//!
//! ponytail: helpers here are cloned from plans.rs (the established route
//! pattern — tasks.rs clones them too). Extract routes/project_id.rs only if a
//! third writer appears; two isn't worth reworking plans.rs's daemon-parity
//! comments.

use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// CSRF guard (plans.rs:24 parity). Saving a space is a disk write, not a
/// terminal type, but keep the same allowlist so a cross-site page can't
/// silently rewrite a user's grid layout.
fn origin_is_trusted(headers: &HeaderMap) -> bool {
    match headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true, // same-origin POST or non-browser (VS Code node fetch)
        Some(o) => {
            let host = o
                .split("://")
                .nth(1)
                .unwrap_or(o)
                .split('/')
                .next()
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("");
            host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1"
        }
    }
}

fn spaces_root() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".immorterm/spaces")
}

/// Daemon-parity sanitizer (plans.rs:52): lowercase, non-alnum → '-', trim
/// '-', cap 50, fallback. Keeps space files under the SAME project slug the
/// daemon/plans use so nothing is orphaned.
fn sanitize_project_id(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let t = s.trim_matches('-');
    if t.is_empty() {
        "unnamed-project".into()
    } else {
        t.chars().take(50).collect()
    }
}

/// Daemon-parity (plans.rs:63): [:/]user/repo(.git)?$ → "user-repo" lowercase.
fn extract_user_repo(url: &str) -> Option<String> {
    let stripped = url.strip_suffix(".git").unwrap_or(url);
    let parts: Vec<&str> = stripped.rsplitn(3, ['/', ':']).collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        return Some(format!("{}-{}", parts[1], parts[0]).to_lowercase());
    }
    None
}

fn resolve_project_id(project_dir: &str) -> String {
    // 1. git remote origin (daemon branch 1)
    if !project_dir.is_empty() {
        if let Ok(out) = std::process::Command::new("git")
            .args(["config", "--get", "remote.origin.url"])
            .current_dir(project_dir)
            .output()
        {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !url.is_empty() {
                if let Some(id) = extract_user_repo(&url) {
                    return id;
                }
            }
        }
    }
    // 2. .claude/project-id — SANITIZED (traversal-safe)
    if let Ok(s) = std::fs::read_to_string(PathBuf::from(project_dir).join(".claude/project-id")) {
        let t = s.trim();
        if !t.is_empty() {
            return sanitize_project_id(t);
        }
    }
    // 3. folder basename
    sanitize_project_id(
        &PathBuf::from(project_dir)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    )
}

/// flock(2) exclusive lock on `<dir>/.lock`, released on drop (plans.rs:165).
struct SpaceLock(#[allow(dead_code)] std::fs::File);

fn lock_space_dir(dir: &Path) -> std::io::Result<SpaceLock> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::File::create(dir.join(".lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(SpaceLock(file))
}

/// Atomic write: per-PID tmp + rename (plans.rs:175 parity).
fn atomic_write_json(path: &Path, value: &Value) -> Result<(), String> {
    let content =
        serde_json::to_string_pretty(value).map_err(|e| format!("Serialize error: {}", e))?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &content).map_err(|e| format!("Write error: {}", e))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("Rename error: {}", e))?;
    Ok(())
}

/// Empty index the webview gets before any space is created.
fn empty_index() -> Value {
    json!({ "version": 1, "order": [], "active": null, "spaces": {} })
}

#[derive(Deserialize)]
pub struct SpacesQuery {
    pub project_dir: Option<String>,
}

/// GET /api/v1/spaces?project_dir=... → { project, index: <index.json | empty> }
pub async fn list_spaces(Query(q): Query<SpacesQuery>) -> Json<Value> {
    let project = resolve_project_id(&q.project_dir.unwrap_or_default());
    let path = spaces_root().join(&project).join("index.json");
    let index = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(empty_index);
    Json(json!({ "project": project, "index": index }))
}

#[derive(Deserialize)]
pub struct SaveReq {
    pub project_dir: Option<String>,
    /// The whole index.json blob — the webview is the single writer, so it
    /// sends the full { version, order, active, spaces } on every debounced save.
    pub index: Value,
}

/// POST /api/v1/spaces/save
/// Body: { project_dir, index: { version, order, active, spaces } }
/// 200 → { ok: true, project }; 400 malformed index; 403 cross-origin; 500 io.
pub async fn save_space(
    headers: HeaderMap,
    Json(req): Json<SaveReq>,
) -> (StatusCode, Json<Value>) {
    if !origin_is_trusted(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "cross-origin save rejected" })),
        );
    }
    // Minimal shape guard — the blob is otherwise opaque (it carries the
    // dockview toJSON() geometry, which the hub never interprets).
    if !req.index.is_object() || req.index.get("spaces").map(|s| !s.is_object()).unwrap_or(true) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "index must be an object with a `spaces` object" })),
        );
    }
    let project = resolve_project_id(req.project_dir.as_deref().unwrap_or(""));
    let dir = spaces_root().join(&project);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("mkdir error: {}", e) })),
        );
    }
    let _lock = match lock_space_dir(&dir) {
        Ok(l) => l,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Lock error: {}", e) })),
            )
        }
    };
    if let Err(e) = atomic_write_json(&dir.join("index.json"), &req.index) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e })));
    }
    (StatusCode::OK, Json(json!({ "ok": true, "project": project })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_and_repo_match_daemon_rules() {
        // project-id sanitization parity with plans/daemon.
        assert_eq!(sanitize_project_id("../.."), "unnamed-project");
        assert_eq!(sanitize_project_id("My.Project"), "my-project");
        assert_eq!(
            extract_user_repo("git@github.com:user/repo.git").as_deref(),
            Some("user-repo")
        );
    }

    #[test]
    fn save_then_list_round_trips_index() {
        // Write to a private temp dir (NOT via spaces_root() — mutating HOME
        // would race the other tests that read it under cargo's parallel run).
        let dir = std::env::temp_dir().join(format!("hub-spaces-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let index = json!({
            "version": 1,
            "order": ["sp_a"],
            "active": "sp_a",
            "spaces": {
                "sp_a": {
                    "name": "Backend",
                    "createdMs": 1_737_600_000_000u64,
                    "layout": { "grid": { "root": { "type": "leaf" }, "width": 800 } },
                    "tiles": { "panel_primary": { "kind": "primary", "locked": false } },
                    "gridLocked": true
                }
            }
        });

        // Write via the same atomic+flock path the route uses.
        let _lock = lock_space_dir(&dir).unwrap();
        atomic_write_json(&dir.join("index.json"), &index).unwrap();
        drop(_lock);

        // Read back exactly (geometry blob + lock state preserved verbatim).
        let read = std::fs::read_to_string(dir.join("index.json")).unwrap();
        let back: Value = serde_json::from_str(&read).unwrap();
        assert_eq!(back, index, "index round-trips byte-for-byte through disk");
        assert_eq!(back["spaces"]["sp_a"]["gridLocked"], true);
        assert_eq!(back["spaces"]["sp_a"]["layout"]["grid"]["width"], 800);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_flock_serializes_writers() {
        let dir = std::env::temp_dir().join(format!("hub-space-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let l1 = lock_space_dir(&dir).unwrap();
        use std::os::unix::io::AsRawFd;
        let f2 = std::fs::File::create(dir.join(".lock")).unwrap();
        // While held: a NON-BLOCKING attempt from another open description must fail.
        let rc = unsafe { libc::flock(f2.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_ne!(rc, 0, "second flock should fail while first is held");
        drop(l1);
        // After release: a BLOCKING acquire (what the route actually uses) succeeds.
        // A non-blocking retry here flakes on macOS — a just-released flock isn't
        // instantly visible to LOCK_NB from another fd; the blocking form waits it out.
        let rc = unsafe { libc::flock(f2.as_raw_fd(), libc::LOCK_EX) };
        assert_eq!(rc, 0, "lock acquirable after release");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
