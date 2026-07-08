//! API endpoints that back the standalone `gpu-terminal.html` popup menu
//! (Diagnostics, Services, Logs, License, Insights). Each one answers a
//! single modal request so the standalone UI matches the VS Code extension
//! without the extension host running.
//!
//! All responses are fire-and-forget: no mutation here. Mutations are
//! handled by /api/v1/registry/* or /api/v1/config/*.

use axum::{extract::Query, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Helpers ──────────────────────────────────────────────────────────

fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn read_config() -> Value {
    crate::config::read_config()
}

fn is_binary_on_path(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn is_process_running(pattern: &str) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

// ── /api/v1/diagnostics ──────────────────────────────────────────────

/// GET /api/v1/diagnostics?project_dir=X — health checks that mirror the
/// TUI `immorterm doctor` command. Each check has `{name, status, detail}`
/// where status is "ok" | "warn" | "error".
pub async fn diagnostics(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let project_dir = q.get("project_dir").cloned().unwrap_or_default();
    let mut checks: Vec<Value> = Vec::new();

    // C binary
    let has_c = is_binary_on_path("immorterm");
    checks.push(json!({
        "name": "immorterm binary",
        "status": if has_c { "ok" } else { "warn" },
        "detail": if has_c { "Found in PATH".to_string() } else { "Not in PATH — terminal persistence disabled".to_string() },
    }));

    // immorterm-ai daemon binary (Rust GPU terminal)
    let has_ai = Path::new(&home()).join(".immorterm/bin/immorterm-ai").exists();
    checks.push(json!({
        "name": "immorterm-ai daemon",
        "status": if has_ai { "ok" } else { "warn" },
        "detail": if has_ai { "~/.immorterm/bin/immorterm-ai".to_string() } else { "Not installed — GPU terminal unavailable".to_string() },
    }));

    // Hub (self)
    checks.push(json!({
        "name": "immorterm-hub",
        "status": "ok",
        "detail": format!("Running on port {} (self)", crate::config::read_state_port().unwrap_or(1440)),
    }));

    // Memory service
    let memory_url = crate::config::discover_memory_url();
    let memory_ok = memory_url.is_some();
    checks.push(json!({
        "name": "Memory service",
        "status": if memory_ok { "ok" } else { "warn" },
        "detail": memory_url.clone().unwrap_or_else(|| "Not reachable — persistent AI memory offline".to_string()),
    }));

    // MCP Gateway
    let gateway_running = is_process_running("mcp-gateway");
    checks.push(json!({
        "name": "MCP Gateway",
        "status": if gateway_running { "ok" } else { "warn" },
        "detail": if gateway_running { "Running".to_string() } else { "Not running — MCP tools proxied per-session".to_string() },
    }));

    // Project dir reachable
    let pd_ok = !project_dir.is_empty() && Path::new(&project_dir).exists();
    checks.push(json!({
        "name": "Project directory",
        "status": if pd_ok { "ok" } else { "warn" },
        "detail": if project_dir.is_empty() { "Not supplied".to_string() } else if pd_ok { project_dir.clone() } else { format!("Missing: {}", project_dir) },
    }));

    Json(json!({ "checks": checks }))
}

// ── /api/v1/services ─────────────────────────────────────────────────

/// GET /api/v1/services — list the services the shell can start/stop, with
/// enabled flag (read from ~/.immorterm/config.json) and runtime status.
pub async fn services() -> Json<Value> {
    let cfg = read_config();
    let services_cfg = cfg
        .get("defaults")
        .and_then(|d| d.get("services"))
        .cloned()
        .unwrap_or(json!({}));

    let memory_enabled = services_cfg
        .get("memory")
        .and_then(|m| m.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let gateway_enabled = services_cfg
        .get("mcpGateway")
        .and_then(|m| m.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let memory_running = crate::config::discover_memory_url().is_some();
    let gateway_running = is_process_running("mcp-gateway");

    let services = vec![
        json!({
            "id": "memory",
            "name": "ImmorTerm Memory",
            "desc": "Persistent AI memory — remembers decisions, context, and lessons across sessions",
            "enabled": memory_enabled,
            "running": memory_running,
            "proOnly": false,
            "canStartStop": true,
            "graphEnabled": false,
        }),
        json!({
            "id": "gateway",
            "name": "MCP Gateway",
            "desc": "Shared MCP server proxy — reduces memory ~90% by deduplicating tool processes",
            "enabled": gateway_enabled,
            "running": gateway_running,
            "proOnly": false,
            "canStartStop": true,
            "graphEnabled": false,
        }),
    ];

    Json(json!({ "services": services }))
}

// ── /api/v1/logs ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LogsQuery {
    pub project_dir: Option<String>,
}

/// GET /api/v1/logs?project_dir=X — enumerate terminal log files.
/// Scans `<project_dir>/.immorterm/terminals/logs/` for `*.grid.jsonl` /
/// `*.ai.jsonl` / `*.log` and groups by session name.
pub async fn logs(Query(q): Query<LogsQuery>) -> Json<Value> {
    let pd = q.project_dir.unwrap_or_default();
    if pd.is_empty() {
        return Json(json!({ "sessions": [] }));
    }
    let dir = Path::new(&pd).join(".immorterm/terminals/logs");
    let mut by_session: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let (session, kind) = if let Some(s) = name.strip_suffix(".grid.jsonl") {
                (s.to_string(), "grid")
            } else if let Some(s) = name.strip_suffix(".ai.jsonl") {
                (s.to_string(), "ai")
            } else if let Some(s) = name.strip_suffix(".cast") {
                (s.to_string(), "cast")
            } else if let Some(s) = name.strip_suffix(".log") {
                (s.to_string(), "raw")
            } else {
                continue;
            };
            by_session.entry(session).or_default().push(kind.into());
        }
    }
    let sessions: Vec<Value> = by_session
        .into_iter()
        .map(|(name, types)| json!({ "name": name, "types": types }))
        .collect();
    Json(json!({ "sessions": sessions }))
}

// ── /api/v1/license ──────────────────────────────────────────────────

/// GET /api/v1/license — report license status from ~/.immorterm/config.json.
pub async fn license() -> Json<Value> {
    let cfg = read_config();
    let license = cfg.get("license").cloned().unwrap_or(json!({}));
    let status = license.get("status").and_then(|v| v.as_str()).unwrap_or("free");
    Json(json!({
        "status": status,
        "tier": if status == "active" { "pro" } else { "free" },
        "customerEmail": license.get("customerEmail"),
        "expiresAt": license.get("expiresAt"),
    }))
}

// ── /api/v1/stats/insights ───────────────────────────────────────────

/// GET /api/v1/stats/insights?immorterm_id=X — insights availability check.
/// Real proxy to the memory service lives in a future PR (requires reqwest
/// as a dep). For now the modal renders "Memory service required" when the
/// memory service isn't running and hints at /memory URL otherwise.
pub async fn insights(Query(_q): Query<HashMap<String, String>>) -> Json<Value> {
    let memory_url = crate::config::discover_memory_url();
    Json(json!({
        "available": memory_url.is_some(),
        "memory_url": memory_url,
        "reason": if memory_url.is_some() {
            "Open the memory dashboard directly — proxy not yet wired"
        } else {
            "Memory service not reachable"
        },
        "stats": {},
    }))
}
