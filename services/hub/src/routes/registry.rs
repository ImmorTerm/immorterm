//! Registry API — manages ~/.immorterm/registry.json for all deployment targets.
//!
//! Enables the browser-based GPU terminal, VS Code webview, and future clients
//! to list, spawn, close, shelve, reattach, and rename daemon sessions.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{info, warn};

// ── Helpers ──────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn registry_path() -> PathBuf {
    home_dir().join(".immorterm/registry.json")
}

fn socket_dir() -> PathBuf {
    home_dir().join(".immorterm/sockets")
}

/// Read and parse the full registry.
fn load_registry() -> Result<Value, String> {
    let path = registry_path();
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read registry.json: {}", e))?;
    serde_json::from_str(&data)
        .map_err(|e| format!("Failed to parse registry.json: {}", e))
}

// ── session-status.json helpers ─────────────────────────────────
//
// Single source of truth: the hub owns `~/.immorterm/session-status.json`.
// All mutations land through these helpers (atomic tmp+rename). The VS Code
// extension and Tauri webview reach this file only through hub HTTP routes
// — never by direct write. See docs/issues/2026-05-18-session-disappearance.md
// for the incident that motivated this contract.

fn session_status_path() -> PathBuf {
    home_dir().join(".immorterm").join("session-status.json")
}

/// Load session-status.json or return an empty `{"active": {}, "sessions": {}}`.
fn load_session_status() -> Value {
    let path = session_status_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "active": {}, "sessions": {} }))
}

/// Atomic write of session-status.json (tmp + rename). The shrinkage guard
/// for this file is intentionally lighter than the registry.json one — the
/// extension routinely clears the sessions object on certain transitions
/// (sweep-orphans, migration-finished-empty), and forbidding shrinkage would
/// reject legitimate writes. We still tmp+rename so partial-write corruption
/// is impossible.
fn save_session_status_atomic(root: &Value) -> Result<(), String> {
    let path = session_status_path();
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(root)
        .map_err(|e| format!("session-status serialize: {}", e))?;
    std::fs::write(&tmp, json)
        .map_err(|e| format!("session-status tmp write: {}", e))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("session-status rename: {}", e))?;
    Ok(())
}

/// Atomic write of registry.json with a shrinkage guard.
///
/// Compares the new session count against what is currently on disk and refuses
/// the write if it would drop the count by more than 20% (when the disk has
/// >5 sessions). Mirrors the prune safety guard in the daemon's `Registry::prune`
/// > at apps/immorterm-ai/immorterm-daemon/src/registry.rs:547.
///
/// Why: on 2026-05-18 a non-atomic `fs::write` race + stale-cache writer
/// silently clobbered 20 sessions in registry.json. The digest watcher saw
/// "trailing characters at line 1552" mid-write, then a subsequent writer with
/// a 53-session in-memory view cleanly overwrote the 73-session truth. This
/// guard makes that class of clobber loud + recoverable.
fn save_registry_atomic(new_root: &Value) -> Result<(), String> {
    let path = registry_path();

    let new_count = new_root.get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    if let Ok(disk_data) = std::fs::read_to_string(&path)
        && let Ok(disk_root) = serde_json::from_str::<Value>(&disk_data)
    {
        let disk_count = disk_root.get("sessions")
            .and_then(|s| s.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if disk_count > 5 && new_count * 5 < disk_count * 4 {
            let msg = format!(
                "save_registry_atomic: refusing to shrink registry.json from {} \u{2192} {} sessions (>20% drop); likely stale-cache writer",
                disk_count, new_count
            );
            warn!("{}", msg);
            return Err(msg);
        }
    }

    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(new_root)
        .map_err(|e| format!("Failed to serialize registry: {}", e))?;
    std::fs::write(&tmp, json)
        .map_err(|e| format!("Failed to write registry tmp: {}", e))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("Failed to rename registry tmp into place: {}", e))?;
    Ok(())
}

/// Check if a process is alive via kill(pid, 0).
pub(crate) fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

// ── batch sync (port of registry-client.ts::batchSyncClaudeState) ─────────

/// Per-window claude-state snapshot. Mirrors `ClaudeSyncUpdate` in
/// registry-client.ts.
///
/// Phase A note: `tool` was added so non-Claude vendors (Cursor, Codex,
/// Cline, Windsurf, opencode, Aider, Gemini) can self-announce via
/// `POST /api/v1/registry/session-link`. The struct keeps its
/// `ClaudeSyncUpdate` name for now — the rename to `AiSyncUpdate` is
/// Phase B work that touches every call site. When `tool` is `None`, the
/// underlying registry entry is not updated (i.e. legacy callers that
/// don't yet pass a tool stay back-compat). Reads default missing entries
/// to `claude-code` so existing rows look unchanged to clients.
#[derive(Debug, Clone)]
pub struct ClaudeSyncUpdate {
    pub window_id: String,
    pub active: bool,
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub stats: Option<ClaudeStatsSnapshot>,
    /// Identifier for the AI tool driving this window. Phase A values:
    /// `claude-code`, `cursor`, `codex`, `windsurf`, `cline`, `opencode`,
    /// `gemini`, `aider`, `copilot`. `None` means caller didn't supply a
    /// tool — leave the existing field on disk untouched.
    pub tool: Option<String>,
}

/// Default tool identifier used when an entry on disk has no `tool` field
/// (i.e. it was written before Phase A). DRY: shared by reads and the
/// session-link handler.
pub(crate) const DEFAULT_TOOL: &str = "claude-code";

#[derive(Debug, Clone)]
pub struct ClaudeStatsSnapshot {
    pub pid: u32,
    pub rss_kb: u64,
    pub cpu_percent: f64,
    pub start_time: u64,
    pub runtime_secs: u64,
}

/// Port of batchSyncClaudeState — merge all claude state updates plus the
/// same inline dedup pass (one session_id, one entry, newest wins) in a
/// single read-modify-write.
pub fn batch_sync_claude_state(updates: &[ClaudeSyncUpdate]) -> Result<(), String> {
    let mut registry = load_registry()?;
    let mut dirty = false;
    {
        let Some(sessions) = registry.get_mut("sessions").and_then(|s| s.as_array_mut()) else {
            return Err("registry missing sessions[]".into());
        };
        for update in updates {
            let Some(entry) = sessions.iter_mut().find(|s| {
                s.get("window_id").and_then(|v| v.as_str()) == Some(update.window_id.as_str())
            }) else { continue };
            let Some(obj) = entry.as_object_mut() else { continue };
            // Phase A: write `tool` whenever caller supplied one, regardless
            // of active state. Vendor-aware hooks announce themselves before
            // their first session_id is known.
            if let Some(tool) = update.tool.as_deref() {
                if obj.get("tool").and_then(|v| v.as_str()) != Some(tool) {
                    obj.insert("tool".into(), Value::String(tool.to_string()));
                    dirty = true;
                }
            }
            if update.active && update.session_id.is_some() {
                let sid = update.session_id.as_deref().unwrap();
                if obj.get("claude_session_id").and_then(|v| v.as_str()) != Some(sid) {
                    obj.insert("claude_session_id".into(), Value::String(sid.to_string()));
                    dirty = true;
                }
                if let Some(tp) = update.transcript_path.as_deref() {
                    if obj.get("claude_transcript_path").and_then(|v| v.as_str()) != Some(tp) {
                        obj.insert("claude_transcript_path".into(), Value::String(tp.to_string()));
                        dirty = true;
                    }
                }
                if let Some(stats) = &update.stats {
                    obj.insert(
                        "claude_stats".into(),
                        serde_json::json!({
                            "pid": stats.pid,
                            "rss_kb": stats.rss_kb,
                            "cpu_percent": stats.cpu_percent,
                            "start_time": stats.start_time,
                            "runtime_secs": stats.runtime_secs,
                        }),
                    );
                    dirty = true;
                }
            } else {
                // Claude not currently running. ONLY clear live runtime stats —
                // keep `claude_session_id` and `claude_transcript_path` as
                // historical anchors so `immorterm-ai recall` can `claude
                // --resume <uuid>` on the next spawn. Wiping them here caused
                // the 2026-05-18 "sessions started empty" incident: every 30s
                // tick reset every shelved-but-not-running session to UUID-less
                // and the recall cascade fell through to plain `claude`.
                if obj.remove("claude_stats").is_some() { dirty = true; }
            }
        }

        // Inline dedup: for each claude_session_id group, keep the newest
        // claude_stats.start_time; strip session_id from older entries.
        let mut groups: std::collections::HashMap<String, Vec<(usize, u64)>> =
            std::collections::HashMap::new();
        for (idx, entry) in sessions.iter().enumerate() {
            let Some(sid) = entry.get("claude_session_id").and_then(|v| v.as_str()) else {
                continue;
            };
            let start = entry
                .get("claude_stats")
                .and_then(|s| s.get("start_time"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            groups.entry(sid.to_string()).or_default().push((idx, start));
        }
        for (_sid, mut group) in groups {
            if group.len() <= 1 { continue; }
            group.sort_by_key(|e| std::cmp::Reverse(e.1));
            for (idx, _) in group.into_iter().skip(1) {
                if let Some(obj) = sessions[idx].as_object_mut() {
                    if obj.remove("claude_session_id").is_some() { dirty = true; }
                }
            }
        }
    }

    if dirty {
        save_registry_atomic(&registry)?;
    }
    Ok(())
}

/// Find the immorterm-ai daemon binary (same search order as VS Code extension).
fn find_daemon_binary() -> Option<PathBuf> {
    let home = home_dir();
    let locations = [
        home.join("Development/immorterm/target/release/immorterm-ai"),
        home.join("Development/immorterm/target/debug/immorterm-ai"),
        PathBuf::from("/usr/local/bin/immorterm-ai"),
    ];

    for loc in &locations {
        if loc.exists() {
            return Some(loc.clone());
        }
    }

    // Fallback: `which immorterm-ai`
    std::process::Command::new("which")
        .arg("immorterm-ai")
        .output()
        .ok()
        .and_then(|out| {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if path.is_empty() { None } else { Some(PathBuf::from(path)) }
        })
}

/// Generate a random window ID (same format as extension: 5-digit random + 8-char hex).
pub(crate) fn generate_window_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let random_part = (ts % 100000) as u32;
    let hex_part = format!("{:08x}", (ts / 1000) as u32 ^ random_part);
    format!("{}-{}", random_part, hex_part)
}

/// Find WebSocket port file for a session name, return (port, daemon_pid).
fn find_ws_port(session_name: &str) -> Option<(u16, u32)> {
    let dir = socket_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return None };
    let suffix = format!(".{}.ws", session_name);

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(&suffix) {
            continue;
        }
        let pid_str = name.split('.').next().unwrap_or("");
        let pid: u32 = pid_str.parse().ok()?;
        if !is_process_alive(pid) {
            continue;
        }
        let content = std::fs::read_to_string(entry.path()).ok()?;
        let port: u16 = content.trim().parse().ok()?;
        if port > 0 {
            return Some((port, pid));
        }
    }
    None
}

