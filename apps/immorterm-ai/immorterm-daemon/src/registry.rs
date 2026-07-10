//! Session registry — shared JSON file that all daemons update atomically.
//!
//! Replaces `restore-terminals.json`, `screen-reconcile`, and `screen-cleanup`.
//!
//! Each daemon registers itself on start and deregisters on exit.
//! The extension queries `immorterm session list --json` to get the current state,
//! or reads the registry file directly for fast startup.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{error, warn, info};

use crate::commands::is_process_alive;
use crate::socket_dir;

const MAX_BACKUPS: usize = 200;

/// Resolved owner-project identity for a spawn directory.
///
/// Returned by `resolve_owner_project()`: the stable owner_project_dir (parent
/// of any worktree), and the worktree path itself if the spawn dir is inside
/// a worktree (else `None`). Falls back to treating spawn_dir as its own owner
/// when git resolution fails (non-git project, missing git binary, etc.).
pub struct OwnerProject {
    pub owner_dir: String,
    pub worktree: Option<String>,
}

/// Read the current git branch for a working directory.
///
/// Pure filesystem read — no `git` subprocess. Mirrors `detectGitBranch` in
/// the TS extension. Handles:
///   - regular checkouts (`<cwd>/.git` is a dir)
///   - worktrees (`<cwd>/.git` is a file containing `gitdir: <abs path>`)
///   - branch refs (`ref: refs/heads/<name>` → returns the branch name)
///   - detached HEAD (raw 40-char SHA → returns the 7-char short form)
///
/// Returns `None` when `cwd` isn't inside a git repo or HEAD is unreadable.
/// Cheap (one or two fs reads + a string match) so it can run on every
/// claude_interval tick without measurable cost.
pub fn read_branch_for_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() { return None }
    let dot_git = std::path::Path::new(cwd).join(".git");
    let git_dir = match fs::metadata(&dot_git) {
        Ok(m) if m.is_dir() => dot_git,
        Ok(_) => {
            // File-mode .git → worktree pointer.
            let raw = fs::read_to_string(&dot_git).ok()?;
            let pointer = raw.trim().strip_prefix("gitdir:")?.trim();
            let p = std::path::Path::new(pointer);
            if p.is_absolute() { p.to_path_buf() } else { std::path::Path::new(cwd).join(p) }
        }
        Err(_) => return None,
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(rest) = head.strip_prefix("ref:") {
        let r = rest.trim();
        return r.strip_prefix("refs/heads/").map(|s| s.to_string());
    }
    if head.len() >= 40 && head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(head[..7].to_string());
    }
    None
}

/// Resolve owner_project_dir + worktree from a spawn directory.
///
/// Per the user's "each workspace owns its sessions" model: owner_project_dir
/// is ALWAYS the spawn dir itself — never walked up to a parent trunk via
/// git-common-dir. A worktree-spawned daemon stays attributed to the worktree
/// (so opening the worktree as its own VS Code workspace shows its sessions);
/// opening the parent project does NOT pull worktree sessions in.
///
/// `worktree` is detected purely informationally — set when git resolution
/// shows the spawn dir is inside a worktree of some larger repo — but it
/// plays no role in the restore filter. Currently always returned as None
/// (CWD-watch wiring can reintroduce informational worktree later).
pub fn resolve_owner_project(spawn_dir: &str) -> OwnerProject {
    OwnerProject {
        owner_dir: spawn_dir.to_string(),
        worktree: None,
    }
}

/// Canonical project identity (WHAT dimension — see internal design notes):
/// the UUID + human-readable name stored in `<owner_dir>/.immorterm/project.json`.
pub struct ProjectIdentity {
    pub id: String,
    pub name: String,
}

