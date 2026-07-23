//! Plans API — read-only view of ~/.immorterm/plans/<project_id>/<plan_id>/current.json,
//! written by the daemon's immorterm_plan MCP tools (S3). Standalone/Tauri
//! webviews can't read disk, so the hub lists them.
//!
//! Project-id resolution MIRRORS the daemon's get_stable_project_id
//! (immorterm-daemon/src/mcp.rs:4757) — the daemon writes the dirs, so the
//! reader must derive the identical id: git remote user-repo → sanitized
//! .claude/project-id → sanitized folder basename.

use axum::extract::Query;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_matches_daemon_rules() {
        assert_eq!(sanitize_project_id("../.."), "unnamed-project");
        assert_eq!(sanitize_project_id("My.Project"), "my-project");
        assert_eq!(extract_user_repo("git@github.com:user/repo.git").as_deref(), Some("user-repo"));
    }
}