/// Poll for WebSocket port file (100ms intervals, up to timeout_ms).
async fn wait_for_ws_port(session_name: &str, timeout_ms: u64) -> Option<(u16, u32)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if let Some(result) = find_ws_port(session_name) {
            return Some(result);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    None
}

/// Update a field in registry.json for a specific window_id.
/// Locate a session by window_id OR registry name ("speak-mode-ai-12345-abc")
/// and mutate it. Standalone webview tracks `sessionName` which is the
/// registry `name` field, while the Rust daemon keys everything by
/// `window_id`; accept either so callers don't have to pre-resolve.
fn update_registry_entry(id: &str, updater: impl Fn(&mut Value)) -> Result<(), String> {
    let data = std::fs::read_to_string(registry_path())
        .map_err(|e| format!("Failed to read registry: {}", e))?;
    let mut registry: Value = serde_json::from_str(&data)
        .map_err(|e| format!("Failed to parse registry: {}", e))?;

    let sessions = registry.get_mut("sessions")
        .and_then(|s| s.as_array_mut())
        .ok_or("No sessions array in registry")?;

    let entry = sessions.iter_mut()
        .find(|s| {
            s.get("window_id").and_then(|w| w.as_str()) == Some(id)
                || s.get("name").and_then(|w| w.as_str()) == Some(id)
        })
        .ok_or_else(|| format!("No entry with window_id/name={}", id))?;

    updater(entry);

    save_registry_atomic(&registry)
}

/// Request-deserialize helper — accept either `window_id` or `session_name`
/// (the standalone webview sends the latter, the TS registry-client sends
/// the former). Returns the first non-empty one.
#[allow(dead_code)]
fn pick_id(window_id: Option<&str>, session_name: Option<&str>) -> Option<String> {
    window_id.and_then(|s| if s.is_empty() { None } else { Some(s.to_string()) })
        .or_else(|| session_name.and_then(|s| if s.is_empty() { None } else { Some(s.to_string()) }))
}

/// Port of terminal/naming.ts::generateNextName. Scans every entry in
/// registry.json for display_names matching `immorterm-(\d+)` (the unified
/// format the extension uses across every project) and returns the next
/// higher index. DRY: reuses load_registry + the same regex the TS uses.
fn next_auto_display_name() -> String {
    let re = regex::Regex::new(r"^immorterm-(\d+)$").unwrap();
    let mut max_n: u64 = 0;
    if let Ok(reg) = load_registry() {
        if let Some(sessions) = reg.get("sessions").and_then(|s| s.as_array()) {
            for s in sessions {
                let Some(name) = s.get("display_name").and_then(|v| v.as_str()) else { continue };
                if let Some(caps) = re.captures(name) {
                    if let Some(n) = caps.get(1).and_then(|m| m.as_str().parse::<u64>().ok()) {
                        if n > max_n { max_n = n; }
                    }
                }
            }
        }
    }
    format!("immorterm-{}", max_n + 1)
}

/// Clean up stale .ws files for a session name (dead PIDs).
fn cleanup_stale_ws_files(session_name: &str) {
    let dir = socket_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let suffix = format!(".{}.ws", session_name);

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(&suffix) {
            continue;
        }
        let pid_str = name.split('.').next().unwrap_or("");
        if let Ok(pid) = pid_str.parse::<u32>() {
            if !is_process_alive(pid) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Kill a daemon by PID: SIGTERM, then wait up to 5s, then SIGKILL.
async fn kill_daemon(pid: u32) {
    if !is_process_alive(pid) { return; }
    unsafe { libc::kill(pid as i32, libc::SIGTERM); }
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if !is_process_alive(pid) { return; }
    }
    if is_process_alive(pid) {
        unsafe { libc::kill(pid as i32, libc::SIGKILL); }
    }
}

/// Spawn an immorterm-ai daemon with the given parameters.
fn spawn_daemon(
    binary: &PathBuf,
    session_name: &str,
    project_dir: &str,
    window_id: &str,
    display_name: &str,
    shell: &str,
    claude_session_id: Option<&str>,
    title_locked: bool,
) -> Result<(), String> {
    let logs_dir = PathBuf::from(project_dir).join(".immorterm/terminals/logs");
    let _ = std::fs::create_dir_all(&logs_dir);
    let log_path = logs_dir.join(format!("{}.log", session_name));

    let mut cmd = std::process::Command::new(binary);
    cmd.args(["-dmS", session_name, "-s", shell]);
    cmd.args(["-L", "-Logfile", &log_path.to_string_lossy()]);
    cmd.current_dir(project_dir);
    cmd.env("IMMORTERM_SESSION", session_name);
    cmd.env("IMMORTERM_SESSION_TYPE", "ai");
    cmd.env("TERM", "xterm-256color");
    cmd.env("IMMORTERM_WINDOW_ID", window_id);
    cmd.env("IMMORTERM_DISPLAY_NAME", display_name);
    cmd.env("SCREEN_PROJECT_DIR", project_dir);
    if let Some(cid) = claude_session_id {
        cmd.env("IMMORTERM_CLAUDE_SESSION_ID", cid);
    }
    if title_locked {
        cmd.env("IMMORTERM_TITLE_LOCKED", "1");
    }

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    // setsid: detach from parent process group so daemon survives
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let child = cmd.spawn()
        .map_err(|e| format!("Failed to spawn daemon: {}", e))?;

    // Don't wait for the child — it's a daemon
    std::mem::forget(child);
    Ok(())
}

// ── GET /api/v1/registry ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct RegistryQuery {
    #[serde(default)]
    pub project_dir: Option<String>,
}