/// Read or create the canonical `<owner_dir>/.immorterm/project.json`
/// (`{"id": "<uuid>", "name": "<display>"}`). This is the single source of
/// truth for `project_id` / `project_name` across the whole system.
///
/// Migration order (each step atomic, tmp+rename):
///   1. `project.json` exists → read it.
///   2. legacy bare `project-id` exists → reuse its UUID, derive `name` from
///      the directory basename, write `project.json` (leave `project-id` in
///      place for a grace period so older binaries keep working).
///   3. neither → mint a UUIDv4 + basename name, write `project.json`.
///
/// Returns `None` only if the dir is unwritable.
pub fn read_or_create_project(owner_dir: &str) -> Option<ProjectIdentity> {
    if owner_dir.is_empty() { return None }

    let dir = Path::new(owner_dir).join(".immorterm");
    let json_file = dir.join("project.json");
    let legacy_file = dir.join("project-id");

    let default_name = Path::new(owner_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    // 1. project.json already present.
    if let Ok(contents) = fs::read_to_string(&json_file)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&contents)
    {
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        if !id.is_empty() {
            let name = v.get("name").and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or(default_name);
            return Some(ProjectIdentity { id, name });
        }
    }

    if let Err(e) = fs::create_dir_all(&dir) {
        warn!("Failed to create .immorterm dir at {:?}: {}", dir, e);
        return None;
    }

    // First time we materialize .immorterm/ in a project — make sure the
    // project's .gitignore ignores the runtime state (but keeps project.json),
    // so `git status` stays clean instead of churning on claude-ctx/logs.
    ensure_gitignore(owner_dir);

    // 2. Migrate legacy bare project-id (reuse UUID), else 3. mint a fresh one.
    let id = fs::read_to_string(&legacy_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(generate_uuid_v4);

    let name = default_name;
    let payload = format!("{{\n  \"id\": \"{id}\",\n  \"name\": \"{name}\"\n}}\n");
    let tmp = json_file.with_extension("json.tmp");
    if let Err(e) = fs::write(&tmp, &payload) {
        warn!("Failed to write tmp project.json at {:?}: {}", tmp, e);
        return None;
    }
    if let Err(e) = fs::rename(&tmp, &json_file) {
        warn!("Failed to rename project.json into place at {:?}: {}", json_file, e);
        let _ = fs::remove_file(&tmp);
        return None;
    }
    info!("Wrote project.json id={} name={} at {:?}", id, name, json_file);
    Some(ProjectIdentity { id, name })
}

/// Ensure `<owner_dir>/.gitignore` ignores `.immorterm/` runtime state while
/// keeping `project.json` tracked. No-op if not a git repo, if the rule is
/// already present, or on any IO error (best-effort — never blocks a spawn).
fn ensure_gitignore(owner_dir: &str) {
    let root = Path::new(owner_dir);
    if !root.join(".git").exists() { return } // only touch actual repos

    let gi = root.join(".gitignore");
    let existing = fs::read_to_string(&gi).unwrap_or_default();
    // Already handled if any line mentions the .immorterm rule.
    if existing.lines().any(|l| {
        let l = l.trim();
        l == ".immorterm" || l == ".immorterm/" || l == ".immorterm/*"
    }) {
        return;
    }

    let block = "\n# ImmorTerm runtime state (keep project.json so teammates/clones\n# share one memory partition; ignore churny logs + claude-ctx)\n.immorterm/*\n!.immorterm/project.json\n";
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') { out.push('\n'); }
    out.push_str(block);
    if let Err(e) = fs::write(&gi, out) {
        warn!("Failed to update .gitignore at {:?}: {}", gi, e);
    } else {
        info!("Added .immorterm rule to {:?}", gi);
    }
}

/// Back-compat shim: previous callers want just the UUID. Delegates to
/// [`read_or_create_project`] so the canonical `project.json` is created/read.
pub fn read_or_create_project_id(owner_dir: &str) -> Option<String> {
    read_or_create_project(owner_dir).map(|p| p.id)
}

/// Flag file marking that the one-time memory-onboarding hint was shown.
const MEMORY_HINT_FLAG: &str = "memory-hint-shown";

/// One-time hint shown in a new session when memory can't be auto-wired.
const MEMORY_HINT: &str =
    "memory isn't wired into this project yet — run `npx immorterm init` once to set it up.";

/// Memory-wiring bootstrap for the owner project (last onboarding gap:
/// a project opened via the Tauri app gets `.immorterm/project.json` +
/// terminals, but no memory hooks — the TS hook-installer only runs from
/// the CLI and the VS Code extension).
///
/// If `<owner_dir>/.immorterm/hooks/` is missing or empty:
///   - probe for an installed `immorterm` CLI via a login shell (GUI-spawned
///     daemons have a minimal PATH), and if found spawn
///     `immorterm hooks install --project <owner_dir>` non-blocking;
///   - otherwise return a one-time hint line (persisted flag in `.immorterm/`
///     so it shows once per project, not on every spawn).
///
/// Best-effort everywhere — never fails or blocks the session spawn beyond
/// the CLI probe itself. Returns `Some(hint)` only when the hint should be
/// rendered in this session.
pub fn ensure_memory_hooks(owner_dir: &str) -> Option<String> {
    if owner_dir.is_empty() { return None }
    let hooks_dir = Path::new(owner_dir).join(".immorterm").join("hooks");
    // Already wired (dir exists and is non-empty) → cheap no-op, skip the probe.
    if fs::read_dir(&hooks_dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return None;
    }
    // `command -v immorterm` can resolve to the C terminal binary (screen
    // fork, same name — e.g. the homebrew formula), which swallows
    // `hooks install` args with exit 0 and installs nothing. Only accept a
    // candidate whose `hooks status` actually answers like the Node CLI.
    let cli = probe_immorterm_cli().filter(|c| cli_supports_hooks(c, owner_dir));
    ensure_memory_hooks_with(owner_dir, cli)
}

/// Testable core of [`ensure_memory_hooks`]: `cli` is the probed CLI path
/// (`None` = not installed). Assumes the hooks dir was already found missing.
fn ensure_memory_hooks_with(owner_dir: &str, cli: Option<String>) -> Option<String> {
    use std::process::{Command, Stdio};

    if let Some(cli_path) = cli {
        match Command::new(&cli_path)
            .args(["hooks", "install", "--project", owner_dir])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                info!(
                    "Memory hooks missing — spawned `{} hooks install --project {}` (pid {})",
                    cli_path, owner_dir, child.id()
                );
                // Reap in a detached thread — a dropped unreaped Child zombies
                // (see the claude-detection ps leak incident).
                std::thread::spawn(move || { let _ = child.wait(); });
            }
            Err(e) => warn!("Failed to spawn `{} hooks install`: {}", cli_path, e),
        }
        return None; // install handles it (or logged); no hint either way
    }

    // No CLI installed → surface the hint once per project.
    let dir = Path::new(owner_dir).join(".immorterm");
    let flag = dir.join(MEMORY_HINT_FLAG);
    if flag.exists() { return None }
    if fs::create_dir_all(&dir).is_err() || fs::write(&flag, "").is_err() {
        return None; // unwritable project dir — don't hint every spawn
    }
    Some(MEMORY_HINT.to_string())
}

