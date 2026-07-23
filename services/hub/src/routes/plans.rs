//! Plans API — read-only view of ~/.immorterm/plans/<project_id>/<plan_id>/current.json,
//! written by the daemon's immorterm_plan MCP tools (S3). Standalone/Tauri
//! webviews can't read disk, so the hub lists them.
//!
//! Project-id resolution MIRRORS the daemon's get_stable_project_id
//! (immorterm-daemon/src/mcp.rs:4757) — the daemon writes the dirs, so the
//! reader must derive the identical id: git remote user-repo → sanitized
//! .claude/project-id → sanitized folder basename.

use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// CSRF guard for state-changing hub routes. The hub sits behind
/// CorsLayer::permissive() and its write routes are unauthenticated — safe for
/// the same-origin webview and the VS Code extension's loopback fetch (no
/// browser Origin), but `submit_plan` also TYPES INTO THE TERMINAL, so a
/// cross-site page must not be able to drive it. Reject any request whose
/// Origin is a real remote site; allow absent Origin (same-origin / non-browser)
/// and loopback origins.
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

fn plans_root() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".immorterm/plans")
}

/// Daemon-parity sanitizer (mcp.rs:4817): lowercase, non-alnum → '-', trim
/// '-', cap 50, fallback. NOT tasks.rs::sanitize_project_id (keeps case + _).
fn sanitize_project_id(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let t = s.trim_matches('-');
    if t.is_empty() { "unnamed-project".into() } else { t.chars().take(50).collect() }
}

/// Daemon-parity (mcp.rs:4803): [:/]user/repo(.git)?$ → "user-repo" lowercase.
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
    // 2. .claude/project-id — SANITIZED (S3 hardening parity, traversal-safe)
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

#[derive(Deserialize)]
pub struct PlansQuery {
    pub project_dir: Option<String>,
}

/// GET /api/v1/plans?project_dir=... → { project, plans: [current.json records] }
/// Full records incl. html — a project has a handful of plans and this is
/// loopback. ponytail: add ?fields=summary + /plans/{id} if payloads grow.
pub async fn list_plans(Query(q): Query<PlansQuery>) -> Json<Value> {
    let project = resolve_project_id(&q.project_dir.unwrap_or_default());
    let dir = plans_root().join(&project);
    let mut plans: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let cur = e.path().join("current.json");
            if let Ok(s) = std::fs::read_to_string(&cur) {
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    plans.push(v);
                }
                // corrupt current.json → skip; daemon sidelines it on next write
            }
        }
    }
    Json(json!({ "project": project, "plans": plans }))
}

// ── POST /api/v1/plans/submit — the ONE write path for the Plans UI ─────
//
// One submission = one flock-guarded read-modify-write of current.json:
// resolutions flip decisions in place, comments append with fresh ids,
// updatedAt bumps — NO history snapshot, NO revision bump, identical
// semantics to the daemon's handle_resolve_plan_decision (mcp.rs:5565).
// The daemon's merge_comments carries these comment ids forward on any
// later agent supersede, so nothing here can be silently dropped.
//
// LOCK PARITY: the daemon locks the same `<dir>/.lock` via
// nix::fcntl::Flock (flock(2), mcp.rs lock_plan_dir); we use libc::flock —
// both are flock(2) advisory locks so they interoperate. If the daemon
// ever changes lock mechanism, this must change with it.

/// Daemon-parity plan-id validation (mcp.rs validate_plan_id) — ids are
/// path components, so this is a trust boundary.
fn validate_plan_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("Plan id cannot be empty".into());
    }
    if id.len() > 64 {
        return Err("Plan id too long (max 64 chars)".into());
    }
    if id.contains('/') || id.contains('\\') || id == "." || id.contains("..") {
        return Err("Plan id must not contain path separators or '..'".into());
    }
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err("Plan id may only contain [a-zA-Z0-9_.-]".into());
    }
    Ok(())
}