/// Return the full registry, optionally filtered by project_dir.
/// Also returns unique project dirs for the project picker.
pub async fn get_registry(
    Query(q): Query<RegistryQuery>,
) -> Json<Value> {
    let registry = match load_registry() {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": e })),
    };

    let sessions = registry.get("sessions")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    // Load session-status.json once; merge speak_mode (and session_order) per
    // window_id. Same file path extension reads via sessionStatusMap. DRY —
    // one read, applied to every session below.
    let session_status_path = home_dir().join(".immorterm").join("session-status.json");
    let session_status: Value = std::fs::read_to_string(&session_status_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    let status_by_wid: &serde_json::Map<String, Value> = session_status
        .get("sessions")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| {
            static EMPTY: std::sync::OnceLock<serde_json::Map<String, Value>> = std::sync::OnceLock::new();
            EMPTY.get_or_init(serde_json::Map::new)
        });

    // Enrich each session with ws_port, alive status, and speak_mode from
    // session-status.json.
    let enriched: Vec<Value> = sessions.iter().map(|s| {
        let mut entry = s.clone();
        let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let pid = s.get("pid").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
        let alive = pid > 0 && is_process_alive(pid);
        entry["alive"] = json!(alive);
        // Phase A: back-compat default for legacy entries that pre-date the
        // `tool` field. Default to "claude-code" so sidebar/CTX-bar clients
        // that read this endpoint always see a tool tag.
        if entry.get("tool").and_then(|v| v.as_str()).map(|s| s.is_empty()).unwrap_or(true) {
            entry["tool"] = json!(DEFAULT_TOOL);
        }
        if alive && !name.is_empty() {
            if let Some((port, _)) = find_ws_port(name) {
                entry["ws_port"] = json!(port);
            }
        }
        if let Some(wid) = s.get("window_id").and_then(|v| v.as_str()) {
            if let Some(status) = status_by_wid.get(wid).and_then(|v| v.as_object()) {
                if let Some(sm) = status.get("speak_mode") {
                    entry["speak_mode"] = sm.clone();
                }
                if let Some(so) = status.get("session_order") {
                    entry["session_order"] = so.clone();
                }
            }
        }
        entry
    }).collect();

    // Filter by project_dir if specified (same logic as extension's matchesProject)
    let filtered: Vec<&Value> = if let Some(ref project) = q.project_dir {
        let project_name = std::path::Path::new(project)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        enriched.iter().filter(|s| {
            let pd = s.get("project_dir").and_then(|p| p.as_str()).unwrap_or("");
            pd == project || pd.ends_with(&format!("/{}", project_name))
        }).collect()
    } else {
        enriched.iter().collect()
    };

    // Extract unique project dirs with session counts for the project picker
    // Only count alive sessions for accurate project picker display
    let mut project_map: HashMap<String, (usize, u64)> = HashMap::new();
    for s in &enriched {
        let pd = s.get("project_dir").and_then(|p| p.as_str()).unwrap_or("").to_string();
        if pd.is_empty() { continue; }
        let alive = s.get("alive").and_then(|a| a.as_bool()).unwrap_or(false);
        if !alive { continue; }
        let created = s.get("created_at").and_then(|c| c.as_u64()).unwrap_or(0);
        let entry = project_map.entry(pd).or_insert((0, 0));
        entry.0 += 1;
        if created > entry.1 { entry.1 = created; }
    }

    let projects: Vec<Value> = project_map.into_iter().map(|(dir, (count, last_activity))| {
        let name = std::path::Path::new(&dir)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| dir.clone());
        json!({
            "project_dir": dir,
            "name": name,
            "session_count": count,
            "last_activity": last_activity,
        })
    }).collect();

    Json(json!({
        "sessions": filtered,
        "projects": projects,
    }))
}

// ── POST /api/v1/registry/spawn ──────────────────────────────────

#[derive(Deserialize)]
pub struct SpawnRequest {
    pub project_dir: String,
    pub display_name: Option<String>,
    pub shell: Option<String>,
}

/// Spawn a new immorterm-ai daemon for a project directory.
pub async fn spawn_session(
    Json(req): Json<SpawnRequest>,
) -> Json<Value> {
    let binary = match find_daemon_binary() {
        Some(b) => b,
        None => return Json(json!({ "error": "immorterm-ai binary not found" })),
    };

    let window_id = generate_window_id();
    let project_name = std::path::Path::new(&req.project_dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "terminal".to_string());
    let session_name = format!("{}-ai-{}", project_name, window_id);
    let display_name = req
        .display_name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(next_auto_display_name);
    let shell = req.shell.unwrap_or_else(|| {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
    });

    cleanup_stale_ws_files(&session_name);

    if let Err(e) = spawn_daemon(&binary, &session_name, &req.project_dir, &window_id, &display_name, &shell, None, false) {
        return Json(json!({ "error": e }));
    }

    info!("Spawned daemon {} for {}", session_name, req.project_dir);

    match wait_for_ws_port(&session_name, 8000).await {
        Some((port, daemon_pid)) => {
            info!("Daemon {} ready on ws://127.0.0.1:{}", session_name, port);
            Json(json!({
                "session_name": session_name,
                "window_id": window_id,
                "display_name": display_name,
                "ws_port": port,
                "pid": daemon_pid,
            }))
        }
        None => {
            warn!("Timeout waiting for WebSocket port for {}", session_name);
            Json(json!({
                "error": "Timeout waiting for daemon WebSocket port",
                "session_name": session_name,
                "window_id": window_id,
            }))
        }
    }
}

// ── POST /api/v1/registry/close ──────────────────────────────────

#[derive(Deserialize)]
pub struct CloseRequest {
    #[serde(alias = "session_name", alias = "windowId", alias = "sessionName")]
    pub window_id: String,
}

/// Close (kill) a daemon session.
pub async fn close_session(
    Json(req): Json<CloseRequest>,
) -> Json<Value> {
    let registry = match load_registry() {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": e })),
    };

    let sessions = registry.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    let entry = sessions.iter().find(|s| {
        s.get("window_id").and_then(|w| w.as_str()) == Some(&req.window_id)
            || s.get("name").and_then(|w| w.as_str()) == Some(&req.window_id)
    });

    let Some(entry) = entry else {
        return Json(json!({ "error": format!("No session with window_id/name={}", req.window_id) }));
    };

    let pid = entry.get("pid").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
    if pid == 0 {
        return Json(json!({ "error": "No PID in registry entry" }));
    }

    kill_daemon(pid).await;

    let session_name = entry.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if !session_name.is_empty() {
        cleanup_stale_ws_files(session_name);
    }

    info!("Closed session {} (PID {})", req.window_id, pid);
    Json(json!({ "success": true }))
}

// ── POST /api/v1/registry/shelve ─────────────────────────────────

#[derive(Deserialize)]
pub struct ShelveRequest {
    #[serde(alias = "session_name", alias = "windowId", alias = "sessionName")]
    pub window_id: String,
}