/// Probe for the `immorterm` CLI through a login shell. GUI-spawned daemons
/// inherit a minimal PATH (the known trap), so `command -v` must run under
/// the user's login environment. Never uses npx (surprise network install).
fn probe_immorterm_cli() -> Option<String> {
    let sh = if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" };
    std::process::Command::new(sh)
        .args(["-lc", "command -v immorterm"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Validate that a probed `immorterm` binary is the Node CLI with the
/// `hooks` subcommand — not the C terminal binary (which exits 0 on
/// arbitrary args) and not an older npm CLI (unknown command). Exit codes
/// don't discriminate (Node `hooks status` exits 1 when not installed; the
/// C binary exits 0 on garbage), so match the command's signature output.
// ponytail: string-match on "Memory hooks"; replace with a `hooks status --json`
// contract if this ever needs more than a yes/no.
fn cli_supports_hooks(cli: &str, owner_dir: &str) -> bool {
    std::process::Command::new(cli)
        .args(["hooks", "status", "--project", owner_dir])
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            text.contains("Memory hooks")
        })
        .unwrap_or(false)
}

pub(crate) fn generate_uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Crude but adequate: 16 random-ish bytes from nanos + pid + counter,
    // formatted as UUIDv4. We accept the entropy weakness because the file
    // is written exactly once per project and uniqueness only matters across
    // a single user's projects.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&secs.to_le_bytes());
    bytes[8..12].copy_from_slice(&nanos.to_le_bytes());
    bytes[12..16].copy_from_slice(&pid.to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Display names we consider "generic" and safe to overwrite when a more
/// specific user-set name exists. Used by Registry::register to avoid
/// dropping a user's custom tab label when a concurrent writer registers
/// with a default/bootstrap value.
fn is_generic_display_name(name: &str) -> bool {
    name.is_empty()
        || name == "zsh"
        || name.starts_with("immorterm-")
}

/// Path to the shared registry file.
fn registry_path() -> PathBuf {
    let dir = socket_dir(); // ~/.immorterm/sockets/
    dir.parent()
        .unwrap_or(&dir)
        .join("registry.json")
}

/// Path to the backup directory.
fn backup_dir() -> PathBuf {
    let dir = socket_dir();
    dir.parent()
        .unwrap_or(&dir)
        .join("registry-backups")
}

/// Backup current registry.json before overwriting.
///
/// Shrinkage guard: if the current on-disk registry has dropped by >20% vs the
/// most recent backup (and the prior backup had >5 sessions), skip capturing
/// this state. Preserves the larger backup as the recovery point so a
/// stale-cache writer cannot bury the source of truth one auto-backup at a
/// time — see the 2026-05-18 incident.
fn backup_registry() {
    let path = registry_path();
    if !path.exists() {
        return;
    }

    let dir = backup_dir();

    let new_count = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Registry>(&s).ok())
        .map(|r| r.sessions.len())
        .unwrap_or(0);

    if let Some((latest_path, _)) = collect_backup_files(&dir).last() {
        let latest_count = fs::read_to_string(latest_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Registry>(&s).ok())
            .map(|r| r.sessions.len())
            .unwrap_or(0);
        if latest_count > 5 && new_count * 5 < latest_count * 4 {
            warn!(
                "Skipping registry backup: current registry has {} sessions but most recent backup had {} (>20% drop)",
                new_count, latest_count
            );
            return;
        }
    }

    let today = today_date_string();
    let day_dir = dir.join(&today);
    if let Err(e) = fs::create_dir_all(&day_dir) {
        warn!("Failed to create backup day dir: {}", e);
        return;
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_path = day_dir.join(format!("registry.{}.json", timestamp));

    if backup_path.exists() {
        return;
    }

    if let Err(e) = fs::copy(&path, &backup_path) {
        warn!("Failed to backup registry: {}", e);
        return;
    }

    prune_backups(&dir);
}

/// Walk the backup tree (day subdirs + legacy flat files) and return all
/// backup files with their mtime, newest last. Used by prune + recovery.
fn collect_backup_files(root: &std::path::Path) -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
    let mut files = Vec::new();
    let Ok(top) = fs::read_dir(root) else { return files };
    for entry in top.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(inner) = fs::read_dir(&path) {
                for inner_entry in inner.flatten() {
                    let inner_path = inner_entry.path();
                    let Some(name) = inner_path.file_name().and_then(|s| s.to_str()) else { continue };
                    if !(name.starts_with("registry.") && name.ends_with(".json")) { continue; }
                    if let Ok(meta) = fs::metadata(&inner_path)
                        && let Ok(mtime) = meta.modified()
                    {
                        files.push((inner_path, mtime));
                    }
                }
            }
        } else if let Some(name) = path.file_name().and_then(|s| s.to_str())
            && name.starts_with("registry.") && name.ends_with(".json")
            && let Ok(meta) = fs::metadata(&path)
            && let Ok(mtime) = meta.modified()
        {
            // Legacy flat file (pre hierarchical layout).
            files.push((path, mtime));
        }
    }
    files.sort_by_key(|(_, m)| *m);
    files
}

/// Keep only the newest MAX_BACKUPS files across the entire backup tree.
/// Walks day subfolders AND legacy flat files so both layouts coexist during
/// migration. Removes empty day dirs after pruning.
fn prune_backups(dir: &std::path::Path) {
    let files = collect_backup_files(dir);
    if files.len() <= MAX_BACKUPS { return; }

    let to_delete = files.len() - MAX_BACKUPS;
    for (path, _) in &files[..to_delete] {
        let _ = fs::remove_file(path);
    }
    info!("Pruned {} old registry backups (kept {})", to_delete, MAX_BACKUPS);

    // Clean up empty day subfolders left behind by pruning.
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && let Ok(inner) = fs::read_dir(&path)
                && inner.count() == 0
            {
                let _ = fs::remove_dir(&path);
            }
        }
    }
}

/// Try to recover a single entry from backups by PID or window_id.
/// Walks the full backup tree (day subfolders + legacy flat files) newest-first.
pub fn recover_entry_from_backup(pid: u32, window_id: &str) -> Option<RegistryEntry> {
    let dir = backup_dir();
    let files = collect_backup_files(&dir);
    for (backup_path, _) in files.iter().rev() {
        let Ok(contents) = fs::read_to_string(backup_path) else { continue };
        let Ok(registry) = serde_json::from_str::<Registry>(&contents) else { continue };
        if let Some(entry) = registry.sessions.iter().find(|e| e.pid == pid) {
            info!(
                "Recovered entry from backup {:?} (matched PID {})",
                backup_path.file_name().unwrap_or_default(),
                pid
            );
            return Some(entry.clone());
        }
        if !window_id.is_empty()
            && let Some(entry) = registry.sessions.iter().find(|e| e.window_id == window_id)
        {
            info!(
                "Recovered entry from backup {:?} (matched window_id {})",
                backup_path.file_name().unwrap_or_default(),
                window_id
            );
            return Some(entry.clone());
        }
    }
    None
}