/// flock(2) exclusive lock on `<dir>/.lock`, released on drop (fd close).
struct PlanLock(#[allow(dead_code)] std::fs::File);

fn lock_plan_dir(dir: &Path) -> std::io::Result<PlanLock> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::File::create(dir.join(".lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(PlanLock(file))
}

/// Atomic write: per-PID tmp + rename (daemon atomic_write_json parity).
fn atomic_write_json(path: &Path, value: &Value) -> Result<(), String> {
    let content =
        serde_json::to_string_pretty(value).map_err(|e| format!("Serialize error: {}", e))?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &content).map_err(|e| format!("Write error: {}", e))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("Rename error: {}", e))?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// WHO wrote the comment — CLAUDE.md identity order:
/// ~/.immorterm/identity.json user_id → IMMORTERM_USER_ID → $USER@$HOSTNAME.
fn comment_author() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    if let Ok(s) = std::fs::read_to_string(PathBuf::from(&home).join(".immorterm/identity.json"))
        && let Ok(v) = serde_json::from_str::<Value>(&s)
        && let Some(u) = v.get("user_id").and_then(|u| u.as_str())
        && !u.is_empty()
    {
        return u.to_string();
    }
    if let Ok(u) = std::env::var("IMMORTERM_USER_ID")
        && !u.is_empty()
    {
        return u;
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".into());
    format!("{}@{}", user, host)
}

/// Flip a decision in place — port of the daemon's resolve_decision_in_plan.
fn resolve_decision(plan: &mut Value, decision_id: &str, resolution: &str) -> Result<(), String> {
    let decisions = plan
        .get_mut("decisions")
        .and_then(|d| d.as_array_mut())
        .ok_or("Plan has no decisions")?;
    let decision = decisions
        .iter_mut()
        .find(|d| d.get("id").and_then(|i| i.as_str()) == Some(decision_id))
        .ok_or_else(|| format!("Decision not found: {}", decision_id))?;
    decision["resolved"] = json!(true);
    decision["resolution"] = json!(resolution);
    Ok(())
}

/// Best-effort NotifyPlanChanged to the plan's attached session daemon over
/// its unix socket (~/.immorterm/sockets/<pid>.<name>). Wire format = serde
/// of the daemon's Request enum (tag = "type"), verified by the daemon's
/// notify_plan_changed_request_serializes test. Failure never fails the
/// submit — files are the source of truth.
async fn notify_plan_changed(session: &str, project: &str, id: &str, plan: &Value) {
    if session.is_empty() {
        return;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let sockets = PathBuf::from(home).join(".immorterm/sockets");
    let Ok(entries) = std::fs::read_dir(&sockets) else { return };
    let mut socket = None;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // Filename is `<pid>.<session>`; skip `.ws` companion sockets.
        if let Some((pid_str, rest)) = name.split_once('.')
            && rest == session
            && let Ok(pid) = pid_str.parse::<i32>()
            && unsafe { libc::kill(pid, 0) } == 0
        {
            socket = Some(e.path());
            break;
        }
    }
    let Some(socket) = socket else { return };
    let unresolved = plan
        .get("decisions")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter(|d| d.get("resolved").and_then(|r| r.as_bool()) != Some(true))
                .count() as u64
        })
        .unwrap_or(0);
    let req = json!({
        "type": "NotifyPlanChanged",
        "project": project,
        "id": id,
        "status": plan.get("status").and_then(|v| v.as_str()).unwrap_or(""),
        "title": plan.get("title").and_then(|v| v.as_str()).unwrap_or(""),
        "summary": plan.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
        "unresolved_decisions": unresolved,
    });
    if let Ok(mut stream) = tokio::net::UnixStream::connect(&socket).await {
        use tokio::io::AsyncWriteExt;
        let _ = stream
            .write_all(&serde_json::to_vec(&req).unwrap_or_default())
            .await;
        // Drain the daemon's one-line Response best-effort so it isn't
        // writing into a closed socket; ignore content and timeouts.
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 1024];
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            stream.read(&mut buf),
        )
        .await;
    }
}

#[derive(Deserialize)]
pub struct SubmitResolution {
    pub decision_id: String,
    pub resolution: String,
}

#[derive(Deserialize)]
pub struct SubmitComment {
    #[serde(rename = "sectionId")]
    pub section_id: Option<String>,
    #[serde(rename = "decisionId")]
    pub decision_id: Option<String>,
    pub text: String,
}

#[derive(Deserialize)]
pub struct SubmitReq {
    pub project_dir: Option<String>,
    pub plan_id: String,
    #[serde(default)]
    pub resolutions: Vec<SubmitResolution>,
    #[serde(default)]
    pub comments: Vec<SubmitComment>,
}

/// Pure core of the submit: apply resolutions + append comments to the plan
/// value. Errors before any mutation is persisted (caller only writes on Ok).
fn apply_submission(
    plan: &mut Value,
    resolutions: &[SubmitResolution],
    comments: &[SubmitComment],
    author: &str,
    now: u64,
) -> Result<(), String> {
    for r in resolutions {
        resolve_decision(plan, &r.decision_id, &r.resolution)?;
    }
    if !comments.is_empty() {
        if plan.get("comments").and_then(|c| c.as_array()).is_none() {
            plan["comments"] = json!([]);
        }
        let arr = plan["comments"].as_array_mut().expect("just ensured");
        for (i, c) in comments.iter().enumerate() {
            let text = c.text.trim();
            if text.is_empty() {
                continue;
            }
            let mut entry = json!({
                // ponytail: ms-timestamp + pid + index, not uuid (no uuid dep
                // in the hub) — unique enough for one plan file's comment ids.
                "id": format!("c{}-{}-{}", now, std::process::id(), i),
                "text": text,
                "author": author,
                "ts": now,
            });
            if let Some(s) = &c.section_id {
                entry["sectionId"] = json!(s);
            }
            if let Some(d) = &c.decision_id {
                entry["decisionId"] = json!(d);
            }
            arr.push(entry);
        }
    }
    plan["updatedAt"] = json!(now);
    Ok(())
}