/// Shelve a session: kill daemon, mark as shelved in registry.
pub async fn shelve_session(
    Json(req): Json<ShelveRequest>,
) -> Json<Value> {
    let registry = match load_registry() {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": e })),
    };

    let sessions = registry.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    let entry = sessions.iter().find(|s| {
        s.get("window_id").and_then(|w| w.as_str()) == Some(&req.window_id)
    });

    let Some(entry) = entry else {
        return Json(json!({ "error": format!("No session with window_id={}", req.window_id) }));
    };

    let claude_session_id = entry.get("claude_session_id")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    let pid = entry.get("pid").and_then(|p| p.as_u64()).unwrap_or(0) as u32;

    if pid > 0 { kill_daemon(pid).await; }

    let session_name = entry.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
    if !session_name.is_empty() {
        cleanup_stale_ws_files(&session_name);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let claude_id_clone = claude_session_id.clone();
    if let Err(e) = update_registry_entry(&req.window_id, |entry| {
        entry["session_status"] = json!("shelved");
        entry["shelved_at"] = json!(now);
        if let Some(ref cid) = claude_id_clone {
            entry["claude_session_id"] = json!(cid);
        }
    }) {
        warn!("Failed to update registry: {}", e);
    }

    info!("Shelved session {} (PID {})", req.window_id, pid);
    Json(json!({
        "success": true,
        "claude_session_id": claude_session_id,
    }))
}

// ── POST /api/v1/registry/reattach ───────────────────────────────

#[derive(Deserialize)]
pub struct ReattachRequest {
    pub window_id: String,
}

/// Reattach a shelved session: spawn fresh daemon with original config.
pub async fn reattach_session(
    Json(req): Json<ReattachRequest>,
) -> Json<Value> {
    let registry = match load_registry() {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": e })),
    };

    let sessions = registry.get("sessions").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    let entry = sessions.iter().find(|s| {
        s.get("window_id").and_then(|w| w.as_str()) == Some(&req.window_id)
    });

    let Some(entry) = entry else {
        return Json(json!({ "error": format!("No shelved session with window_id={}", req.window_id) }));
    };

    let project_dir = entry.get("project_dir").and_then(|p| p.as_str()).unwrap_or("").to_string();
    let display_name = entry.get("display_name").and_then(|d| d.as_str()).unwrap_or("immorterm").to_string();
    let claude_session_id = entry.get("claude_session_id").and_then(|c| c.as_str()).map(|s| s.to_string());
    let title_locked = entry.get("title_locked").and_then(|t| t.as_bool()).unwrap_or(false);
    let session_name = entry.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();

    if project_dir.is_empty() || session_name.is_empty() {
        return Json(json!({ "error": "Shelved entry missing project_dir or name" }));
    }

    let binary = match find_daemon_binary() {
        Some(b) => b,
        None => return Json(json!({ "error": "immorterm-ai binary not found" })),
    };

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

    cleanup_stale_ws_files(&session_name);

    if let Err(e) = spawn_daemon(
        &binary, &session_name, &project_dir, &req.window_id,
        &display_name, &shell, claude_session_id.as_deref(), title_locked,
    ) {
        return Json(json!({ "error": e }));
    }

    match wait_for_ws_port(&session_name, 8000).await {
        Some((port, daemon_pid)) => {
            if let Err(e) = update_registry_entry(&req.window_id, |entry| {
                entry["session_status"] = json!("active");
                entry.as_object_mut().map(|o| o.remove("shelved_at"));
            }) {
                warn!("Failed to update registry after reattach: {}", e);
            }

            info!("Reattached session {} on ws://127.0.0.1:{}", session_name, port);
            Json(json!({
                "session_name": session_name,
                "window_id": req.window_id,
                "display_name": display_name,
                "ws_port": port,
                "pid": daemon_pid,
                "claude_session_id": claude_session_id,
            }))
        }
        None => {
            Json(json!({
                "error": "Timeout waiting for daemon WebSocket port after reattach",
                "session_name": session_name,
            }))
        }
    }
}

// ── POST /api/v1/registry/rename ─────────────────────────────────

#[derive(Deserialize)]
pub struct RenameRequest {
    #[serde(alias = "session_name", alias = "windowId", alias = "sessionName")]
    pub window_id: String,
    #[serde(alias = "displayName")]
    pub display_name: String,
}

/// Rename a session (updates display_name and locks title).
pub async fn rename_session(
    Json(req): Json<RenameRequest>,
) -> Json<Value> {
    let new_name = req.display_name.clone();
    match update_registry_entry(&req.window_id, |entry| {
        entry["display_name"] = json!(new_name);
        entry["title_locked"] = json!(true);
    }) {
        Ok(()) => {
            info!("Renamed session {} to '{}'", req.window_id, req.display_name);
            Json(json!({ "success": true }))
        }
        Err(e) => Json(json!({ "error": e })),
    }
}

#[derive(Deserialize)]
pub struct TitleLockRequest {
    #[serde(alias = "session_name", alias = "windowId", alias = "sessionName")]
    pub window_id: String,
    pub locked: bool,
}

/// Port of registry-client.ts::updateRegistryTitleLocked — bool flag that
/// keeps the sidebar name stable when the terminal title string drifts.
pub async fn set_title_lock(Json(req): Json<TitleLockRequest>) -> Json<Value> {
    let locked = req.locked;
    match update_registry_entry(&req.window_id, |entry| {
        entry["title_locked"] = json!(locked);
    }) {
        Ok(()) => {
            info!("titleLocked={} for {}", locked, req.window_id);
            Json(json!({ "success": true }))
        }
        Err(e) => Json(json!({ "error": e })),
    }
}

#[derive(Deserialize)]
pub struct SpeakModeRequest {
    #[serde(alias = "session_name", alias = "windowId", alias = "sessionName")]
    pub window_id: String,
    /// None / "" / "default" clears the override — matches TS semantics.
    #[serde(default)]
    pub mode: Option<String>,
}

/// Port of registry-client.ts::updateSessionSpeakMode. Writes to
/// ~/.immorterm/session-status.json (NOT registry.json — the Rust daemon
/// strips unknown fields on every refresh). mode=null/""/"default" clears
/// the per-session override so the cascade falls through to project
/// default.
pub async fn set_speak_mode(Json(req): Json<SpeakModeRequest>) -> Json<Value> {
    let mut root = load_session_status();
    let sessions_val = root
        .as_object_mut().map(|m| m.entry("sessions".to_string()).or_insert_with(|| json!({})));
    let Some(sessions) = sessions_val.and_then(|v| v.as_object_mut()) else {
        return Json(json!({ "error": "session-status.sessions is not an object" }));
    };
    let mode = req.mode.as_deref().unwrap_or("").trim().to_string();
    let clear = mode.is_empty() || mode == "default";
    let entry = sessions
        .entry(req.window_id.clone())
        .or_insert_with(|| json!({}));
    if let Some(obj) = entry.as_object_mut() {
        if clear {
            obj.remove("speak_mode");
        } else {
            obj.insert("speak_mode".into(), json!(mode));
        }
    }
    if let Err(e) = save_session_status_atomic(&root) {
        return Json(json!({ "error": e }));
    }
    info!(
        "[speak-mode] window_id={} {} (session-status.json)",
        req.window_id,
        if clear { "cleared".to_string() } else { format!("= {}", mode) }
    );
    Json(json!({ "success": true, "speak_mode": if clear { Value::Null } else { json!(mode) } }))
}

#[derive(Deserialize)]
pub struct ActiveWindowRequest {
    pub window_id: Option<String>,
}

/// Port of SessionManager::setActiveWindowId. The webview fires
/// `session-switched` whenever the user clicks a sidebar row; the hub's
/// SessionManager uses that to decide which terminal's stats to auto-
/// toggle every 30 s. Stored in a process-global OnceLock so any handler
/// can read/write it without threading a reference.
pub async fn set_active_window(Json(req): Json<ActiveWindowRequest>) -> Json<Value> {
    crate::session_manager::set_global_active_window_id(req.window_id.clone());
    Json(json!({ "success": true, "window_id": req.window_id }))
}

#[derive(Deserialize)]
pub struct RegisterProjectRequest {
    pub project_dir: String,
}

/// POST /api/v1/registry/register-project — ensure a SessionManager +
/// claude_tracker loop are running for this project_dir. Idempotent. The
/// multi-tab Tauri frontend calls this on tab open so the hub starts
/// tracking the registry entries that match the project, even if it's
/// different from the hub's initial cwd.
pub async fn register_project(Json(req): Json<RegisterProjectRequest>) -> Json<Value> {
    if req.project_dir.trim().is_empty() {
        return Json(json!({ "error": "missing project_dir" }));
    }
    let _ = crate::session_manager::manager_for(&req.project_dir);
    Json(json!({ "success": true, "project_dir": req.project_dir }))
}

#[derive(Deserialize)]
pub struct ReorderRequest {
    pub window_ids: Vec<String>,
}

/// Port of registry-client.ts::updateRegistrySessionOrder. session_order is
/// stored in <home>/.immorterm/session-status.json under the
/// `sessions.<window_id>.session_order` key — matches saveSessionStatus()
/// in TS exactly (previous version wrote root-level keys, which the
/// sessionStatusMap loader on either side silently dropped).
pub async fn reorder_sessions(Json(req): Json<ReorderRequest>) -> Json<Value> {
    let mut root = load_session_status();
    let sessions_val = root
        .as_object_mut().map(|m| m.entry("sessions".to_string()).or_insert_with(|| json!({})));
    let Some(sessions) = sessions_val.and_then(|v| v.as_object_mut()) else {
        return Json(json!({ "error": "session-status.sessions is not an object" }));
    };
    for (i, wid) in req.window_ids.iter().enumerate() {
        let entry = sessions
            .entry(wid.clone())
            .or_insert_with(|| json!({ "status": "active" }));
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("session_order".into(), json!(i));
        }
    }
    if let Err(e) = save_session_status_atomic(&root) {
        return Json(json!({ "error": e }));
    }
    Json(json!({ "success": true, "count": req.window_ids.len() }))
}

// ── POST /api/v1/registry/session-status — generic set ─────────────
//
// Replaces direct extension writes to session-status.json. Caller supplies
// `window_id` plus any subset of {status, shelved_at, claude_resume_id,
// claude_explicitly_exited, session_order, speak_mode}. Unspecified fields
// are left as-is on the existing entry.
//
// `status: "active"` REMOVES the entry entirely (per the TS semantics — absence
// means default-active). `status: "shelved"` replaces the entry's status/
// timestamp/resume-id fields (cleared first to drop stale fields).
#[derive(Deserialize)]
pub struct SessionStatusSetRequest {
    pub window_id: String,
    #[serde(default)] pub status: Option<String>,
    #[serde(default)] pub shelved_at: Option<u64>,
    #[serde(default)] pub claude_resume_id: Option<String>,
    #[serde(default)] pub claude_explicitly_exited: Option<bool>,
    #[serde(default)] pub session_order: Option<u64>,
    #[serde(default)] pub speak_mode: Option<String>,
}