/// Try to load registry from the latest non-empty backup.
/// Walks day subfolders + legacy flat files newest-first.
fn read_latest_backup() -> Option<Registry> {
    let dir = backup_dir();
    let files = collect_backup_files(&dir);
    for (backup_path, _) in files.iter().rev() {
        let Ok(contents) = fs::read_to_string(backup_path) else { continue };
        let Ok(registry) = serde_json::from_str::<Registry>(&contents) else { continue };
        if !registry.sessions.is_empty() {
            info!(
                "Recovered registry from backup: {:?} ({} sessions)",
                backup_path.file_name().unwrap_or_default(),
                registry.sessions.len()
            );
            return Some(registry);
        }
    }
    None
}

/// One entry in the per-window `tool_history` timeline (see `RegistryEntry`).
/// Written by the hub's session-link endpoint each time a vendor hook
/// self-announces. Daemon preserves the whole list across rewrites via
/// `#[serde(default)]` on the parent field — losing this struct or any of
/// its members would silently strip vendor-history rows on the next save,
/// same failure pattern as the `tool` field hit in commit `0963e3e9`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolHistoryEntry {
    /// Vendor identifier (claude-code, codex, cursor, windsurf, cline,
    /// opencode, gemini, aider, copilot).
    pub tool: String,
    /// Vendor's own session id (Claude UUID, Codex session-id, etc.).
    pub session_id: String,
    /// Path to the vendor's transcript file at link time.
    pub transcript_path: String,
    /// RFC3339 UTC timestamp when this link was made (e.g.
    /// `2026-05-07T20:48:00Z`).
    pub ts: String,
}

/// Claude process stats stored in the registry (written by VS Code extension).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeStatsEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default)]
    pub rss_kb: u64,
    #[serde(default)]
    pub cpu_percent: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<u64>,
    #[serde(default)]
    pub runtime_secs: u64,
    /// Model display name (e.g. "Claude Opus 4")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Total session cost in USD
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Context window usage percentage (0-100)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<f64>,
}

/// A single session entry in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Process ID of the daemon
    pub pid: u32,
    /// Session name (e.g., "immorterm-ai-abc12345")
    pub name: String,
    /// Window ID for VS Code terminal identity
    pub window_id: String,
    /// Display name (friendly name for tab)
    pub display_name: String,
    /// Project directory
    pub project_dir: String,
    /// Claude session ID (if associated)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    /// Whether title is locked by user
    #[serde(default)]
    pub title_locked: bool,
    /// Current terminal title
    #[serde(default)]
    pub title: String,
    /// Log file path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logfile: Option<String>,
    /// Shell path
    pub shell: String,
    /// Creation timestamp (Unix seconds)
    pub created_at: u64,

    // ── Phase 2A: Extension-managed fields ──────────────────────
    /// Session type: "regular" or "ai"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_type: Option<String>,
    /// WebSocket port (AI sessions only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_port: Option<u16>,
    /// Theme name (e.g., "aurora-borealis")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Claude transcript JSONL path
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_transcript_path: Option<String>,
    /// Claude process stats (written by extension)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_stats: Option<ClaudeStatsEntry>,

    /// AI tool driving this session.
    /// One of: claude-code, codex, cursor, windsurf, cline, opencode, gemini, aider, copilot.
    /// `None` on legacy entries — readers should default to "claude-code".
    /// Set by the hub's POST /api/v1/registry/session-link endpoint when a hook
    /// self-announces. Daemon preserves it across rewrites via #[serde(default)].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,

    /// Append-only timeline of `(tool, session_id, transcript_path, ts)`
    /// tuples written by hub session-link calls. Lets us reconstruct which
    /// vendor was active in this immorterm window over time even though
    /// `tool` / `claude_session_id` are overwritten on each link. Daemon
    /// preserves this across rewrites via `#[serde(default)]` — same
    /// pattern as `tool` above. Empty by default; serialized only when
    /// non-empty so legacy entries don't gain an empty `[]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_history: Vec<ToolHistoryEntry>,

    /// Session lifecycle status: "active", "shelved", "dead"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_status: Option<String>,
    /// Unix timestamp (seconds) when session was shelved
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shelved_at: Option<u64>,

    /// Structured log directory (contains .grid.jsonl, .cast, .ai.jsonl)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_log_dir: Option<String>,

    /// Claude needs user attention (permission prompt or idle).
    /// Persists through VS Code reload so the badge survives restarts.
    #[serde(default)]
    pub needs_attention: bool,

    /// Agent is currently working (between UserPromptSubmit and Stop).
    /// Persists like `needs_attention` so VS Code reload mid-turn keeps the
    /// pulse; daemons reset it to `false` when registering themselves at spawn,
    /// so a cold-boot daemon never falsely reports "working".
    #[serde(default)]
    pub is_working: bool,

    /// Stable owner project directory. Resolved at spawn from
    /// $SCREEN_PROJECT_DIR via `git rev-parse --git-common-dir`: if the
    /// spawn dir is a git worktree, this is the parent project root. Never
    /// mutated after creation. The restore filter matches on this, not on
    /// `project_dir`, so worktree-spawned sessions stay visible from the
    /// parent project's VS Code workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_project_dir: Option<String>,

    /// Stable owner project identity. Read from
    /// `<owner_project_dir>/.immorterm/project.json` at spawn (UUID created
    /// on first session if missing). Survives project renames and moves
    /// between machines — the restore filter prefers this over path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_project_id: Option<String>,

    /// Human-readable project name (the `name` field of project.json — the
    /// WHAT display label in the identity model). Mirrored here so consumers
    /// (extension modal, status bar) don't re-read the file per request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_project_name: Option<String>,

    /// Current git worktree path when the daemon is operating inside one.
    /// Set at spawn if `$SCREEN_PROJECT_DIR != owner_project_dir`, then
    /// updated live from OSC 7 cwd changes when Claude `cd`s between
    /// trunk and worktrees. `None` when the daemon is on the trunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
}

/// The full registry state.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    pub sessions: Vec<RegistryEntry>,
}