/// POST /api/v1/plans/submit
/// Body: { project_dir, plan_id, resolutions: [{decision_id, resolution}],
///         comments: [{sectionId?, decisionId?, text}] }
/// 200 → { plan: <updated record> }; 400 bad id / unknown decision;
/// 404 plan not found; 500 io.
pub async fn submit_plan(
    headers: HeaderMap,
    Json(req): Json<SubmitReq>,
) -> (StatusCode, Json<Value>) {
    if !origin_is_trusted(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "cross-origin submit rejected" })),
        );
    }
    if let Err(e) = validate_plan_id(&req.plan_id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": e })));
    }
    let project = resolve_project_id(req.project_dir.as_deref().unwrap_or(""));
    let dir = plans_root().join(&project).join(&req.plan_id);
    if !dir.join("current.json").exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Plan not found: {}", req.plan_id) })),
        );
    }
    // std flock + std fs inside an async handler: calls are all loopback-fast
    // and the lock is held for one read-modify-write; fine on axum's runtime.
    let _lock = match lock_plan_dir(&dir) {
        Ok(l) => l,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Lock error: {}", e) })),
            )
        }
    };
    let mut plan: Value = match std::fs::read_to_string(dir.join("current.json"))
        .map_err(|e| format!("Read error: {}", e))
        .and_then(|s| serde_json::from_str(&s).map_err(|e| format!("Parse error: {}", e)))
    {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e }))),
    };
    if let Err(e) = apply_submission(
        &mut plan,
        &req.resolutions,
        &req.comments,
        &comment_author(),
        now_ms(),
    ) {
        // Unknown decision_id etc. — nothing written.
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": e })));
    }
    if let Err(e) = atomic_write_json(&dir.join("current.json"), &plan) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e })));
    }
    drop(_lock);
    let session = plan
        .get("sessionName")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    notify_plan_changed(&session, &project, &req.plan_id, &plan).await;
    (StatusCode::OK, Json(json!({ "plan": plan })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_matches_daemon_rules() {
        assert_eq!(sanitize_project_id("../.."), "unnamed-project");
        assert_eq!(sanitize_project_id("My.Project"), "my-project");
        assert_eq!(extract_user_repo("git@github.com:user/repo.git").as_deref(), Some("user-repo"));
    }

    #[test]
    fn apply_submission_resolves_and_appends_atomically() {
        let mut plan = json!({
            "decisions": [
                { "id": "d1", "label": "L", "options": ["A", "B"], "resolved": false },
                { "id": "d2", "label": "M", "options": ["X"], "resolved": false }
            ],
        });
        let res = vec![SubmitResolution { decision_id: "d1".into(), resolution: "A".into() }];
        let com = vec![
            SubmitComment { section_id: Some("arch".into()), decision_id: None, text: "note".into() },
            SubmitComment { section_id: None, decision_id: None, text: "   ".into() }, // dropped
        ];
        apply_submission(&mut plan, &res, &com, "shai", 1000).unwrap();
        assert_eq!(plan["decisions"][0]["resolved"], true);
        assert_eq!(plan["decisions"][0]["resolution"], "A");
        assert_eq!(plan["decisions"][1]["resolved"], false);
        let comments = plan["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["sectionId"], "arch");
        assert_eq!(comments[0]["author"], "shai");
        assert_eq!(plan["updatedAt"], 1000);

        // Unknown decision id → error, and the caller must not persist.
        let bad = vec![SubmitResolution { decision_id: "nope".into(), resolution: "A".into() }];
        assert!(apply_submission(&mut plan, &bad, &[], "shai", 2000).is_err());
    }

    #[test]
    fn concurrent_flock_serializes_writers() {
        // The safety property the route depends on: two lockers on the same
        // dir cannot hold the lock simultaneously.
        let dir = std::env::temp_dir().join(format!("hub-plan-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let l1 = lock_plan_dir(&dir).unwrap();
        // Non-blocking attempt from "another writer" must fail while held.
        use std::os::unix::io::AsRawFd;
        let f2 = std::fs::File::create(dir.join(".lock")).unwrap();
        let rc = unsafe { libc::flock(f2.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_ne!(rc, 0, "second flock should fail while first is held");
        drop(l1);
        let rc = unsafe { libc::flock(f2.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "lock acquirable after release");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