pub async fn session_status_set(Json(req): Json<SessionStatusSetRequest>) -> Json<Value> {
    if req.window_id.trim().is_empty() {
        return Json(json!({ "error": "missing window_id" }));
    }
    let mut root = load_session_status();
    let sessions_val = root
        .as_object_mut().map(|m| m.entry("sessions".to_string()).or_insert_with(|| json!({})));
    let Some(sessions) = sessions_val.and_then(|v| v.as_object_mut()) else {
        return Json(json!({ "error": "session-status.sessions is not an object" }));
    };

    // status: "active" → remove entry entirely (absence = default active)
    if req.status.as_deref() == Some("active") {
        sessions.remove(&req.window_id);
        if let Err(e) = save_session_status_atomic(&root) {
            return Json(json!({ "error": e }));
        }
        return Json(json!({ "success": true, "removed": true }));
    }

    let entry = sessions
        .entry(req.window_id.clone())
        .or_insert_with(|| json!({}));
    let Some(obj) = entry.as_object_mut() else {
        return Json(json!({ "error": "entry corrupt" }));
    };

    if let Some(status) = req.status.as_deref() {
        if status == "shelved" {
            // Replace shelve-related fields atomically (drop stale).
            obj.insert("status".into(), json!("shelved"));
            obj.insert(
                "shelved_at".into(),
                req.shelved_at.map(|v| json!(v)).unwrap_or(Value::Null),
            );
            if let Some(rid) = req.claude_resume_id.as_deref() {
                obj.insert("claude_resume_id".into(), json!(rid));
            }
            if req.claude_explicitly_exited == Some(true) {
                obj.insert("claude_explicitly_exited".into(), json!(true));
            } else {
                obj.remove("claude_explicitly_exited");
            }
        } else {
            obj.insert("status".into(), json!(status));
        }
    }
    if let Some(order) = req.session_order {
        obj.insert("session_order".into(), json!(order));
    }
    if let Some(speak) = req.speak_mode.as_deref() {
        let trimmed = speak.trim();
        if trimmed.is_empty() || trimmed == "default" {
            obj.remove("speak_mode");
        } else {
            obj.insert("speak_mode".into(), json!(trimmed));
        }
    }

    if let Err(e) = save_session_status_atomic(&root) {
        return Json(json!({ "error": e }));
    }
    Json(json!({ "success": true }))
}

// ── POST /api/v1/registry/session-status/remove ────────────────────
//
// Drops a session-status entry entirely. Matches removeSessionStatus() in
// the extension (used by reconciler when a session's directory is gone on
// disk — also clears speak_mode / session_order / claude_resume_id).
#[derive(Deserialize)]
pub struct SessionStatusRemoveRequest {
    pub window_id: String,
}

pub async fn session_status_remove(Json(req): Json<SessionStatusRemoveRequest>) -> Json<Value> {
    if req.window_id.trim().is_empty() {
        return Json(json!({ "error": "missing window_id" }));
    }
    let mut root = load_session_status();
    let Some(sessions) = root
        .as_object_mut()
        .and_then(|m| m.get_mut("sessions"))
        .and_then(|v| v.as_object_mut())
    else {
        return Json(json!({ "success": true, "removed": false }));
    };
    let removed = sessions.remove(&req.window_id).is_some();
    if removed {
        if let Err(e) = save_session_status_atomic(&root) {
            return Json(json!({ "error": e }));
        }
    }
    Json(json!({ "success": true, "removed": removed }))
}

// ── POST /api/v1/registry/active-terminal ──────────────────────────
//
// Sets the top-level `active.<type>` field in session-status.json. The
// extension uses this to remember which terminal was last focused per
// `type` ('regular' | 'ai'). Replaces saveSessionStatus() debounced call
// from setActiveTerminal().
#[derive(Deserialize)]
pub struct ActiveTerminalRequest {
    /// "regular" or "ai"
    pub terminal_type: String,
    /// Pass `None` (omit) to clear.
    #[serde(default)]
    pub window_id: Option<String>,
}

pub async fn set_active_terminal(Json(req): Json<ActiveTerminalRequest>) -> Json<Value> {
    let ty = req.terminal_type.trim();
    if ty != "regular" && ty != "ai" {
        return Json(json!({ "error": "terminal_type must be 'regular' or 'ai'" }));
    }
    let mut root = load_session_status();
    let active_val = root
        .as_object_mut().map(|m| m.entry("active".to_string()).or_insert_with(|| json!({})));
    let Some(active) = active_val.and_then(|v| v.as_object_mut()) else {
        return Json(json!({ "error": "session-status.active is not an object" }));
    };
    match req.window_id.as_deref() {
        Some(wid) if !wid.is_empty() => { active.insert(ty.to_string(), json!(wid)); }
        _ => { active.remove(ty); }
    }
    if let Err(e) = save_session_status_atomic(&root) {
        return Json(json!({ "error": e }));
    }
    Json(json!({ "success": true }))
}

// ── POST /api/v1/registry/session-link ───────────────────────────
//
// Hooks self-announce a session at start. Phase A — same endpoint serves
// every vendor (claude-code today, cursor/codex/cline/windsurf/opencode/
// gemini/aider/copilot as their hook scripts come online). The hook supplies the
// natural key (`window_id`) plus the freshly-resolved per-tool fields:
// `tool`, `session_id`, `transcript_path`, plus `pid`/`cwd` for diagnostics.
//
// The lookup-and-mutate is split into a pure `apply_session_link` helper
// so it can be unit-tested without touching disk.
//
// IMPORTANT cross-process caveat — the immorterm-ai daemon
// (`apps/immorterm-ai/immorterm-daemon/src/registry.rs`) deserialises
// registry.json into a strict `RegistryEntry` struct that does NOT yet
// include a `tool` field, so any field this endpoint writes will be
// dropped the next time a daemon flushes the registry (every register/
// deregister, ~hourly housekeeping). Adding `tool: Option<String>` with
// `#[serde(default)]` to `RegistryEntry` is queued as a follow-up — see
// Phase A Task 9 in internal design notes. Until then this endpoint is
// useful for in-process consumers (the hub itself, tests) and any reader
// querying within ~seconds of the call.

/// Allowlist of valid `tool` values. Any caller-supplied value outside this
/// set is rejected with HTTP 200 + `{"updated": false, "reason": ...}` —
/// matches the contract the planning doc spells out.
const VALID_TOOLS: &[&str] = &[
    "claude-code",
    "cursor",
    "codex",
    "windsurf",
    "cline",
    "opencode",
    "gemini",
    "aider",
    "copilot",
];

#[derive(Debug, Deserialize)]
pub struct SessionLinkRequest {
    pub window_id: String,
    pub tool: String,
    pub session_id: String,
    pub transcript_path: String,
    /// Optional — the hook script may not always know the AI tool's PID
    /// (Cline, opencode plugin) at the moment of the link call.
    #[serde(default)]
    pub pid: Option<u64>,
    /// Optional — kept for diagnostics / future routing. Not persisted to
    /// disk here (the existing `project_dir` is the authoritative path).
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Pure mutator: locate the entry by `window_id` in `registry.sessions[]`,
/// stamp the four fields, return whether anything was updated. No I/O.
///
/// Returns `Ok(())` on success, `Err(reason)` when the window_id isn't
/// present in the registry. Caller is responsible for serializing the
/// `Err` reason into the API response.
fn apply_session_link(
    registry: &mut Value,
    window_id: &str,
    tool: &str,
    session_id: &str,
    transcript_path: &str,
    pid: Option<u64>,
) -> Result<(), String> {
    let sessions = registry
        .get_mut("sessions")
        .and_then(|s| s.as_array_mut())
        .ok_or_else(|| "registry missing sessions[]".to_string())?;
    let entry = sessions
        .iter_mut()
        .find(|s| s.get("window_id").and_then(|v| v.as_str()) == Some(window_id))
        .ok_or_else(|| "window_id not found".to_string())?;
    let obj = entry
        .as_object_mut()
        .ok_or_else(|| "registry entry is not an object".to_string())?;

    obj.insert("tool".into(), Value::String(tool.to_string()));
    // The on-disk schema still uses `claude_session_id` /
    // `claude_transcript_path` as the field names — Phase B renames those
    // to `ai_session_id` / `ai_transcript_path`. Keep the existing keys so
    // the rest of the codebase (sidebar, CTX bar, claude_tracker) keeps
    // working without churn.
    obj.insert(
        "claude_session_id".into(),
        Value::String(session_id.to_string()),
    );
    obj.insert(
        "claude_transcript_path".into(),
        Value::String(transcript_path.to_string()),
    );

    // Append-only tool_history. Captures the timeline of which AI tool +
    // session_id was attached to this window and when. Lets us reconstruct
    // "this immorterm window was Claude 10:00\u201311:30, Codex 11:30\u201312:00"
    // even though the top-level `tool` field gets overwritten on each
    // session-link. Dedupe consecutive identical entries so a digest-
    // daemon polling tick that reannounces the same session doesn't
    // bloat the history.
    let now_iso = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let new_entry = json!({
        "tool": tool,
        "session_id": session_id,
        "transcript_path": transcript_path,
        "ts": now_iso,
    });
    let history = obj
        .entry("tool_history")
        .or_insert_with(|| Value::Array(vec![]));
    if let Some(arr) = history.as_array_mut() {
        let last_matches = arr.last().map(|prev| {
            prev.get("tool").and_then(|v| v.as_str()) == Some(tool)
                && prev.get("session_id").and_then(|v| v.as_str()) == Some(session_id)
        });
        if last_matches != Some(true) {
            arr.push(new_entry);
        }
    }

    if let Some(p) = pid {
        // Stash the AI tool's pid alongside the snapshot fields the
        // claude_tracker writes. Doesn't conflict with the registry-level
        // `pid` (that one is the immorterm-ai daemon's pid).
        obj.entry("claude_stats")
            .or_insert_with(|| json!({}));
        if let Some(stats) = obj.get_mut("claude_stats").and_then(|v| v.as_object_mut()) {
            stats.insert("pid".into(), json!(p));
        }
    }
    Ok(())
}

pub async fn session_link(Json(req): Json<SessionLinkRequest>) -> Json<Value> {
    if req.window_id.trim().is_empty() {
        return Json(json!({ "updated": false, "reason": "missing window_id" }));
    }
    if !VALID_TOOLS.contains(&req.tool.as_str()) {
        return Json(json!({
            "updated": false,
            "reason": format!("unknown tool '{}'", req.tool),
        }));
    }

    let path = registry_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            return Json(json!({
                "updated": false,
                "reason": format!("read registry: {}", e),
            }));
        }
    };
    let mut registry: Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(e) => {
            return Json(json!({
                "updated": false,
                "reason": format!("parse registry: {}", e),
            }));
        }
    };

    if let Err(reason) = apply_session_link(
        &mut registry,
        &req.window_id,
        &req.tool,
        &req.session_id,
        &req.transcript_path,
        req.pid,
    ) {
        return Json(json!({
            "updated": false,
            "reason": reason,
            "window_id": req.window_id,
        }));
    }

    if let Err(reason) = save_registry_atomic(&registry) {
        return Json(json!({
            "updated": false,
            "reason": reason,
        }));
    }

    let _ = path;
    info!(
        "[session-link] window_id={} tool={} session_id={} cwd={:?}",
        req.window_id, req.tool, req.session_id, req.cwd
    );
    Json(json!({
        "updated": true,
        "window_id": req.window_id,
        "tool": req.tool,
    }))
}