impl Registry {
    /// Load the registry from disk (or return empty if doesn't exist).
    /// On parse failure, attempts recovery from the latest backup.
    pub fn load() -> Self {
        let path = registry_path();
        match fs::read_to_string(&path) {
            Ok(contents) => {
                match serde_json::from_str::<Registry>(&contents) {
                    Ok(registry) => registry,
                    Err(e) => {
                        // ROOT CAUSE FIX #2: Parse failure — recover from backup
                        warn!("Failed to parse registry.json: {} — trying backup", e);
                        read_latest_backup().unwrap_or_default()
                    }
                }
            }
            Err(e) => {
                if path.exists() {
                    // File exists but unreadable — try backup
                    warn!("Failed to read registry.json: {} — trying backup", e);
                    read_latest_backup().unwrap_or_default()
                } else {
                    Self::default()
                }
            }
        }
    }

    /// Save the registry to disk (atomic: write tmp + rename).
    /// Backs up the current file before overwriting.
    ///
    /// Shrinkage guard: if the new state would drop the on-disk session count
    /// by more than 20% (when disk has >5 sessions), refuse the write. Mirrors
    /// the prune guard at `prune()`. Catches stale-cache writers that would
    /// silently clobber the source of truth — see the 2026-05-18 incident
    /// where a hub writer with a 53-session in-memory view overwrote a
    /// 73-session truth, losing 20 sessions.
    pub fn save(&self) -> std::io::Result<()> {
        let path = registry_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Shrinkage guard
        let new_count = self.sessions.len();
        if let Ok(disk_data) = fs::read_to_string(&path)
            && let Ok(disk_reg) = serde_json::from_str::<Registry>(&disk_data)
        {
            let disk_count = disk_reg.sessions.len();
            if disk_count > 5 && new_count * 5 < disk_count * 4 {
                warn!(
                    "Refusing to save registry.json: would shrink from {} → {} sessions (>20% drop). \
                     Likely a stale-cache writer.",
                    disk_count, new_count
                );
                return Err(std::io::Error::other(
                    "registry shrinkage guard: refused write",
                ));
            }
        }

        // LAYER 1: Backup before overwriting
        backup_registry();

        let tmp = path.with_extension("tmp");
        let json = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)?;
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Prune dead sessions (process no longer alive).
    /// ROOT CAUSE FIX #3: Refuses to prune if >80% of entries would be removed
    /// (indicates a laptop restart, not actual dead sessions).
    pub fn prune(&mut self) {
        let total = self.sessions.len();
        if total == 0 {
            return;
        }

        let alive_count = self.sessions.iter().filter(|e| is_process_alive(e.pid)).count();
        let dead_count = total - alive_count;

        // Safety: if >80% would be pruned, this is likely a restart — refuse
        if dead_count > 0 && dead_count * 100 / total > 80 {
            warn!(
                "Refusing to prune: {}/{} sessions have dead PIDs (likely laptop restart). \
                 Use 'immorterm session cleanup --force' to override.",
                dead_count, total
            );
            return;
        }

        self.sessions.retain(|entry| is_process_alive(entry.pid));
    }

    /// Force-prune dead sessions regardless of safety threshold.
    /// Only use via explicit `--force` flag.
    pub fn prune_force(&mut self) {
        let before = self.sessions.len();
        self.sessions.retain(|entry| is_process_alive(entry.pid));
        let pruned = before - self.sessions.len();
        if pruned > 0 {
            warn!("Force-pruned {}/{} dead sessions", pruned, before);
        }
    }

    /// Register a new session, merging forward any fields that existed on a prior
    /// matching entry but aren't set on the incoming entry.
    ///
    /// **Why the merge exists**: this registry is a multi-writer file. The daemon's
    /// `register_session()` knows pid, name, window_id, display_name,
    /// claude_session_id (from env), title_locked, shell, project_dir,
    /// session_type, created_at, structured_log_dir. The extension writes *other*
    /// fields asynchronously: `theme`, `claude_transcript_path`, `claude_stats`,
    /// `session_status`, `shelved_at`, and sometimes a more recent
    /// `claude_session_id` (via claude-sync.ts). Without this merge, every daemon
    /// respawn silently wipes those extension-managed fields — which is exactly
    /// what caused a restore failure we hit (claude_session_id race-wiped
    /// from registry, breaking auto-resume on reboot).
    ///
    /// The dedup then runs as before (name OR window_id match → replace) so there's
    /// still exactly one live entry per identity.
    pub fn register(&mut self, mut entry: RegistryEntry) {
        if let Some(existing) = self.sessions.iter().find(|e| {
            e.name == entry.name
                || (!entry.window_id.is_empty() && e.window_id == entry.window_id)
        }) {
            if entry.claude_session_id.is_none() {
                entry.claude_session_id = existing.claude_session_id.clone();
            }
            if entry.theme.is_none() {
                entry.theme = existing.theme.clone();
            }
            if entry.claude_transcript_path.is_none() {
                entry.claude_transcript_path = existing.claude_transcript_path.clone();
            }
            if entry.claude_stats.is_none() {
                entry.claude_stats = existing.claude_stats.clone();
            }
            if entry.session_status.is_none() {
                entry.session_status = existing.session_status.clone();
            }
            if entry.shelved_at.is_none() {
                entry.shelved_at = existing.shelved_at;
            }
            // Preserve append-only vendor timeline across daemon respawn.
            // Incoming entry from `register_session()` always starts with
            // an empty Vec; without this merge we'd silently drop every
            // history row written by hub session-link since the prior
            // registration.
            if entry.tool_history.is_empty() && !existing.tool_history.is_empty() {
                entry.tool_history = existing.tool_history.clone();
            }
            // Same back-compat carry for `tool` itself — incoming
            // register_session() leaves it None, but a vendor hook may
            // have stamped it via session-link in the meantime.
            if entry.tool.is_none() && existing.tool.is_some() {
                entry.tool = existing.tool.clone();
            }
            // Title: preserve existing when incoming is empty; if it was locked, keep locked.
            if entry.title.is_empty() && !existing.title.is_empty() {
                entry.title = existing.title.clone();
                if existing.title_locked {
                    entry.title_locked = true;
                }
            }
            // Display name: preserve user-set over generic fallback.
            if is_generic_display_name(&entry.display_name)
                && !is_generic_display_name(&existing.display_name)
            {
                entry.display_name = existing.display_name.clone();
            }
        }

        // Remove any existing entry with same name OR same window_id.
        // Name dedup handles normal restarts; window_id dedup handles shelve/reattach
        // where the shelved entry persists with the same window_id but the new daemon
        // gets a fresh PID (and potentially different name format).
        self.sessions.retain(|e| {
            e.name != entry.name
                && (entry.window_id.is_empty() || e.window_id != entry.window_id)
        });
        self.sessions.push(entry);
    }

    /// Deregister a session by PID.
    pub fn deregister(&mut self, pid: u32) {
        self.sessions.retain(|e| e.pid != pid);
    }

    /// Find entry by window_id.
    pub fn find_by_window_id(&self, window_id: &str) -> Option<&RegistryEntry> {
        self.sessions.iter().find(|e| e.window_id == window_id)
    }

    /// Find entry by PID (used for daemon self-healing).
    pub fn find_by_pid(&self, pid: u32) -> Option<&RegistryEntry> {
        self.sessions.iter().find(|e| e.pid == pid)
    }

    /// Update the Claude session ID for a window.
    pub fn update_claude_session(&mut self, window_id: &str, claude_id: &str) {
        if let Some(entry) = self.sessions.iter_mut().find(|e| e.window_id == window_id) {
            entry.claude_session_id = Some(claude_id.to_string());
        }
    }

    /// Update Claude stats (process + API) for a window.
    pub fn update_claude_stats(&mut self, window_id: &str, claude: &crate::claude::ClaudeTracker) {
        if let Some(entry) = self.sessions.iter_mut().find(|e| e.window_id == window_id) {
            entry.claude_stats = Some(ClaudeStatsEntry {
                pid: claude.claude_pid,
                rss_kb: claude.rss_kb,
                cpu_percent: claude.cpu_percent as f64,
                start_time: claude.start_time.map(|t| t.elapsed().as_secs()),
                runtime_secs: claude.runtime_secs(),
                model: if claude.api_stats.model.is_empty() { None } else { Some(claude.api_stats.model.clone()) },
                cost_usd: if claude.api_stats.cost_usd > 0.0 { Some(claude.api_stats.cost_usd) } else { None },
                context_pct: if claude.api_stats.context_pct > 0.0 { Some(claude.api_stats.context_pct) } else { None },
            });
        }
    }

    /// Generate restore-terminals.json format for VS Code extension compatibility.
    ///
    /// This generates the same structure that `screen-reconcile` used to build,
    /// so the extension's terminal restoration works without changes.
    pub fn to_restore_json(&self) -> serde_json::Value {
        let terminals: Vec<serde_json::Value> = self
            .sessions
            .iter()
            .filter(|e| is_process_alive(e.pid))
            .map(|e| {
                let mut terminal = serde_json::json!({
                    "name": e.display_name,
                    "windowId": e.window_id,
                    "shellPath": "/bin/zsh",
                    "commands": [
                        format!(
                            "exec immorterm session auto \"{}\" \"{}\"",
                            e.window_id, e.display_name
                        )
                    ],
                });
                if let Some(ref claude_id) = e.claude_session_id {
                    terminal["claudeSessionId"] = serde_json::json!(claude_id);
                }
                serde_json::json!({
                    "splitTerminals": [terminal]
                })
            })
            .collect();

        serde_json::json!({
            "artificialDelayMilliseconds": 0,
            "terminals": terminals
        })
    }
}

/// Register the current daemon process in the shared registry.
///
/// Called during daemon startup (in `run_daemon`).
pub fn register_session(
    name: &str,
    shell: &str,
    logfile: Option<&str>,
) {
    let window_id = std::env::var("IMMORTERM_WINDOW_ID")
        .or_else(|_| std::env::var("SCREEN_WINDOW_ID"))
        .unwrap_or_default();
    let display_name = std::env::var("IMMORTERM_DISPLAY_NAME")
        .or_else(|_| std::env::var("SCREEN_WINDOW_NAME"))
        .unwrap_or_else(|_| name.to_string());
    let project_dir = std::env::var("SCREEN_PROJECT_DIR")
        .unwrap_or_default();

    // Resolve owner_project_dir + worktree from project_dir via git.
    // Worktree-spawned daemons end up with owner_project_dir = trunk, worktree = spawn dir.
    let owner = resolve_owner_project(&project_dir);
    let owner_identity = read_or_create_project(&owner.owner_dir);
    let owner_project_id = owner_identity.as_ref().map(|p| p.id.clone());
    let owner_project_name = owner_identity.as_ref().map(|p| p.name.clone());

    let claude_session_id = std::env::var("IMMORTERM_CLAUDE_SESSION_ID").ok()
        .filter(|s| !s.is_empty());
    let title_locked = std::env::var("IMMORTERM_TITLE_LOCKED")
        .map(|v| v == "1")
        .unwrap_or(false);

    let session_type = std::env::var("IMMORTERM_SESSION_TYPE").ok()
        .filter(|s| !s.is_empty());
    let ws_port = None; // Set later by daemon after WebSocket starts

    // Compute per-session structured log directory: {base}/{date}_{window_id}/
    let base_log_dir = if !project_dir.is_empty() {
        Some(format!("{}/.immorterm/terminals/logs", project_dir))
    } else {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            Some(format!("{}/.immorterm/logs", home))
        } else {
            None
        }
    };
    let structured_log_dir = base_log_dir.map(|base| {
        let dir_suffix = if !window_id.is_empty() {
            window_id.as_str()
        } else {
            name
        };
        let base_path = std::path::Path::new(&base);
        // Reuse existing session directory if one exists for this window_id
        // (matches both new bare-windowId names AND legacy date-prefixed names).
        if let Some(existing) = find_existing_session_dir(base_path, dir_suffix) {
            existing.to_string_lossy().into_owned()
        } else {
            // New naming: bare windowId (no date prefix). WindowId entropy is high
            // enough that collisions are impossible; date-prefix only caused
            // proliferation when find_existing missed, creating a fresh dated dir
            // each day for the same window. See task #24 / agent #21 diagnosis.
            format!("{}/{}", base, dir_suffix)
        }
    });

    let entry = RegistryEntry {
        pid: std::process::id(),
        name: name.to_string(),
        window_id,
        display_name,
        project_dir,
        claude_session_id,
        title_locked,
        title: String::new(),
        logfile: logfile.map(|s| s.to_string()),
        shell: shell.to_string(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        session_type,
        ws_port,
        theme: None,
        claude_transcript_path: None,
        claude_stats: None,
        tool: None,
        tool_history: Vec::new(),
        session_status: None,
        shelved_at: None,
        structured_log_dir,
        needs_attention: false,
        is_working: false,
        owner_project_dir: if owner.owner_dir.is_empty() { None } else { Some(owner.owner_dir) },
        owner_project_id,
        owner_project_name,
        worktree: owner.worktree,
    };

    // Write session.json inside the per-session log directory
    write_session_json(&entry);

    let mut registry = Registry::load();
    // Do NOT prune here — dead entries are the restore state after laptop restart.
    // Pruning happens explicitly via `immorterm session cleanup`.
    registry.register(entry);
    if let Err(e) = registry.save() {
        error!("Failed to register session in registry: {}", e);
    }
}