// ─── GET /api/v1/registry/window/{window_id} ───────────────────────
// Per internal design notes §3.1.
// Closes the silent-fail curl at
// `.immorterm/hooks/immorterm-memory-digest.sh:369` — that path has been
// hitting a non-existent endpoint since Phase A T9; with no reader the
// bash extractor falls back to `claude-code` for every vendor.

#[derive(Debug, PartialEq, Eq)]
pub enum WindowLookupError {
    NotFound,
    ParseFailure,
}

/// Lightweight ETag using stdlib's DefaultHasher. Stable within a single
/// process; not cryptographic — purpose is poll-with-If-None-Match
/// cache-busting, not authentication.
fn compute_etag(value: &Value) -> String {
    let mut h = DefaultHasher::new();
    value.to_string().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Pure projection of a registry entry into the WindowView shape from
/// v4 §3.1. Returns NotFound if the window_id is absent, ParseFailure
/// if `sessions[]` is missing/non-array. Caller maps to HTTP status.
///
/// v4-additions not yet populated on disk (`host_id`,
/// `claude_stats.comm`) project as `null` — daemon populates them via
/// `apply_session_link` once it ships.
pub fn lookup_window(registry: &Value, window_id: &str) -> Result<Value, WindowLookupError> {
    let sessions = registry
        .get("sessions")
        .and_then(|s| s.as_array())
        .ok_or(WindowLookupError::ParseFailure)?;

    let entry = sessions
        .iter()
        .find(|s| s.get("window_id").and_then(|v| v.as_str()) == Some(window_id))
        .ok_or(WindowLookupError::NotFound)?;

    let pid = entry
        .get("claude_stats")
        .and_then(|s| s.get("pid"))
        .and_then(|p| p.as_u64())
        .unwrap_or(0) as u32;
    let ai_alive = pid > 0 && is_process_alive(pid);

    let tool = entry
        .get("tool")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let claude_stats = entry
        .get("claude_stats")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let mut view = json!({
        "window_id":         window_id,
        "host_id":           entry.get("host_id").cloned().unwrap_or(Value::Null),
        "project_dir":       entry.get("project_dir").cloned().unwrap_or(Value::Null),
        "tool":              tool,
        "vendor_session_id": entry.get("claude_session_id").cloned().unwrap_or(Value::Null),
        "transcript_path":   entry.get("claude_transcript_path").cloned().unwrap_or(Value::Null),
        "tool_history":      entry.get("tool_history").cloned().unwrap_or_else(|| json!([])),
        "claude_stats":      claude_stats,
        "ai_alive":          ai_alive,
    });
    let etag = compute_etag(&view);
    view["etag"] = json!(etag);
    Ok(view)
}

pub async fn get_window(Path(window_id): Path<String>) -> (StatusCode, Json<Value>) {
    let registry = match load_registry() {
        Ok(r) => r,
        Err(e) => {
            warn!("[/registry/window/{}] parse failure: {}", window_id, e);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "registry parse failure" })),
            );
        }
    };
    match lookup_window(&registry, &window_id) {
        Ok(view) => (StatusCode::OK, Json(view)),
        Err(WindowLookupError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown window_id", "window_id": window_id })),
        ),
        Err(WindowLookupError::ParseFailure) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "registry parse failure" })),
        ),
    }
}

// ─── GET /api/v1/registry/by-transcript?path=... ────────────────────
// Per v4 §3.3. Daemon's notify-watcher fires on transcript paths not
// yet in its by_transcript index (vendor-swap race). Daemon synchronously
// queries this endpoint to resolve transcript_path → (window_id,
// vendor_session_id, host_id, tool, project_dir).

#[derive(Debug, Deserialize)]
pub struct ByTranscriptQuery {
    pub path: String,
}

pub fn lookup_by_transcript(registry: &Value, path: &str) -> Result<Value, WindowLookupError> {
    let sessions = registry
        .get("sessions")
        .and_then(|s| s.as_array())
        .ok_or(WindowLookupError::ParseFailure)?;

    // Check tool_history rows first (most recent first within each entry),
    // then fall back to top-level claude_transcript_path. tool_history is
    // the precedence source per v4 §4.2.
    for entry in sessions {
        let window_id = entry.get("window_id").and_then(|v| v.as_str()).unwrap_or("");
        if window_id.is_empty() {
            continue;
        }
        if let Some(history) = entry.get("tool_history").and_then(|h| h.as_array()) {
            for row in history.iter().rev() {
                if row.get("transcript_path").and_then(|v| v.as_str()) == Some(path) {
                    return Ok(json!({
                        "window_id":         window_id,
                        "vendor_session_id": row.get("session_id").cloned().unwrap_or(Value::Null),
                        "host_id":           entry.get("host_id").cloned().unwrap_or(Value::Null),
                        "tool":              row.get("tool").cloned().unwrap_or(Value::Null),
                        "project_dir":       entry.get("project_dir").cloned().unwrap_or(Value::Null),
                    }));
                }
            }
        }
        // Fallback: legacy top-level field. Only consider if no tool_history
        // claim found above. Match exact path.
        if entry.get("claude_transcript_path").and_then(|v| v.as_str()) == Some(path) {
            let tool = entry.get("tool").cloned().unwrap_or_else(|| json!("claude-code"));
            return Ok(json!({
                "window_id":         window_id,
                "vendor_session_id": entry.get("claude_session_id").cloned().unwrap_or(Value::Null),
                "host_id":           entry.get("host_id").cloned().unwrap_or(Value::Null),
                "tool":              tool,
                "project_dir":       entry.get("project_dir").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    Err(WindowLookupError::NotFound)
}

pub async fn get_by_transcript(Query(q): Query<ByTranscriptQuery>) -> (StatusCode, Json<Value>) {
    let registry = match load_registry() {
        Ok(r) => r,
        Err(e) => {
            warn!("[/registry/by-transcript] parse failure: {}", e);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "registry parse failure" })),
            );
        }
    };
    match lookup_by_transcript(&registry, &q.path) {
        Ok(view) => (StatusCode::OK, Json(view)),
        Err(WindowLookupError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no window claims this transcript", "path": q.path })),
        ),
        Err(WindowLookupError::ParseFailure) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "registry parse failure" })),
        ),
    }
}

// ─── POST /api/v1/registry/session-end ──────────────────────────────
// Per v4 §3.5. Daemon-only writer. Mutates the matching tool_history row
// in-place to add `ended_at` + `exit_reason`. Idempotent on
// (window_id, host_id, vendor_session_id, exit_reason); mismatched
// exit_reasons are tie-broken by first write (analyzer F11).

#[derive(Debug, Deserialize)]
pub struct SessionEndRequest {
    pub window_id: String,
    #[serde(default)]
    pub host_id: Option<String>,
    pub vendor_session_id: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    pub exit_reason: String,
}

/// Pure mutator for unit testing — mirrors `apply_session_link` shape.
/// Returns (updated, reason) per the v4 §3.5 contract.
pub fn apply_session_end(
    registry: &mut Value,
    window_id: &str,
    vendor_session_id: &str,
    exit_reason: &str,
    ended_at_iso: &str,
) -> Result<(bool, String), String> {
    let sessions = registry
        .get_mut("sessions")
        .and_then(|s| s.as_array_mut())
        .ok_or_else(|| "registry has no sessions[] array".to_string())?;

    let entry = sessions
        .iter_mut()
        .find(|s| s.get("window_id").and_then(|v| v.as_str()) == Some(window_id))
        .ok_or_else(|| "window_id not found".to_string())?;

    let history = entry
        .get_mut("tool_history")
        .and_then(|h| h.as_array_mut())
        .ok_or_else(|| "tool_history not initialized".to_string())?;

    let row = history
        .iter_mut()
        .rev()
        .find(|r| r.get("session_id").and_then(|v| v.as_str()) == Some(vendor_session_id))
        .ok_or_else(|| "vendor_session_id not in tool_history".to_string())?;

    let existing_reason = row
        .get("exit_reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    match existing_reason {
        Some(existing) if existing == exit_reason => {
            // idempotent — same exit_reason already recorded
            Ok((false, "already ended (exit_reason matches)".to_string()))
        }
        Some(_) => {
            // tie-broken by first write — do NOT overwrite
            Ok((false, "already ended (exit_reason mismatch — first write wins)".to_string()))
        }
        None => {
            let obj = row.as_object_mut().expect("row is object");
            obj.insert("ended_at".into(), Value::String(ended_at_iso.to_string()));
            obj.insert("exit_reason".into(), Value::String(exit_reason.to_string()));
            Ok((true, "appended_history_row".to_string()))
        }
    }
}