/// Write a `session.json` file inside the session's structured log directory.
///
/// Contains the full registry entry as a self-contained metadata file.
pub fn write_session_json(entry: &RegistryEntry) {
    let Some(ref log_dir) = entry.structured_log_dir else {
        return;
    };
    let dir = std::path::Path::new(log_dir);
    if let Err(e) = fs::create_dir_all(dir) {
        error!("Failed to create session log dir {:?}: {}", dir, e);
        return;
    }
    let session_json_path = dir.join("session.json");
    match serde_json::to_string_pretty(entry) {
        Ok(json) => {
            if let Err(e) = fs::write(&session_json_path, json) {
                error!("Failed to write session.json: {}", e);
            }
        }
        Err(e) => error!("Failed to serialize session.json: {}", e),
    }
}

/// Get today's date as YYYY-MM-DD string (UTC).
pub fn today_date_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    // Civil date algorithm (Howard Hinnant)
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Find an existing per-session directory for the given suffix (window_id or session name).
///
/// Scans `base_dir` for a directory matching EITHER:
///   - new format: bare `{suffix}` (post-2026-04-21, no date prefix), OR
///   - legacy format: `{YYYY-MM-DD}_{suffix}` (pre-2026-04-21 date-prefixed).
///
/// Returns the first match. Respawned daemons reuse the original directory
/// regardless of which naming era created it.
pub fn find_existing_session_dir(base_dir: &std::path::Path, suffix: &str) -> Option<std::path::PathBuf> {
    let entries = match std::fs::read_dir(base_dir) {
        Ok(e) => e,
        Err(_) => return None,
    };
    let legacy_target = format!("_{}", suffix);
    for entry in entries.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        // New naming — bare windowId (no date prefix).
        if name_str == suffix {
            return Some(entry.path());
        }
        // Legacy naming — "YYYY-MM-DD_{suffix}" (10 date chars + '_' + suffix = 11+len).
        if name_str.ends_with(&legacy_target) && name_str.len() == 11 + suffix.len() {
            return Some(entry.path());
        }
    }
    None
}

/// Find the newest claude-env/<uuid>.env file whose content has
/// `IMMORTERM_ID=<window_id>`. Used by the daemon to backfill
/// `claude_session_id` when OSC 1337 was never emitted (e.g. older Claude
/// versions or when Claude started after daemon boot without passing the
/// env var downstream). Returns the Claude UUID (filename without `.env`)
/// or None if no matching file exists.
pub fn resolve_claude_uuid_via_env(window_id: &str) -> Option<String> {
    let env_dir = crate::dirs_home().join(".immorterm").join("claude-env");
    let entries = std::fs::read_dir(&env_dir).ok()?;
    let needle = format!("IMMORTERM_ID={}", window_id);
    let mut best: Option<(String, std::time::SystemTime)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
        if !name.ends_with(".env") { continue; }
        let Ok(contents) = std::fs::read_to_string(&path) else { continue };
        // Line-oriented exact match on IMMORTERM_ID=<wid>
        let matches = contents.lines().any(|line| {
            line.trim() == needle || line.trim().starts_with(&format!("{} ", needle))
        });
        if !matches { continue; }
        let mtime = entry.metadata().and_then(|m| m.modified()).ok()?;
        let uuid = name.trim_end_matches(".env").to_string();
        match &best {
            Some((_, best_mtime)) if *best_mtime >= mtime => {}
            _ => best = Some((uuid, mtime)),
        }
    }
    best.map(|(u, _)| u)
}

/// Deregister the current daemon process from the shared registry.
///
/// Called during daemon shutdown.
pub fn deregister_session() {
    let pid = std::process::id();
    let mut registry = Registry::load();
    registry.deregister(pid);
    if let Err(e) = registry.save() {
        error!("Failed to deregister session from registry: {}", e);
    }
}