pub async fn session_end(Json(req): Json<SessionEndRequest>) -> (StatusCode, Json<Value>) {
    if req.window_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing window_id" })),
        );
    }
    if req.vendor_session_id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing vendor_session_id" })),
        );
    }
    // Validate exit_reason against the allowlist from v4 §3.5.
    const VALID_REASONS: &[&str] = &[
        "idle_timeout",
        "pid_dead",
        "superseded",
        "hook_session_end",
        "hook_stop",
        "size_stable",
    ];
    if !VALID_REASONS.contains(&req.exit_reason.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "unknown exit_reason",
                "exit_reason": req.exit_reason,
                "valid": VALID_REASONS,
            })),
        );
    }

    let path = registry_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": format!("read registry: {}", e) })),
            );
        }
    };
    let mut registry: Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": format!("parse registry: {}", e) })),
            );
        }
    };

    // Hub re-stamps ended_at with receive-time (F5) — request-supplied value
    // is informational only.
    let now_iso = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let result = apply_session_end(
        &mut registry,
        &req.window_id,
        &req.vendor_session_id,
        &req.exit_reason,
        &now_iso,
    );

    match result {
        Ok((true, _reason)) => {
            // Routed through save_registry_atomic for the shrinkage guard +
            // atomic tmp+rename in a single place. The v4 §3 strict invariant
            // around fsync(file) + fsync(parent_dir) is still TODO.
            if let Err(e) = save_registry_atomic(&registry) {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({ "error": e })),
                );
            }
            let _ = path;
            (
                StatusCode::OK,
                Json(json!({
                    "updated": true,
                    "appended_history_row": true,
                    "window_id": req.window_id,
                    "vendor_session_id": req.vendor_session_id,
                    "exit_reason": req.exit_reason,
                    "ended_at": now_iso,
                })),
            )
        }
        Ok((false, reason)) => (
            StatusCode::OK,
            Json(json!({
                "updated": false,
                "reason": reason,
                "window_id": req.window_id,
                "vendor_session_id": req.vendor_session_id,
            })),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e, "window_id": req.window_id })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_registry() -> Value {
        json!({
            "sessions": [
                {
                    "window_id": "11111-aaaa",
                    "name": "demo-ai-11111-aaaa",
                    "project_dir": "/tmp/demo",
                    "pid": 0,
                },
                {
                    "window_id": "22222-bbbb",
                    "name": "demo-ai-22222-bbbb",
                    "project_dir": "/tmp/demo",
                    "pid": 0,
                    "tool": "claude-code",
                }
            ]
        })
    }

    #[test]
    fn session_link_stamps_tool_and_session_fields() {
        let mut reg = sample_registry();
        apply_session_link(
            &mut reg,
            "11111-aaaa",
            "cursor",
            "sess-xyz",
            "/tmp/transcripts/sess-xyz.jsonl",
            Some(4242),
        )
        .expect("should succeed for known window_id");
        let entry = &reg["sessions"][0];
        assert_eq!(entry["tool"], json!("cursor"));
        assert_eq!(entry["claude_session_id"], json!("sess-xyz"));
        assert_eq!(
            entry["claude_transcript_path"],
            json!("/tmp/transcripts/sess-xyz.jsonl")
        );
        assert_eq!(entry["claude_stats"]["pid"], json!(4242));
    }

    #[test]
    fn session_link_rejects_unknown_window_id() {
        let mut reg = sample_registry();
        let err = apply_session_link(
            &mut reg,
            "no-such-window",
            "codex",
            "sess",
            "/tmp/x",
            None,
        )
        .unwrap_err();
        assert_eq!(err, "window_id not found");
    }

    #[test]
    fn session_link_overwrites_existing_tool() {
        // Entry already says claude-code; vendor switch should win.
        let mut reg = sample_registry();
        apply_session_link(
            &mut reg,
            "22222-bbbb",
            "windsurf",
            "sess-2",
            "/tmp/t2.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(reg["sessions"][1]["tool"], json!("windsurf"));
    }

    #[test]
    fn valid_tools_includes_phase_a_vendors() {
        for vendor in [
            "claude-code", "cursor", "codex", "windsurf",
            "cline", "opencode", "gemini", "aider", "copilot",
        ] {
            assert!(VALID_TOOLS.contains(&vendor), "missing {}", vendor);
        }
    }

    #[test]
    fn default_tool_is_claude_code() {
        // Back-compat default surfaced by GET /api/v1/registry must stay
        // claude-code so existing clients see no behaviour change.
        assert_eq!(DEFAULT_TOOL, "claude-code");
    }

    #[test]
    fn session_link_appends_first_history_entry() {
        let mut reg = sample_registry();
        apply_session_link(
            &mut reg,
            "11111-aaaa",
            "cursor",
            "sess-xyz",
            "/tmp/transcripts/sess-xyz.jsonl",
            None,
        )
        .unwrap();
        let history = reg["sessions"][0]["tool_history"]
            .as_array()
            .expect("tool_history should be created");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["tool"], json!("cursor"));
        assert_eq!(history[0]["session_id"], json!("sess-xyz"));
        assert_eq!(
            history[0]["transcript_path"],
            json!("/tmp/transcripts/sess-xyz.jsonl")
        );
        let ts = history[0]["ts"].as_str().expect("ts should be a string");
        assert!(
            ts.ends_with('Z') && ts.contains('T'),
            "ts should be RFC3339 UTC, got {}",
            ts
        );
    }

    #[test]
    fn session_link_dedupes_consecutive_identical_entries() {
        // A digest-daemon polling loop can re-call session-link with the
        // same tool+session_id — we should keep one row, not bloat the list.
        let mut reg = sample_registry();
        for _ in 0..3 {
            apply_session_link(
                &mut reg,
                "11111-aaaa",
                "cursor",
                "sess-xyz",
                "/tmp/transcripts/sess-xyz.jsonl",
                None,
            )
            .unwrap();
        }
        let history = reg["sessions"][0]["tool_history"].as_array().unwrap();
        assert_eq!(history.len(), 1, "consecutive identical links must dedupe");
    }

    #[test]
    fn session_link_appends_when_vendor_changes() {
        // Mid-session vendor switch (e.g. user pivots from Claude Code to
        // Codex in the same immorterm window) — both rows must persist so
        // we can reconstruct the timeline.
        let mut reg = sample_registry();
        apply_session_link(
            &mut reg,
            "22222-bbbb",
            "claude-code",
            "claude-uuid-1",
            "/tmp/cc.jsonl",
            None,
        )
        .unwrap();
        apply_session_link(
            &mut reg,
            "22222-bbbb",
            "codex",
            "codex-sess-1",
            "/tmp/codex.jsonl",
            None,
        )
        .unwrap();
        let history = reg["sessions"][1]["tool_history"].as_array().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["tool"], json!("claude-code"));
        assert_eq!(history[1]["tool"], json!("codex"));
        // Top-level `tool` reflects the latest link; history preserves all.
        assert_eq!(reg["sessions"][1]["tool"], json!("codex"));
    }

    #[test]
    fn session_link_appends_when_session_id_changes_within_same_tool() {
        // Same tool, new session_id → still a new history row. (A user
        // restarting Claude Code in the same window opens a fresh UUID.)
        let mut reg = sample_registry();
        apply_session_link(
            &mut reg,
            "11111-aaaa",
            "claude-code",
            "uuid-1",
            "/tmp/a.jsonl",
            None,
        )
        .unwrap();
        apply_session_link(
            &mut reg,
            "11111-aaaa",
            "claude-code",
            "uuid-2",
            "/tmp/b.jsonl",
            None,
        )
        .unwrap();
        let history = reg["sessions"][0]["tool_history"].as_array().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["session_id"], json!("uuid-1"));
        assert_eq!(history[1]["session_id"], json!("uuid-2"));
    }

    // ── lookup_window tests (v4 §3.1) ──────────────────────────────

    fn sample_with_history() -> Value {
        json!({
            "sessions": [
                {
                    "window_id": "11111-aaaa",
                    "name": "demo-ai-11111-aaaa",
                    "project_dir": "/tmp/demo",
                    "pid": 0,
                    "tool": "claude-code",
                    "claude_session_id": "uuid-current",
                    "claude_transcript_path": "/tmp/transcripts/uuid-current.jsonl",
                    "claude_stats": { "pid": 0, "model": "Sonnet 4" },
                    "tool_history": [
                        {
                            "tool": "claude-code",
                            "session_id": "uuid-old",
                            "transcript_path": "/tmp/transcripts/uuid-old.jsonl",
                            "ts": "2026-05-12T10:00:00Z"
                        },
                        {
                            "tool": "claude-code",
                            "session_id": "uuid-current",
                            "transcript_path": "/tmp/transcripts/uuid-current.jsonl",
                            "ts": "2026-05-12T11:00:00Z"
                        }
                    ]
                },
                {
                    "window_id": "22222-bbbb",
                    "name": "legacy-pre-phase-a",
                    "project_dir": "/tmp/legacy",
                    "pid": 0,
                    // NO tool, NO tool_history → legacy entry
                    "claude_session_id": "uuid-legacy",
                    "claude_transcript_path": "/tmp/transcripts/uuid-legacy.jsonl"
                }
            ]
        })
    }

    #[test]
    fn lookup_window_returns_full_view_for_known_window() {
        let reg = sample_with_history();
        let view = lookup_window(&reg, "11111-aaaa").expect("known window");
        assert_eq!(view["window_id"], json!("11111-aaaa"));
        assert_eq!(view["tool"], json!("claude-code"));
        assert_eq!(view["vendor_session_id"], json!("uuid-current"));
        assert_eq!(view["transcript_path"], json!("/tmp/transcripts/uuid-current.jsonl"));
        assert_eq!(view["project_dir"], json!("/tmp/demo"));
        assert_eq!(view["tool_history"].as_array().unwrap().len(), 2);
        assert_eq!(view["ai_alive"], json!(false), "pid=0 -> not alive");
        assert!(view["etag"].is_string(), "etag must be present");
        // host_id is null today (daemon will populate later)
        assert_eq!(view["host_id"], Value::Null);
    }

    #[test]
    fn lookup_window_returns_legacy_entry_with_nulls() {
        let reg = sample_with_history();
        let view = lookup_window(&reg, "22222-bbbb").expect("legacy window");
        assert_eq!(view["tool"], Value::Null, "legacy has no tool");
        assert_eq!(view["vendor_session_id"], json!("uuid-legacy"));
        assert_eq!(view["tool_history"], json!([]));
    }

    #[test]
    fn lookup_window_returns_not_found_for_unknown_window() {
        let reg = sample_with_history();
        let err = lookup_window(&reg, "no-such-window").unwrap_err();
        assert_eq!(err, WindowLookupError::NotFound);
    }

    #[test]
    fn lookup_window_returns_parse_failure_when_sessions_missing() {
        let reg = json!({});
        let err = lookup_window(&reg, "anything").unwrap_err();
        assert_eq!(err, WindowLookupError::ParseFailure);
    }

    #[test]
    fn lookup_window_etag_is_deterministic() {
        let reg = sample_with_history();
        let v1 = lookup_window(&reg, "11111-aaaa").unwrap();
        let v2 = lookup_window(&reg, "11111-aaaa").unwrap();
        assert_eq!(v1["etag"], v2["etag"]);
    }

    // ── lookup_by_transcript tests (v4 §3.3) ───────────────────────

    #[test]
    fn by_transcript_finds_current_session() {
        let reg = sample_with_history();
        let view = lookup_by_transcript(&reg, "/tmp/transcripts/uuid-current.jsonl")
            .expect("known transcript");
        assert_eq!(view["window_id"], json!("11111-aaaa"));
        assert_eq!(view["vendor_session_id"], json!("uuid-current"));
        assert_eq!(view["tool"], json!("claude-code"));
        assert_eq!(view["project_dir"], json!("/tmp/demo"));
    }

    #[test]
    fn by_transcript_finds_historical_session() {
        let reg = sample_with_history();
        // Should find the OLD session via tool_history scan, not just current
        let view = lookup_by_transcript(&reg, "/tmp/transcripts/uuid-old.jsonl")
            .expect("historical transcript");
        assert_eq!(view["window_id"], json!("11111-aaaa"));
        assert_eq!(view["vendor_session_id"], json!("uuid-old"));
    }

    #[test]
    fn by_transcript_falls_back_to_legacy_top_level() {
        let reg = sample_with_history();
        // Legacy entry has NO tool_history; only claude_transcript_path
        let view = lookup_by_transcript(&reg, "/tmp/transcripts/uuid-legacy.jsonl")
            .expect("legacy transcript");
        assert_eq!(view["window_id"], json!("22222-bbbb"));
        assert_eq!(view["vendor_session_id"], json!("uuid-legacy"));
        // Default tool for legacy = claude-code (per Phase A back-compat)
        assert_eq!(view["tool"], json!("claude-code"));
    }

    #[test]
    fn by_transcript_returns_not_found_for_unknown_path() {
        let reg = sample_with_history();
        let err = lookup_by_transcript(&reg, "/tmp/nope.jsonl").unwrap_err();
        assert_eq!(err, WindowLookupError::NotFound);
    }

    // ── apply_session_end tests (v4 §3.5) ──────────────────────────

    #[test]
    fn session_end_adds_ended_at_and_exit_reason() {
        let mut reg = sample_with_history();
        let (updated, _reason) = apply_session_end(
            &mut reg,
            "11111-aaaa",
            "uuid-current",
            "idle_timeout",
            "2026-05-12T12:00:00Z",
        )
        .expect("known session");
        assert!(updated);
        let row = &reg["sessions"][0]["tool_history"][1];
        assert_eq!(row["ended_at"], json!("2026-05-12T12:00:00Z"));
        assert_eq!(row["exit_reason"], json!("idle_timeout"));
    }

    #[test]
    fn session_end_idempotent_on_matching_reason() {
        let mut reg = sample_with_history();
        apply_session_end(&mut reg, "11111-aaaa", "uuid-current", "pid_dead", "t1").unwrap();
        let (updated, reason) =
            apply_session_end(&mut reg, "11111-aaaa", "uuid-current", "pid_dead", "t2").unwrap();
        assert!(!updated);
        assert!(reason.contains("exit_reason matches"));
        // ended_at NOT overwritten
        assert_eq!(reg["sessions"][0]["tool_history"][1]["ended_at"], json!("t1"));
    }

    #[test]
    fn session_end_mismatched_reason_is_first_write_wins() {
        let mut reg = sample_with_history();
        apply_session_end(&mut reg, "11111-aaaa", "uuid-current", "idle_timeout", "t1").unwrap();
        let (updated, reason) =
            apply_session_end(&mut reg, "11111-aaaa", "uuid-current", "pid_dead", "t2").unwrap();
        assert!(!updated);
        assert!(reason.contains("first write wins"));
        // Original reason preserved
        assert_eq!(
            reg["sessions"][0]["tool_history"][1]["exit_reason"],
            json!("idle_timeout")
        );
    }

    #[test]
    fn session_end_rejects_unknown_window() {
        let mut reg = sample_with_history();
        let err = apply_session_end(&mut reg, "no-such", "uuid", "idle_timeout", "t").unwrap_err();
        assert!(err.contains("window_id not found"));
    }

    #[test]
    fn session_end_rejects_session_not_in_history() {
        let mut reg = sample_with_history();
        let err =
            apply_session_end(&mut reg, "11111-aaaa", "unknown-sid", "idle_timeout", "t").unwrap_err();
        assert!(err.contains("not in tool_history"));
    }
}