#[cfg(test)]
mod project_identity_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Unique temp dir per test (no tempfile dep; pid + atomic counter).
    fn temp_owner_dir() -> String {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("imterm-projtest-{}-{}", std::process::id(), n));
        let _ = fs::create_dir_all(&p);
        p.to_string_lossy().to_string()
    }

    #[test]
    fn mints_fresh_project_json_with_basename_name() {
        let owner = temp_owner_dir();
        let id = read_or_create_project(&owner).expect("should create");
        assert!(!id.id.is_empty());
        // name = basename of the owner dir
        let expected = Path::new(&owner).file_name().unwrap().to_str().unwrap();
        assert_eq!(id.name, expected);
        // project.json now exists and is valid JSON with the same id.
        let raw = fs::read_to_string(Path::new(&owner).join(".immorterm/project.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["id"].as_str().unwrap(), id.id);
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn reuses_uuid_when_migrating_from_legacy_project_id() {
        let owner = temp_owner_dir();
        let dir = Path::new(&owner).join(".immorterm");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("project-id"), "legacy-uuid-1234\n").unwrap();

        let id = read_or_create_project(&owner).expect("should migrate");
        assert_eq!(id.id, "legacy-uuid-1234", "must reuse the legacy UUID");
        // project.json written with the reused id; legacy file left in place.
        assert!(dir.join("project.json").exists());
        assert!(dir.join("project-id").exists(), "legacy file kept for grace period");
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn reads_existing_project_json_verbatim() {
        let owner = temp_owner_dir();
        let dir = Path::new(&owner).join(".immorterm");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("project.json"), r#"{"id":"abc-123","name":"My Project"}"#).unwrap();

        let id = read_or_create_project(&owner).expect("should read");
        assert_eq!(id.id, "abc-123");
        assert_eq!(id.name, "My Project");
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn project_json_is_idempotent() {
        let owner = temp_owner_dir();
        let first = read_or_create_project(&owner).unwrap();
        let second = read_or_create_project(&owner).unwrap();
        assert_eq!(first.id, second.id, "id stable across calls");
        assert_eq!(first.name, second.name);
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn gitignore_added_only_in_repos_and_idempotent() {
        // Non-repo: no .gitignore touched.
        let owner = temp_owner_dir();
        read_or_create_project(&owner).unwrap();
        assert!(!Path::new(&owner).join(".gitignore").exists(), "no repo → no .gitignore");
        let _ = fs::remove_dir_all(&owner);

        // Repo: rule appended, project.json negation present, idempotent, and
        // an existing rule is left untouched.
        let owner = temp_owner_dir();
        fs::create_dir_all(Path::new(&owner).join(".git")).unwrap();
        fs::write(Path::new(&owner).join(".gitignore"), "node_modules\n").unwrap();
        read_or_create_project(&owner).unwrap();
        let gi = fs::read_to_string(Path::new(&owner).join(".gitignore")).unwrap();
        assert!(gi.contains(".immorterm/*"));
        assert!(gi.contains("!.immorterm/project.json"));
        assert!(gi.contains("node_modules"), "existing rules preserved");
        // Second call must not append a duplicate block.
        ensure_gitignore(&owner);
        let gi2 = fs::read_to_string(Path::new(&owner).join(".gitignore")).unwrap();
        assert_eq!(gi, gi2, "idempotent — no duplicate rule");
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn back_compat_shim_returns_just_uuid() {
        let owner = temp_owner_dir();
        let full = read_or_create_project(&owner).unwrap();
        let just_id = read_or_create_project_id(&owner).unwrap();
        assert_eq!(full.id, just_id);
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn memory_hooks_noop_when_hooks_present() {
        let owner = temp_owner_dir();
        let hooks = Path::new(&owner).join(".immorterm").join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        fs::write(hooks.join("immorterm-memory-guide.sh"), "#!/bin/sh\n").unwrap();
        // Wired project → no probe, no hint, no flag.
        assert_eq!(ensure_memory_hooks(&owner), None);
        assert!(!Path::new(&owner).join(".immorterm").join(MEMORY_HINT_FLAG).exists());
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn memory_hooks_hint_shown_once_when_cli_missing() {
        let owner = temp_owner_dir();
        let first = ensure_memory_hooks_with(&owner, None);
        assert_eq!(first.as_deref(), Some(MEMORY_HINT));
        assert!(Path::new(&owner).join(".immorterm").join(MEMORY_HINT_FLAG).exists());
        // Second spawn: flag persists → no repeat hint.
        assert_eq!(ensure_memory_hooks_with(&owner, None), None);
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn memory_hooks_spawns_cli_install_when_found() {
        let owner = temp_owner_dir();
        // Fake CLI that records its argv, so we can assert the spawn contract.
        let cli = Path::new(&owner).join("fake-immorterm");
        let out = Path::new(&owner).join("cli-args.txt");
        fs::write(&cli, format!("#!/bin/sh\necho \"$@\" > {}\n", out.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();

        let hint = ensure_memory_hooks_with(&owner, Some(cli.to_string_lossy().into_owned()));
        assert_eq!(hint, None, "CLI found → install path, no hint");
        assert!(!Path::new(&owner).join(".immorterm").join(MEMORY_HINT_FLAG).exists());

        // Non-blocking spawn — poll briefly for the fake CLI's output.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut args = String::new();
        while std::time::Instant::now() < deadline {
            if let Ok(s) = fs::read_to_string(&out) { args = s; break }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(args.trim(), format!("hooks install --project {}", owner));
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn memory_hooks_empty_owner_dir_is_noop() {
        assert_eq!(ensure_memory_hooks(""), None);
    }

    #[test]
    fn cli_validation_rejects_impostors_and_accepts_node_cli() {
        use std::os::unix::fs::PermissionsExt;
        let owner = temp_owner_dir();

        // The C terminal binary shape: exits 0, prints unrelated noise.
        let impostor = Path::new(&owner).join("c-binary");
        fs::write(&impostor, "#!/bin/sh\necho 'connect: No such file or directory' >&2\nexit 0\n").unwrap();
        fs::set_permissions(&impostor, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!cli_supports_hooks(&impostor.to_string_lossy(), &owner));

        // The Node CLI shape: answers `hooks status` (exit code irrelevant).
        let real = Path::new(&owner).join("node-cli");
        fs::write(&real, "#!/bin/sh\necho 'Memory hooks not installed'\nexit 1\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(cli_supports_hooks(&real.to_string_lossy(), &owner));

        let _ = fs::remove_dir_all(&owner);
    }
}
