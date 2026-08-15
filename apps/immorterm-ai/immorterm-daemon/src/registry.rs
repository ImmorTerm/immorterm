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
use tracing::{error, info, warn};

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
    if cwd.is_empty() {
        return None;
    }
    let dot_git = std::path::Path::new(cwd).join(".git");
    let git_dir = match fs::metadata(&dot_git) {
        Ok(m) if m.is_dir() => dot_git,
        Ok(_) => {
            // File-mode .git → worktree pointer.
            let raw = fs::read_to_string(&dot_git).ok()?;
            let pointer = raw.trim().strip_prefix("gitdir:")?.trim();
            let p = std::path::Path::new(pointer);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                std::path::Path::new(cwd).join(p)
            }
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
    if owner_dir.is_empty() {
        return None;
    }

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
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !id.is_empty() {
            let name = v
                .get("name")
                .and_then(|x| x.as_str())
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
        warn!(
            "Failed to rename project.json into place at {:?}: {}",
            json_file, e
        );
        let _ = fs::remove_file(&tmp);
        return None;
    }
    info!(
        "Wrote project.json id={} name={} at {:?}",
        id, name, json_file
    );
    Some(ProjectIdentity { id, name })
}

/// Ensure `<owner_dir>/.gitignore` ignores `.immorterm/` runtime state while
/// keeping `project.json` tracked. No-op if not a git repo, if the rule is
/// already present, or on any IO error (best-effort — never blocks a spawn).
fn ensure_gitignore(owner_dir: &str) {
    let root = Path::new(owner_dir);
    if !root.join(".git").exists() {
        return;
    } // only touch actual repos

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
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
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

/// Read an existing project's id WITHOUT creating `project.json` or touching
/// `.gitignore`. For read-only paths (e.g. `prune`) that must never mutate the
/// workspace. `None` when `project.json` is absent/unreadable — the caller then
/// leaves the entry's `registry.d` file in place (the daemon self-cleans it on
/// real exit), which is strictly safer than materializing project state during
/// a prune.
fn read_project_id_only(owner_dir: &str) -> Option<String> {
    if owner_dir.is_empty() {
        return None;
    }
    let json_file = Path::new(owner_dir).join(".immorterm").join("project.json");
    let contents = fs::read_to_string(&json_file).ok()?;
    let v: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() { None } else { Some(id) }
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
    if owner_dir.is_empty() {
        return None;
    }
    let hooks_dir = Path::new(owner_dir).join(".immorterm").join("hooks");
    // Already wired (dir exists and is non-empty) → cheap no-op, skip the probe.
    if fs::read_dir(&hooks_dir)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
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
                    cli_path,
                    owner_dir,
                    child.id()
                );
                // Reap in a detached thread — a dropped unreaped Child zombies
                // (see the claude-detection ps leak incident).
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => warn!("Failed to spawn `{} hooks install`: {}", cli_path, e),
        }
        return None; // install handles it (or logged); no hint either way
    }

    // No CLI installed → surface the hint once per project.
    let dir = Path::new(owner_dir).join(".immorterm");
    let flag = dir.join(MEMORY_HINT_FLAG);
    if flag.exists() {
        return None;
    }
    if fs::create_dir_all(&dir).is_err() || fs::write(&flag, "").is_err() {
        return None; // unwritable project dir — don't hint every spawn
    }
    Some(MEMORY_HINT.to_string())
}

/// Probe for the `immorterm` CLI through a login shell. GUI-spawned daemons
/// inherit a minimal PATH (the known trap), so `command -v` must run under
/// the user's login environment. Never uses npx (surprise network install).
fn probe_immorterm_cli() -> Option<String> {
    let sh = if cfg!(target_os = "macos") {
        "/bin/zsh"
    } else {
        "/bin/sh"
    };
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
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

/// Display names we consider "generic" and safe to overwrite when a more
/// specific user-set name exists. Used by Registry::register to avoid
/// dropping a user's custom tab label when a concurrent writer registers
/// with a default/bootstrap value.
fn is_generic_display_name(name: &str) -> bool {
    name.is_empty() || name == "zsh" || name.starts_with("immorterm-")
}

/// Path to the shared registry file.
fn registry_path() -> PathBuf {
    let dir = socket_dir(); // ~/.immorterm/sockets/
    dir.parent().unwrap_or(&dir).join("registry.json")
}

/// `~/.immorterm/registry.d/<project_id>/`  (the per-project directory).
/// Sibling of `registry.json`; base mirrors `registry_path()`.
fn registry_d_project_dir(project_id: &str) -> PathBuf {
    let dir = socket_dir();
    dir.parent()
        .unwrap_or(&dir)
        .join("registry.d")
        .join(project_id)
}

/// Path to a session's per-session registry file:
///   `~/.immorterm/registry.d/<project_id>/<window_id>.json`
/// Both components are UUIDs / separator-free slugs used verbatim as single
/// path segments. Callers MUST never pass an empty component (see the guards
/// in `write_session_file` / `remove_session_file`).
fn registry_d_path(project_id: &str, window_id: &str) -> PathBuf {
    registry_d_project_dir(project_id).join(format!("{window_id}.json"))
}

/// Root of the per-project registry.d tree: `~/.immorterm/registry.d/`.
fn registry_d_root() -> PathBuf {
    let dir = socket_dir();
    dir.parent().unwrap_or(&dir).join("registry.d")
}

/// Per-session version-history dir, a SIBLING of registry.d (never inside it, so
/// no registry.d reader ever mistakes an old version for a live session):
///   `~/.immorterm/registry-history/<project_id>/<window_id>/<unix_ts>.json`
fn registry_history_dir(project_id: &str, window_id: &str) -> PathBuf {
    let dir = socket_dir();
    dir.parent()
        .unwrap_or(&dir)
        .join("registry-history")
        .join(project_id)
        .join(window_id)
}

/// Keep at most this many historical versions per window.
const HISTORY_KEEP: usize = 20;
/// Snapshot a window's prior version at most this often (seconds) — the file is
/// rewritten on every stats tick, so without this the history ring would churn
/// with near-identical stats-only versions.
const HISTORY_MIN_INTERVAL_SECS: u64 = 60;

/// Copy the CURRENT on-disk version of a session file into its history ring
/// BEFORE it is overwritten. This is the per-session backup layer that survives
/// a corrupt/collapsed global registry — the whole-file backups all held the
/// already-collapsed state in the 2026-08 incident, so a single racing writer
/// wiped every restore point. A per-session, single-writer ring can't be
/// cross-contaminated. Rate-limited + capped; best-effort (never fails a write).
fn snapshot_history(current: &std::path::Path, project_id: &str, window_id: &str) {
    if !current.exists() {
        return; // nothing to snapshot on the first write
    }
    let hist_dir = registry_history_dir(project_id, window_id);
    // Rate-limit: skip if the newest snapshot is younger than the interval.
    if let Ok(rd) = fs::read_dir(&hist_dir) {
        let newest = rd
            .flatten()
            .filter_map(|e| e.metadata().ok().and_then(|m| m.modified().ok()))
            .max();
        if let Some(t) = newest
            && let Ok(age) = t.elapsed()
            && age.as_secs() < HISTORY_MIN_INTERVAL_SECS
        {
            return;
        }
    }
    if fs::create_dir_all(&hist_dir).is_err() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = fs::copy(current, hist_dir.join(format!("{ts}.json")));
    prune_history(&hist_dir);
}

/// Keep only the newest `HISTORY_KEEP` snapshots in a window's history ring.
fn prune_history(hist_dir: &std::path::Path) {
    let Ok(rd) = fs::read_dir(hist_dir) else {
        return;
    };
    let mut files: Vec<_> = rd
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| {
            e.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| (e.path(), t))
        })
        .collect();
    if files.len() <= HISTORY_KEEP {
        return;
    }
    files.sort_by_key(|(_, t)| *t);
    let drop = files.len() - HISTORY_KEEP;
    for (p, _) in &files[..drop] {
        let _ = fs::remove_file(p);
    }
}

/// Path to the advisory write lock guarding `registry.json`.
fn registry_lock_path() -> PathBuf {
    let dir = socket_dir();
    dir.parent().unwrap_or(&dir).join("registry.lock")
}

/// Advisory lock serializing registry read-modify-writes across processes,
/// mirroring `mcp.rs`'s `lock_plan_dir()`. Held for the duration of a `save()`
/// and released on drop.
///
/// Returns `None` if the lock can't be taken (unwritable dir, or the flock call
/// itself fails). Degrading to the old unlocked behaviour is deliberate: a
/// session that can't lock should still register — losing the race is bad, but
/// refusing to appear at all is worse.
/// ponytail: blocking exclusive lock, no timeout — held only across one 15KB
/// read-merge-write, so contention is microseconds. Add a try_lock + backoff
/// only if a wedged holder is ever observed in the wild.
fn lock_registry() -> Option<nix::fcntl::Flock<std::fs::File>> {
    let path = registry_lock_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .ok()?;
    match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive) {
        Ok(guard) => Some(guard),
        Err((_, e)) => {
            warn!("registry lock unavailable ({e}) — writing unserialized");
            None
        }
    }
}

/// Path to the backup directory.
fn backup_dir() -> PathBuf {
    let dir = socket_dir();
    dir.parent().unwrap_or(&dir).join("registry-backups")
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
        .and_then(|s| parse_registry(&s).ok())
        .map(|r| r.sessions.len())
        .unwrap_or(0);

    if let Some((latest_path, _)) = collect_backup_files(&dir).last() {
        let latest_count = fs::read_to_string(latest_path)
            .ok()
            .and_then(|s| parse_registry(&s).ok())
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
fn collect_backup_files(
    root: &std::path::Path,
) -> Vec<(std::path::PathBuf, std::time::SystemTime)> {
    let mut files = Vec::new();
    let Ok(top) = fs::read_dir(root) else {
        return files;
    };
    for entry in top.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(inner) = fs::read_dir(&path) {
                for inner_entry in inner.flatten() {
                    let inner_path = inner_entry.path();
                    let Some(name) = inner_path.file_name().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    if !(name.starts_with("registry.") && name.ends_with(".json")) {
                        continue;
                    }
                    if let Ok(meta) = fs::metadata(&inner_path)
                        && let Ok(mtime) = meta.modified()
                    {
                        files.push((inner_path, mtime));
                    }
                }
            }
        } else if let Some(name) = path.file_name().and_then(|s| s.to_str())
            && name.starts_with("registry.")
            && name.ends_with(".json")
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
    if files.len() <= MAX_BACKUPS {
        return;
    }

    let to_delete = files.len() - MAX_BACKUPS;
    for (path, _) in &files[..to_delete] {
        let _ = fs::remove_file(path);
    }
    info!(
        "Pruned {} old registry backups (kept {})",
        to_delete, MAX_BACKUPS
    );

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
        let Ok(contents) = fs::read_to_string(backup_path) else {
            continue;
        };
        let Ok(registry) = parse_registry(&contents) else {
            continue;
        };
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
        let Ok(contents) = fs::read_to_string(backup_path) else {
            continue;
        };
        let Ok(registry) = parse_registry(&contents) else {
            continue;
        };
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
pub struct AiStatsEntry {
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
    /// AI session ID (if associated) — the vendor's own session/thread uuid.
    /// Which vendor it belongs to is `tool`; this holds a Codex thread id just
    /// as readily as a Claude session id.
    ///
    /// The `claude_session_id` alias keeps every existing registry.json
    /// deserializing; entries are rewritten under the new name on first save.
    #[serde(alias = "claude_session_id", skip_serializing_if = "Option::is_none")]
    pub ai_session_id: Option<String>,
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
    /// Vendor transcript path — Claude's `~/.claude/projects/**.jsonl` or
    /// Codex's `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`.
    #[serde(
        default,
        alias = "claude_transcript_path",
        skip_serializing_if = "Option::is_none"
    )]
    pub ai_transcript_path: Option<String>,
    /// Live AI stats (model, context %, cost) — written by the extension for
    /// Claude, by the daemon's Codex rollout reader for Codex.
    #[serde(
        default,
        alias = "claude_stats",
        skip_serializing_if = "Option::is_none"
    )]
    pub ai_stats: Option<AiStatsEntry>,

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
    /// `tool` / `ai_session_id` are overwritten on each link. Daemon
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

    /// Unix epoch milliseconds of the latest PTY I/O activity. Persisted so
    /// external project directories expose the same underlying activity used
    /// by the status bar's `Last:` display, even after UI reconnects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<u64>,
    /// Unix epoch milliseconds of the daemon's latest registry heartbeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_at: Option<u64>,

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
    /// Snapshot of `sessions` exactly as it was read from disk by `load()`.
    /// Never serialized — it exists so `save()` can tell "I changed this entry"
    /// apart from "I never knew about this entry", which is what makes the
    /// three-way merge in `save()` possible. See the comment there.
    #[serde(skip)]
    baseline: Vec<RegistryEntry>,
}

/// Serialize a registry to `Value` and mirror each vendor-neutral key back
/// under its legacy name (T20 transition).
///
/// Deployment is NOT atomic: the daemon binary, the VS Code extension and any
/// already-running daemon process update independently. A pre-T20 daemon has
/// no alias for `ai_session_id`, so if it read a file written only under the
/// new names it would deserialize them to `None` and drop them on its next
/// write — silent loss of the session id on every entry that had one.
///
/// Writing BOTH names makes every version mix safe: old code finds
/// `claude_session_id`, new code prefers `ai_session_id`, and neither can
/// destroy the other's view. Costs three duplicated keys per entry.
///
/// REMOVE once no pre-T20 daemon can still be running (a release cycle plus a
/// machine reboot); the reader-side aliases stay forever for old files.
/// Single source of truth for the vendor-neutral → legacy key mirror list.
const MIRRORED: &[(&str, &str)] = &[
    ("ai_session_id", "claude_session_id"),
    ("ai_transcript_path", "claude_transcript_path"),
    ("ai_stats", "claude_stats"),
];

/// Mirror each `MIRRORED` key into one already-serialized entry object, in place.
fn mirror_legacy_keys(obj: &mut serde_json::Map<String, serde_json::Value>) {
    for (modern, legacy) in MIRRORED {
        if let Some(v) = obj.get(*modern).cloned() {
            obj.insert((*legacy).to_string(), v);
        }
    }
}

/// Serialize ONE entry to a `Value` with the three legacy keys mirrored in —
/// the per-entry variant of [`dual_write_legacy_keys`], used by the per-session
/// `registry.d` writer (which stores a bare entry, not a `Registry`).
fn dual_write_entry_legacy_keys(entry: &RegistryEntry) -> std::io::Result<serde_json::Value> {
    let mut v = serde_json::to_value(entry).map_err(std::io::Error::other)?;
    if let Some(obj) = v.as_object_mut() {
        mirror_legacy_keys(obj);
    }
    Ok(v)
}

fn dual_write_legacy_keys<T: Serialize>(value: &T) -> std::io::Result<serde_json::Value> {
    let mut root = serde_json::to_value(value).map_err(std::io::Error::other)?;
    if let Some(sessions) = root.get_mut("sessions").and_then(|s| s.as_array_mut()) {
        for entry in sessions.iter_mut() {
            if let Some(obj) = entry.as_object_mut() {
                mirror_legacy_keys(obj);
            }
        }
    }
    Ok(root)
}

/// Inverse of [`mirror_legacy_keys`]: collapse each mirrored pair to the MODERN
/// key in one entry object, in place. If both keys are present (a dual-written
/// entry) the legacy duplicate is dropped; if only the legacy key is present (an
/// old file) it is promoted to the modern name.
///
/// This MUST run before deserializing into `RegistryEntry`: `ai_session_id`
/// carries `#[serde(alias = "claude_session_id")]`, and serde rejects an object
/// that has BOTH the field and its alias as a `duplicate field` error. Without
/// this step `from_str::<Registry>` fails on our own `dual_write_legacy_keys`
/// output, `load()` falls through to an (also dual-written) backup that fails the
/// same way, and returns an EMPTY registry — a self-inflicted wipe. Mirrors the
/// TS side's `normalizeRegistryKeys`.
fn unmirror_legacy_keys(obj: &mut serde_json::Map<String, serde_json::Value>) {
    for (modern, legacy) in MIRRORED {
        let has_modern = obj.contains_key(*modern);
        // remove() runs regardless (drops the duplicate when modern is present);
        // only promote the legacy value when the modern key is absent.
        if let Some(legacy_val) = obj.remove(*legacy)
            && !has_modern
        {
            obj.insert((*modern).to_string(), legacy_val);
        }
    }
}

/// Parse a whole `registry.json` document, normalizing legacy keys first so that
/// dual-written entries deserialize. Use this everywhere instead of
/// `serde_json::from_str::<Registry>` — see [`unmirror_legacy_keys`].
fn parse_registry(contents: &str) -> Result<Registry, serde_json::Error> {
    let mut root: serde_json::Value = serde_json::from_str(contents)?;
    if let Some(sessions) = root.get_mut("sessions").and_then(|s| s.as_array_mut()) {
        for entry in sessions.iter_mut() {
            if let Some(obj) = entry.as_object_mut() {
                unmirror_legacy_keys(obj);
            }
        }
    }
    serde_json::from_value(root)
}

/// Parse ONE bare `registry.d/<project>/<window>.json` entry, normalizing legacy
/// keys first (the per-session files are dual-written too). Phase-2 readers of
/// `registry.d` MUST go through here, not raw `from_str::<RegistryEntry>`.
fn parse_session_entry(contents: &str) -> Result<RegistryEntry, serde_json::Error> {
    let mut v: serde_json::Value = serde_json::from_str(contents)?;
    if let Some(obj) = v.as_object_mut() {
        unmirror_legacy_keys(obj);
    }
    serde_json::from_value(v)
}

/// Resolve a session's `project_dir` by NAME across the UNIONED view: the global
/// `registry.json` first, then every `registry.d/<project>/<window>.json`.
///
/// The global file can lag or omit a live session that exists only in registry.d
/// — a daemon writes its per-session file at spawn, possibly before/without a
/// global entry, or after `save()`'s shrinkage guard refused a global write.
/// Security-sensitive readers (the cross-project workshop guard in `mcp.rs`) MUST
/// see those sessions; resolving from `registry.json` alone made the guard fail
/// OPEN for registry.d-only targets.
pub fn project_dir_for_session_name(name: &str) -> Option<String> {
    // Global first — one cheap read covers the common case.
    if let Some(dir) = Registry::load()
        .sessions
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.project_dir)
        .filter(|d| !d.is_empty())
    {
        return Some(dir);
    }
    // Fallback: scan registry.d for a per-session file carrying this name.
    let root = registry_d_root();
    for proj in fs::read_dir(&root).ok()?.flatten() {
        let p = proj.path();
        if !p.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(&p) else {
            continue;
        };
        for f in files.flatten() {
            let fp = f.path();
            if fp.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&fp) else {
                continue;
            };
            let Ok(entry) = parse_session_entry(&content) else {
                continue;
            };
            if entry.name == name && !entry.project_dir.is_empty() {
                return Some(entry.project_dir);
            }
        }
    }
    None
}

impl Registry {
    /// Load the registry from disk (or return empty if doesn't exist).
    /// On parse failure, attempts recovery from the latest backup.
    pub fn load() -> Self {
        let mut loaded = Self::load_raw();
        loaded.baseline = loaded.sessions.clone();
        loaded
    }

    /// `load()` without stamping the baseline — used by `save()`'s merge to
    /// re-read the current on-disk state, which must NOT become our baseline.
    fn load_raw() -> Self {
        let path = registry_path();
        match fs::read_to_string(&path) {
            Ok(contents) => {
                match parse_registry(&contents) {
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

        // Serialize the whole read-modify-write against every other writer
        // (~90 session daemons plus one VS Code extension host per window).
        // Without this, `load()` → mutate → `save()` is not atomic across
        // processes: any entry another writer added after our load is erased,
        // because our in-memory snapshot never contained it. Measured before
        // this landed: 87 live sessions, 28 registered, and the entry count
        // dropping on its own while nothing was happening.
        //
        // The lock is taken on WRITES only. Readers stay lock-free — the
        // tmp+rename below is atomic, so a reader always sees a whole file.
        let _lock = lock_registry();

        // Three-way merge against the CURRENT disk state, so we only impose
        // the entries we actually touched:
        //   • in baseline, gone from ours   → we deleted it       → drop
        //   • ours differs from baseline    → we changed it       → write ours
        //   • unchanged since baseline      → someone else owns it → keep disk's
        //   • on disk, never in our baseline → arrived after we loaded → keep
        // Holding the lock is what makes this re-read meaningful: without it
        // another writer can land between the read and the rename.
        let merged = self.merge_over_disk();

        // Shrinkage guard
        let new_count = merged.len();
        if let Ok(disk_data) = fs::read_to_string(&path)
            && let Ok(disk_reg) = parse_registry(&disk_data)
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

        let to_write = Registry {
            sessions: merged,
            baseline: Vec::new(),
        };
        let tmp = path.with_extension("tmp");
        let json = serde_json::to_string_pretty(&dual_write_legacy_keys(&to_write)?)
            .map_err(std::io::Error::other)?;
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Identity of an entry for merge purposes. `register()` dedups by name OR
    /// window_id, so name is the primary key with window_id as the fallback for
    /// entries that carry no name.
    fn merge_key(e: &RegistryEntry) -> String {
        if !e.name.is_empty() {
            e.name.clone()
        } else {
            e.window_id.clone()
        }
    }

    /// Compare by serialized form — `RegistryEntry` nests several types that
    /// don't derive `PartialEq`, and entries are small enough that this is
    /// cheaper than threading derives through all of them.
    fn entry_repr(e: &RegistryEntry) -> String {
        serde_json::to_string(e).unwrap_or_default()
    }

    /// Apply only OUR changes on top of whatever is on disk right now.
    /// Caller must hold the registry lock.
    fn merge_over_disk(&self) -> Vec<RegistryEntry> {
        self.merge_with(Self::load_raw().sessions)
    }

    /// The merge itself, with the on-disk state passed in so it can be tested
    /// without touching the filesystem.
    fn merge_with(&self, disk: Vec<RegistryEntry>) -> Vec<RegistryEntry> {
        use std::collections::HashMap;
        let base: HashMap<String, String> = self
            .baseline
            .iter()
            .map(|e| (Self::merge_key(e), Self::entry_repr(e)))
            .collect();
        let mine: HashMap<String, &RegistryEntry> = self
            .sessions
            .iter()
            .map(|e| (Self::merge_key(e), e))
            .collect();

        let mut out: Vec<RegistryEntry> = Vec::new();
        for disk_entry in disk {
            let key = Self::merge_key(&disk_entry);
            match mine.get(&key) {
                // We still have it: ours wins only if we actually changed it.
                Some(ours) => {
                    let changed = base
                        .get(&key)
                        .map(|b| *b != Self::entry_repr(ours))
                        .unwrap_or(true);
                    out.push(if changed { (*ours).clone() } else { disk_entry });
                }
                // Missing from ours: a deliberate delete only if we had it at
                // load time. Otherwise it arrived after us and must survive.
                None => {
                    if !base.contains_key(&key) {
                        out.push(disk_entry);
                    }
                }
            }
        }
        // Entries we added that aren't on disk yet.
        let seen: std::collections::HashSet<String> = out.iter().map(Self::merge_key).collect();
        for e in &self.sessions {
            if !seen.contains(&Self::merge_key(e)) {
                out.push(e.clone());
            }
        }
        out
    }

    /// Prune dead sessions (process no longer alive).
    /// ROOT CAUSE FIX #3: Refuses to prune if >80% of entries would be removed
    /// (indicates a laptop restart, not actual dead sessions).
    pub fn prune(&mut self) {
        let total = self.sessions.len();
        if total == 0 {
            return;
        }

        let alive_count = self
            .sessions
            .iter()
            .filter(|e| is_process_alive(e.pid))
            .count();
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

        let removed = self.dead_session_files();
        self.sessions.retain(|entry| is_process_alive(entry.pid));
        for (project_id, window_id) in removed {
            remove_session_file(&project_id, &window_id);
        }
    }

    /// Force-prune dead sessions regardless of safety threshold.
    /// Only use via explicit `--force` flag.
    pub fn prune_force(&mut self) {
        let before = self.sessions.len();
        let removed = self.dead_session_files();
        self.sessions.retain(|entry| is_process_alive(entry.pid));
        for (project_id, window_id) in removed {
            remove_session_file(&project_id, &window_id);
        }
        let pruned = before - self.sessions.len();
        if pruned > 0 {
            warn!("Force-pruned {}/{} dead sessions", pruned, before);
        }
    }

    /// (project_id, window_id) of every entry whose PID is dead — the set
    /// `prune`/`prune_force` are about to drop, so their per-session
    /// `registry.d` files can be removed to match.
    fn dead_session_files(&self) -> Vec<(String, String)> {
        self.sessions
            .iter()
            .filter(|e| !is_process_alive(e.pid) && !e.window_id.is_empty())
            // READ-ONLY: prune must not create project.json / touch .gitignore.
            .filter_map(|e| project_id_for_entry_readonly(e).map(|pid| (pid, e.window_id.clone())))
            .collect()
    }

    /// Register a new session, merging forward any fields that existed on a prior
    /// matching entry but aren't set on the incoming entry.
    ///
    /// **Why the merge exists**: this registry is a multi-writer file. The daemon's
    /// `register_session()` knows pid, name, window_id, display_name,
    /// ai_session_id (from env), title_locked, shell, project_dir,
    /// session_type, created_at, structured_log_dir. The extension writes *other*
    /// fields asynchronously: `theme`, `ai_transcript_path`, `ai_stats`,
    /// `session_status`, `shelved_at`, and sometimes a more recent
    /// `ai_session_id` (via claude-sync.ts). Without this merge, every daemon
    /// respawn silently wipes those extension-managed fields — which is exactly
    /// what caused a restore failure we hit (ai_session_id race-wiped
    /// from registry, breaking auto-resume on reboot).
    ///
    /// The dedup then runs as before (name OR window_id match → replace) so there's
    /// still exactly one live entry per identity.
    pub fn register(&mut self, mut entry: RegistryEntry) {
        if let Some(existing) = self.sessions.iter().find(|e| {
            e.name == entry.name || (!entry.window_id.is_empty() && e.window_id == entry.window_id)
        }) {
            if entry.ai_session_id.is_none() {
                entry.ai_session_id = existing.ai_session_id.clone();
            }
            if entry.theme.is_none() {
                entry.theme = existing.theme.clone();
            }
            if entry.ai_transcript_path.is_none() {
                entry.ai_transcript_path = existing.ai_transcript_path.clone();
            }
            if entry.ai_stats.is_none() {
                entry.ai_stats = existing.ai_stats.clone();
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
            e.name != entry.name && (entry.window_id.is_empty() || e.window_id != entry.window_id)
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
    /// Returns false when no entry matches `window_id` — i.e. the resume id was
    /// dropped on the floor.
    ///
    /// This used to be a silent no-op, which is why not one registry entry in
    /// 200 backup snapshots ever carried a resume id: all four writers of the
    /// AI session id call this, and a session whose entry had been clobbered by
    /// the (now fixed) write race had nothing to write into. The id was
    /// discovered correctly every time and then discarded without a word.
    /// Self-healing re-registers a missing entry within ~10s and carries the id
    /// with it, so a `false` here is recoverable — but it must be visible.
    #[must_use = "a dropped resume id means that session can never be resumed"]
    pub fn update_claude_session(&mut self, window_id: &str, claude_id: &str) -> bool {
        match self.sessions.iter_mut().find(|e| e.window_id == window_id) {
            Some(entry) => {
                entry.ai_session_id = Some(claude_id.to_string());
                true
            }
            None => false,
        }
    }

    /// Stamp the daemon-detected AI tool onto this window's entry so
    /// registry.d-only readers can label the vendor (codex vs claude-code)
    /// before any hub session-link lands. Returns false when no entry matches.
    /// Callers MUST pass a known tool only (guard on `detected_tool.is_some()`):
    /// a `None` detection must never blank a hub-set value.
    pub fn update_tool(&mut self, window_id: &str, tool: &str) -> bool {
        match self.sessions.iter_mut().find(|e| e.window_id == window_id) {
            Some(entry) => {
                entry.tool = Some(tool.to_string());
                true
            }
            None => false,
        }
    }

    /// Build the persisted stats snapshot from a live `ClaudeTracker`. Plain
    /// field copies — the result is `Send + 'static`, so the background persist
    /// worker can be handed one without touching the non-`Send` tracker.
    pub fn build_ai_stats_entry(claude: &crate::claude::ClaudeTracker) -> AiStatsEntry {
        AiStatsEntry {
            pid: claude.claude_pid,
            rss_kb: claude.rss_kb,
            cpu_percent: claude.cpu_percent as f64,
            start_time: claude.start_time.map(|t| t.elapsed().as_secs()),
            runtime_secs: claude.runtime_secs(),
            model: if claude.api_stats.model.is_empty() {
                None
            } else {
                Some(claude.api_stats.model.clone())
            },
            cost_usd: if claude.api_stats.cost_usd > 0.0 {
                Some(claude.api_stats.cost_usd)
            } else {
                None
            },
            context_pct: if claude.api_stats.context_pct > 0.0 {
                Some(claude.api_stats.context_pct)
            } else {
                None
            },
        }
    }

    /// Update Claude stats (process + API) for a window.
    pub fn update_ai_stats(&mut self, window_id: &str, claude: &crate::claude::ClaudeTracker) {
        if let Some(entry) = self.sessions.iter_mut().find(|e| e.window_id == window_id) {
            entry.ai_stats = Some(Self::build_ai_stats_entry(claude));
        }
    }

    /// Persist one window's entry to all destinations: the global registry
    /// (`save()` — keeps its lock/merge/backup/shrinkage guard), `session.json`,
    /// and the per-session `registry.d` file. Call after ANY mutation of that
    /// window's entry so the three copies never drift (e.g. an `ai_session_id`
    /// discovered after spawn via OSC 1337 / env backfill). No matching entry
    /// in memory → only the global `save()` runs (nothing to shadow).
    pub fn persist_session(&mut self, window_id: &str) -> std::io::Result<()> {
        self.save()?;
        if let Some(entry) = self
            .sessions
            .iter()
            .find(|e| e.window_id == window_id)
            .cloned()
        {
            write_session_json(&entry);
            if let Err(e) = write_session_file(&entry) {
                warn!(
                    "Failed to write registry.d for window_id={}: {}",
                    window_id, e
                );
            }
        }
        Ok(())
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
                if let Some(ref claude_id) = e.ai_session_id {
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
pub fn register_session(name: &str, shell: &str, logfile: Option<&str>) {
    let window_id = std::env::var("IMMORTERM_WINDOW_ID")
        .or_else(|_| std::env::var("SCREEN_WINDOW_ID"))
        .unwrap_or_default();
    let display_name = std::env::var("IMMORTERM_DISPLAY_NAME")
        .or_else(|_| std::env::var("SCREEN_WINDOW_NAME"))
        .unwrap_or_else(|_| name.to_string());
    let project_dir = std::env::var("SCREEN_PROJECT_DIR").unwrap_or_default();

    // Resolve owner_project_dir + worktree from project_dir via git.
    // Worktree-spawned daemons end up with owner_project_dir = trunk, worktree = spawn dir.
    let owner = resolve_owner_project(&project_dir);
    let owner_identity = read_or_create_project(&owner.owner_dir);
    let owner_project_id = owner_identity.as_ref().map(|p| p.id.clone());
    let owner_project_name = owner_identity.as_ref().map(|p| p.name.clone());

    let ai_session_id = std::env::var("IMMORTERM_CLAUDE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty());
    let title_locked = std::env::var("IMMORTERM_TITLE_LOCKED")
        .map(|v| v == "1")
        .unwrap_or(false);

    let session_type = std::env::var("IMMORTERM_SESSION_TYPE")
        .ok()
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
        ai_session_id,
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
        ai_transcript_path: None,
        ai_stats: None,
        tool: None,
        tool_history: Vec::new(),
        session_status: None,
        shelved_at: None,
        structured_log_dir,
        needs_attention: false,
        is_working: false,
        last_activity_at: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        ),
        heartbeat_at: Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        ),
        owner_project_dir: if owner.owner_dir.is_empty() {
            None
        } else {
            Some(owner.owner_dir)
        },
        owner_project_id,
        owner_project_name,
        worktree: owner.worktree,
    };

    // Key to find the entry back after register()'s forward-merge runs.
    let window_id = entry.window_id.clone();
    let name = entry.name.clone();

    let mut registry = Registry::load();
    // Do NOT prune here — dead entries are the restore state after laptop restart.
    // Pruning happens explicitly via `immorterm session cleanup`.
    registry.register(entry);
    if let Err(e) = registry.save() {
        error!("Failed to register session in registry: {}", e);
    }

    // Write the side files (session.json + registry.d) from the POST-merge
    // entry, not the raw incoming one: a respawn with ai_session_id=None must
    // carry forward the existing id (register() did that in-memory), never blank
    // it in any of the three destinations. See the field-stripping fix.
    if let Some(merged) = registry
        .sessions
        .iter()
        .find(|e| e.name == name || (!window_id.is_empty() && e.window_id == window_id))
        .cloned()
    {
        write_session_json(&merged);
        if let Err(e) = write_session_file(&merged) {
            warn!(
                "Failed to write registry.d at spawn for window_id={}: {}",
                window_id, e
            );
        }
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

/// Resolve the `project_id` used to key this entry's `registry.d` file.
/// Prefers the entry's own `owner_project_id`; falls back to reading (or
/// creating) `project.json` under `owner_project_dir`, then `project_dir`.
/// `None` only when no directory yields an identity (unwritable / empty).
fn project_id_for_entry(entry: &RegistryEntry) -> Option<String> {
    project_id_for_entry_with(entry, |d| read_or_create_project(d).map(|p| p.id))
}

/// Read-only variant: resolves the same id but NEVER creates `project.json` /
/// touches `.gitignore`. Used by `prune`/`dead_session_files`, which must not
/// write to any workspace while reaping dead entries.
fn project_id_for_entry_readonly(entry: &RegistryEntry) -> Option<String> {
    project_id_for_entry_with(entry, read_project_id_only)
}

/// Shared resolver: the entry's own `owner_project_id`, else the id for its
/// owner/project dir via `resolve_dir` (create-or-read vs read-only).
fn project_id_for_entry_with(
    entry: &RegistryEntry,
    resolve_dir: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(id) = entry.owner_project_id.as_deref().filter(|s| !s.is_empty()) {
        return Some(id.to_string());
    }
    let dir = entry
        .owner_project_dir
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(Some(entry.project_dir.as_str()))
        .filter(|s| !s.is_empty())?;
    resolve_dir(dir)
}

/// Atomically write a session's per-session `registry.d` file:
///   mkdir -p parent → write tmp in the SAME dir → fsync → rename over dest.
///
/// The file is ONE `RegistryEntry` (not a `Registry`), with the three legacy
/// keys mirrored in — identical serde surface to a row in `registry.json`.
///
/// Single-writer: each `registry.d/<project_id>/<window_id>.json` is written by
/// exactly one process for its whole lifetime — the daemon that owns that
/// `window_id`. So no flock, no read-merge: the writer just overwrites its own
/// previous version atomically; the tmp+fsync+rename makes every read see a
/// whole file. This holds ONLY while `window_id` is globally unique per live
/// daemon (it is — VS Code terminal identity, `register()` dedups on it).
// ponytail: no lock, single-writer invariant. If two daemons ever share a
// window_id, this needs the same flock the global registry has.
// ponytail: dual-writes claude_* mirror per spec, but a file carrying BOTH
// ai_session_id and claude_session_id will NOT deserialize into RegistryEntry
// (the #[serde(alias)] derive rejects it as a duplicate field — same as the
// global registry.json). Phase 1 only diffs registry.d as JSON, so this is
// inert now; before Phase 2 makes it a read source, either strip the mirror on
// read or drop the mirror here (no old process ever reads registry.d, so the
// mirror protects nobody). See the round-trip test's strip_legacy().
pub fn write_session_file(entry: &RegistryEntry) -> std::io::Result<()> {
    use std::io::Write;
    let Some(project_id) = project_id_for_entry(entry) else {
        return Ok(()); // no project identity → global registry still has it
    };
    if entry.window_id.is_empty() {
        return Ok(()); // never key a file on an empty component
    }

    let path = registry_d_path(&project_id, &entry.window_id);
    let parent = path.parent().expect("registry_d_path always has a parent");
    fs::create_dir_all(parent)?;

    // Back up the prior version into the per-session history ring before we
    // overwrite it (rate-limited; best-effort — never blocks the write).
    snapshot_history(&path, &project_id, &entry.window_id);

    let value = dual_write_entry_legacy_keys(entry)?;
    let json = serde_json::to_string_pretty(&value).map_err(std::io::Error::other)?;

    // tmp sibling in the same dir so the rename is same-filesystem & atomic.
    let tmp = path.with_extension("json.tmp");
    let write = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())
        // NO fsync: this file is rewritten constantly and is backed by both the
        // global registry.json and the memory DB, so per-write durability buys
        // nothing. sync_all() here ran on the daemon's OSC/claude-stats hot path
        // and stalled PTY relay (typed input not shown until the flush). The
        // atomic tmp+rename still guarantees readers never see a partial file.
    })();
    if let Err(e) = write.and_then(|()| fs::rename(&tmp, &path)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Remove a session's per-session `registry.d` file. Best-effort and
/// idempotent (`ENOENT` is success). Prunes the `<project_id>/` dir if it
/// becomes empty. A failure to delete is non-fatal in Phase 1: the global
/// registry stays authoritative and a stale side file is inert until Phase 2.
pub fn remove_session_file(project_id: &str, window_id: &str) {
    if project_id.is_empty() || window_id.is_empty() {
        return;
    }
    let path = registry_d_path(project_id, window_id);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("Failed to remove registry.d file {:?}: {}", path, e),
    }
    // Prune the now-possibly-empty project dir.
    let dir = registry_d_project_dir(project_id);
    if let Ok(mut rd) = fs::read_dir(&dir)
        && rd.next().is_none()
    {
        let _ = fs::remove_dir(&dir);
    }
    // Genuine removal → drop this window's history ring too (the memory DB is
    // the durable record for gone sessions; the ring is for live rollback).
    let _ = fs::remove_dir_all(registry_history_dir(project_id, window_id));
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
pub fn find_existing_session_dir(
    base_dir: &std::path::Path,
    suffix: &str,
) -> Option<std::path::PathBuf> {
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
/// `ai_session_id` when OSC 1337 was never emitted (e.g. older Claude
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
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.ends_with(".env") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Line-oriented exact match on IMMORTERM_ID=<wid>
        let matches = contents
            .lines()
            .any(|line| line.trim() == needle || line.trim().starts_with(&format!("{} ", needle)));
        if !matches {
            continue;
        }
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
    // Capture the project+window key BEFORE removing so we can delete the
    // per-session registry.d file too (additive: failure here is non-fatal).
    let side_key = registry
        .sessions
        .iter()
        .find(|e| e.pid == pid)
        .and_then(|e| project_id_for_entry(e).map(|pid| (pid, e.window_id.clone())));
    registry.deregister(pid);
    if let Err(e) = registry.save() {
        error!("Failed to deregister session from registry: {}", e);
    }
    if let Some((project_id, window_id)) = side_key {
        remove_session_file(&project_id, &window_id);
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    fn entry(name: &str, display: &str) -> RegistryEntry {
        serde_json::from_value(serde_json::json!({
            "pid": 1, "name": name, "window_id": name, "display_name": display,
            "project_dir": "/tmp/p", "shell": "/bin/zsh", "created_at": 0,
        }))
        .expect("minimal entry should deserialize")
    }

    fn reg(baseline: Vec<RegistryEntry>, sessions: Vec<RegistryEntry>) -> Registry {
        Registry { sessions, baseline }
    }

    fn names(v: &[RegistryEntry]) -> Vec<&str> {
        v.iter().map(|e| e.name.as_str()).collect()
    }

    /// The whole point of the merge: a writer must impose only its own changes,
    /// never its whole snapshot. Each case here is a way the old whole-file
    /// overwrite lost data.
    #[test]
    fn merge_imposes_only_our_own_changes() {
        // We loaded [a], then registered b. Meanwhile another daemon added c.
        let ours = reg(
            vec![entry("a", "A")],
            vec![entry("a", "A"), entry("b", "B")],
        );
        let disk = vec![entry("a", "A"), entry("c", "C")];
        let out = ours.merge_with(disk);
        assert!(
            names(&out).contains(&"c"),
            "an entry added after our load must survive"
        );
        assert!(names(&out).contains(&"b"), "our new entry must land");
        assert!(names(&out).contains(&"a"), "untouched entries stay");

        // We changed a's display name → ours wins.
        let ours = reg(vec![entry("a", "A")], vec![entry("a", "RENAMED")]);
        let out = ours.merge_with(vec![entry("a", "A")]);
        assert_eq!(out[0].display_name, "RENAMED");

        // We did NOT touch a, but another process did → keep theirs.
        let ours = reg(vec![entry("a", "A")], vec![entry("a", "A")]);
        let out = ours.merge_with(vec![entry("a", "THEIRS")]);
        assert_eq!(
            out[0].display_name, "THEIRS",
            "unchanged entries must not clobber"
        );

        // We deliberately removed a (it was in our baseline) → it goes.
        let ours = reg(
            vec![entry("a", "A"), entry("b", "B")],
            vec![entry("b", "B")],
        );
        let out = ours.merge_with(vec![entry("a", "A"), entry("b", "B")]);
        assert_eq!(names(&out), vec!["b"], "a delete we made is still a delete");
    }

    /// A resume id written against a window_id that has no entry is a session
    /// that can never be resumed. It must report the failure, not swallow it —
    /// this is why 200 backup snapshots contained zero resume ids.
    #[test]
    fn update_claude_session_reports_a_missing_entry() {
        let mut reg = reg(vec![], vec![entry("a", "A")]);
        assert!(
            reg.update_claude_session("a", "uuid-1"),
            "entry present → written"
        );
        assert_eq!(reg.sessions[0].ai_session_id.as_deref(), Some("uuid-1"));
        assert!(
            !reg.update_claude_session("ghost", "uuid-2"),
            "no entry for that window_id → must report false, not silently drop"
        );
    }

    /// #14: registry.d-only readers default to claude-code when `tool` is absent,
    /// so the daemon must stamp its detected vendor. Mirrors update_claude_session.
    #[test]
    fn update_tool_stamps_and_reports_missing() {
        let mut reg = reg(vec![], vec![entry("a", "A")]);
        assert!(reg.update_tool("a", "codex"), "entry present → written");
        assert_eq!(reg.sessions[0].tool.as_deref(), Some("codex"));
        assert!(
            !reg.update_tool("ghost", "codex"),
            "no entry → false, not a panic"
        );
    }

    /// The exact production scenario, in miniature: many daemons registering
    /// concurrently, each holding a snapshot from before the others existed.
    #[test]
    fn concurrent_registrations_all_survive() {
        let disk_start = vec![entry("existing", "E")];
        let mut disk = disk_start.clone();
        for name in ["d1", "d2", "d3", "d4", "d5"] {
            // Every daemon loaded the SAME stale snapshot, then registered itself.
            let mut mine = disk_start.clone();
            mine.push(entry(name, name));
            disk = reg(disk_start.clone(), mine).merge_with(disk);
        }
        let got = names(&disk);
        for name in ["existing", "d1", "d2", "d3", "d4", "d5"] {
            assert!(got.contains(&name), "{name} was lost; got {got:?}");
        }
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
        assert!(
            dir.join("project-id").exists(),
            "legacy file kept for grace period"
        );
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn reads_existing_project_json_verbatim() {
        let owner = temp_owner_dir();
        let dir = Path::new(&owner).join(".immorterm");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("project.json"),
            r#"{"id":"abc-123","name":"My Project"}"#,
        )
        .unwrap();

        let id = read_or_create_project(&owner).expect("should read");
        assert_eq!(id.id, "abc-123");
        assert_eq!(id.name, "My Project");
        let _ = fs::remove_dir_all(&owner);
    }

    /// #5 backups: the per-session history ring keeps exactly the newest
    /// HISTORY_KEEP snapshots so it can't grow unbounded on a hot-writing window.
    #[test]
    fn prune_history_keeps_newest_n() {
        let dir = std::env::temp_dir().join(format!(
            "imterm-hist-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        for i in 0..(HISTORY_KEEP + 5) {
            fs::write(dir.join(format!("{i}.json")), b"{}").unwrap();
        }
        prune_history(&dir);
        let remaining = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .count();
        assert_eq!(
            remaining, HISTORY_KEEP,
            "prune keeps exactly HISTORY_KEEP newest"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// #9: the prune-path resolver must NEVER create project.json or .gitignore.
    #[test]
    fn read_project_id_only_never_writes() {
        let owner = temp_owner_dir();
        // No project.json yet → None, and nothing gets created.
        assert_eq!(read_project_id_only(&owner), None);
        assert!(
            !Path::new(&owner).join(".immorterm/project.json").exists(),
            "read-only resolver must not materialize project.json"
        );
        assert!(
            !Path::new(&owner).join(".gitignore").exists(),
            "read-only resolver must not touch .gitignore"
        );
        // With project.json present → reads the id verbatim.
        let dir = Path::new(&owner).join(".immorterm");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("project.json"), r#"{"id":"pid-9","name":"P"}"#).unwrap();
        assert_eq!(read_project_id_only(&owner).as_deref(), Some("pid-9"));
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
        assert!(
            !Path::new(&owner).join(".gitignore").exists(),
            "no repo → no .gitignore"
        );
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
        assert!(
            !Path::new(&owner)
                .join(".immorterm")
                .join(MEMORY_HINT_FLAG)
                .exists()
        );
        let _ = fs::remove_dir_all(&owner);
    }

    #[test]
    fn memory_hooks_hint_shown_once_when_cli_missing() {
        let owner = temp_owner_dir();
        let first = ensure_memory_hooks_with(&owner, None);
        assert_eq!(first.as_deref(), Some(MEMORY_HINT));
        assert!(
            Path::new(&owner)
                .join(".immorterm")
                .join(MEMORY_HINT_FLAG)
                .exists()
        );
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
        fs::write(
            &cli,
            format!("#!/bin/sh\necho \"$@\" > {}\n", out.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();

        let hint = ensure_memory_hooks_with(&owner, Some(cli.to_string_lossy().into_owned()));
        assert_eq!(hint, None, "CLI found → install path, no hint");
        assert!(
            !Path::new(&owner)
                .join(".immorterm")
                .join(MEMORY_HINT_FLAG)
                .exists()
        );

        // Non-blocking spawn — poll for the fake CLI's output. Wait for the
        // EXPECTED content, not merely for the file to become readable: the
        // shell's `>` truncates before it writes, so a first successful read
        // can legitimately return "". Breaking on that produced a load-dependent
        // flake that failed with an empty-string mismatch and no explanation.
        let want = format!("hooks install --project {}", owner);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut args = String::new();
        while std::time::Instant::now() < deadline {
            args = fs::read_to_string(&out).unwrap_or_default();
            if args.trim() == want {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            args.trim(),
            want,
            "fake CLI never recorded the expected argv (empty = it was never \
             spawned or never ran; different = wrong spawn contract)"
        );
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
        fs::write(
            &impostor,
            "#!/bin/sh\necho 'connect: No such file or directory' >&2\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&impostor, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!cli_supports_hooks(&impostor.to_string_lossy(), &owner));

        // The Node CLI shape: answers `hooks status` (exit code irrelevant).
        let real = Path::new(&owner).join("node-cli");
        fs::write(
            &real,
            "#!/bin/sh\necho 'Memory hooks not installed'\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(cli_supports_hooks(&real.to_string_lossy(), &owner));

        let _ = fs::remove_dir_all(&owner);
    }

    /// T20: a pre-rename registry.json must keep every field, and come back
    /// out under the vendor-neutral names. If the serde aliases are ever
    /// dropped, 67 live entries silently lose their session id — this is the
    /// check that fails first.
    #[test]
    fn legacy_claude_keys_deserialize_and_reemit_as_ai() {
        let legacy = r#"{
            "sessions": [{
                "pid": 1, "name": "immorterm-ai-1", "window_id": "w1",
                "display_name": "one", "project_dir": "/tmp/p", "shell": "/bin/zsh",
                "created_at": 0,
                "claude_session_id": "sess-abc",
                "claude_transcript_path": "/home/u/.codex/sessions/x.jsonl",
                "claude_stats": {"model": "gpt-5", "context_pct": 42.0}
            }]
        }"#;

        let reg: Registry = serde_json::from_str(legacy).expect("legacy registry must parse");
        let e = &reg.sessions[0];
        assert_eq!(e.ai_session_id.as_deref(), Some("sess-abc"));
        assert_eq!(
            e.ai_transcript_path.as_deref(),
            Some("/home/u/.codex/sessions/x.jsonl")
        );
        assert!(e.ai_stats.is_some(), "claude_stats must survive the rename");

        // Re-serialized under the new names only — no entry carries both.
        let out = serde_json::to_string(&reg).unwrap();
        assert!(out.contains("\"ai_session_id\""));
        assert!(out.contains("\"ai_transcript_path\""));
        assert!(out.contains("\"ai_stats\""));
        assert!(!out.contains("claude_session_id"));
        assert!(!out.contains("claude_transcript_path"));
        assert!(!out.contains("claude_stats"));
    }

    /// T20 transition: a file we write must still be readable by a PRE-T20
    /// daemon, which knows only `claude_session_id` and has no alias. Modelled
    /// with a struct shaped like the old one — if the mirror is ever dropped,
    /// this fails and the 67 live entries with a session id are at risk.
    #[test]
    fn dual_write_keeps_pre_t20_readers_working() {
        #[derive(serde::Deserialize)]
        struct OldEntry {
            claude_session_id: Option<String>,
            claude_transcript_path: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct OldRegistry {
            sessions: Vec<OldEntry>,
        }

        let modern = r#"{
            "sessions": [{
                "pid": 1, "name": "n", "window_id": "w", "display_name": "d",
                "project_dir": "/tmp", "shell": "/bin/zsh", "created_at": 0,
                "ai_session_id": "sess-abc",
                "ai_transcript_path": "/p/x.jsonl"
            }]
        }"#;
        let reg: Registry = serde_json::from_str(modern).unwrap();

        let on_disk = dual_write_legacy_keys(&reg).unwrap();
        let raw = serde_json::to_string(&on_disk).unwrap();

        // New readers see the new names...
        assert!(raw.contains("\"ai_session_id\""));
        // ...and an old reader still finds everything it needs.
        let old: OldRegistry = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            old.sessions[0].claude_session_id.as_deref(),
            Some("sess-abc")
        );
        assert_eq!(
            old.sessions[0].claude_transcript_path.as_deref(),
            Some("/p/x.jsonl")
        );
    }
}

#[cfg(test)]
mod registry_d_tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    // $HOME is process-global (socket_dir → registry.d derives from it), so
    // these tests serialize on it and restore it after.
    static HOME_LOCK: Mutex<()> = Mutex::new(());
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _g = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("imterm-regd-{}-{}", std::process::id(), n));
        let _ = fs::create_dir_all(&home);
        let prev = std::env::var_os("HOME");
        // SAFETY: serialized by HOME_LOCK, restored below.
        unsafe { std::env::set_var("HOME", &home) };
        let out = f();
        match prev {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = fs::remove_dir_all(&home);
        out
    }

    /// A fully-populated entry — every serde field set so the round-trip test
    /// actually exercises them, not just the always-present ones.
    fn full_entry() -> RegistryEntry {
        serde_json::from_value(serde_json::json!({
            "pid": 4242,
            "name": "immorterm-ai-abc123",
            "window_id": "win-abc-123",
            "display_name": "my tab",
            "project_dir": "/tmp/proj",
            "ai_session_id": "sess-uuid-9",
            "title_locked": true,
            "title": "hello",
            "logfile": "/tmp/proj/log",
            "shell": "/bin/zsh",
            "created_at": 1234,
            "session_type": "ai",
            "ws_port": 8123,
            "theme": "aurora-borealis",
            "ai_transcript_path": "/home/u/.claude/projects/x.jsonl",
            "ai_stats": {"pid": 99, "rss_kb": 512, "cpu_percent": 3.5, "model": "Claude Opus 4", "cost_usd": 0.42, "context_pct": 61.0},
            "tool": "claude-code",
            "tool_history": [{"tool": "claude-code", "session_id": "sess-uuid-9", "transcript_path": "/home/u/.claude/projects/x.jsonl", "ts": "2026-08-09T00:00:00Z"}],
            "session_status": "active",
            "shelved_at": 4200,
            "structured_log_dir": "/tmp/proj/.immorterm/terminals/logs/win-abc-123",
            "needs_attention": true,
            "is_working": true,
            "last_activity_at": 1786705000123_u64,
            "heartbeat_at": 1786705000456_u64,
            "owner_project_dir": "/tmp/proj",
            "owner_project_id": "proj-uuid-1",
            "owner_project_name": "proj",
            "worktree": "/tmp/proj-wt"
        }))
        .expect("full entry must deserialize")
    }

    /// Strip the legacy mirror keys `dual_write_entry_legacy_keys` added, so the
    /// value collapses back to the canonical single-key form the struct parses.
    /// A Phase-2 reader of registry.d must do the same (the `#[serde(alias)]`
    /// derive rejects a file carrying BOTH `ai_session_id` and
    /// `claude_session_id` — "duplicate field"). See the report note.
    fn strip_legacy(mut v: serde_json::Value) -> serde_json::Value {
        if let Some(obj) = v.as_object_mut() {
            for (_, legacy) in MIRRORED {
                obj.remove(*legacy);
            }
        }
        v
    }

    /// (a) write_session_file → read the file back → every field survives,
    /// including ai_session_id, and the legacy mirror keys are present on disk.
    #[test]
    fn write_session_file_round_trips_every_field() {
        with_temp_home(|| {
            let e = full_entry();
            write_session_file(&e).expect("write must succeed");

            let path = registry_d_path("proj-uuid-1", "win-abc-123");
            let raw = fs::read_to_string(&path).expect("file must exist at project+window path");
            let on_disk: serde_json::Value = serde_json::from_str(&raw).unwrap();

            // Legacy mirror written to disk (dual-write contract).
            assert_eq!(
                on_disk["claude_session_id"],
                serde_json::json!("sess-uuid-9")
            );
            assert!(on_disk.get("claude_transcript_path").is_some());
            assert!(on_disk.get("claude_stats").is_some());

            // Reverse the mirror the write added, then deserialize: every field
            // round-trips into the struct, ai_session_id included.
            let back: RegistryEntry =
                serde_json::from_value(strip_legacy(on_disk)).expect("read-back must parse");
            assert_eq!(back.ai_session_id.as_deref(), Some("sess-uuid-9"));
            assert_eq!(
                serde_json::to_value(&e).unwrap(),
                serde_json::to_value(&back).unwrap(),
                "every field must round-trip through registry.d"
            );
        });
    }

    /// (b) The field-stripping fix: a re-register carrying ai_session_id=None
    /// must merge-forward the existing id and never blank it in registry.d.
    /// Mirrors register_session's flow: register() then write the MERGED entry.
    #[test]
    fn reregister_with_none_does_not_strip_existing_id() {
        with_temp_home(|| {
            let mut with_id = full_entry();
            with_id.ai_session_id = Some("keep-me".to_string());

            let mut registry = Registry {
                sessions: vec![],
                baseline: vec![],
            };
            registry.register(with_id);

            // A respawn: same window_id, ai_session_id stripped (env not set yet).
            let mut respawn = full_entry();
            respawn.pid = 5555;
            respawn.ai_session_id = None;
            registry.register(respawn);

            let merged = registry
                .sessions
                .iter()
                .find(|e| e.window_id == "win-abc-123")
                .cloned()
                .expect("entry present after re-register");
            assert_eq!(
                merged.ai_session_id.as_deref(),
                Some("keep-me"),
                "in-memory merge-forward"
            );

            write_session_file(&merged).expect("write must succeed");
            let path = registry_d_path("proj-uuid-1", "win-abc-123");
            let on_disk: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            let back: RegistryEntry = serde_json::from_value(strip_legacy(on_disk)).unwrap();
            assert_eq!(
                back.ai_session_id.as_deref(),
                Some("keep-me"),
                "registry.d must not blank an existing id on a None re-register"
            );
        });
    }

    /// REGRESSION (critical): `save()` dual-writes BOTH `ai_session_id` and its
    /// `claude_session_id` alias; `load()` must still parse it. Before
    /// `parse_registry()`, `load_raw()`'s `from_str::<Registry>` failed on the
    /// duplicate field, the backup fallback failed the same way, and `load()`
    /// returned an EMPTY registry — a self-inflicted wipe on every reload, and
    /// (via `save()`'s re-read-and-merge) a collapse to the writer's own session.
    #[test]
    fn save_then_load_survives_dual_write() {
        with_temp_home(|| {
            let mut reg = Registry {
                sessions: vec![],
                baseline: vec![],
            };
            reg.sessions.push(full_entry());
            reg.save().expect("save must succeed");

            // The on-disk file really does carry both keys (the trap).
            let raw = fs::read_to_string(registry_path()).unwrap();
            assert!(
                raw.contains("\"ai_session_id\"") && raw.contains("\"claude_session_id\""),
                "save() must dual-write both keys for this regression to be meaningful"
            );

            // The REAL load path must recover the session + id, not wipe it.
            let loaded = Registry::load();
            assert_eq!(
                loaded.sessions.len(),
                1,
                "load() wiped the registry — dual-write parse failure regressed"
            );
            assert_eq!(
                loaded.sessions[0].ai_session_id.as_deref(),
                Some("sess-uuid-9")
            );
        });
    }

    /// `parse_registry` promotes a legacy-only key and collapses a dual-written
    /// pair to the modern key without a duplicate-field error.
    #[test]
    fn parse_registry_normalizes_legacy_and_dual_keys() {
        let base = |extra: &str| {
            format!(
                r#"{{"sessions":[{{"pid":1,"name":"n","window_id":"w","display_name":"d","project_dir":"/p","shell":"/bin/zsh","created_at":0,{extra}}}]}}"#
            )
        };
        let legacy = base(r#""claude_session_id":"legacy-id""#);
        assert_eq!(
            parse_registry(&legacy)
                .expect("legacy-only must parse")
                .sessions[0]
                .ai_session_id
                .as_deref(),
            Some("legacy-id"),
        );
        let both = base(r#""ai_session_id":"modern-id","claude_session_id":"legacy-id""#);
        assert_eq!(
            parse_registry(&both)
                .expect("dual-written must parse")
                .sessions[0]
                .ai_session_id
                .as_deref(),
            Some("modern-id"),
        );
    }

    /// remove_session_file deletes the file and is idempotent on a missing one.
    #[test]
    fn remove_session_file_is_idempotent() {
        with_temp_home(|| {
            let e = full_entry();
            write_session_file(&e).unwrap();
            let path = registry_d_path("proj-uuid-1", "win-abc-123");
            assert!(path.exists());
            remove_session_file("proj-uuid-1", "win-abc-123");
            assert!(!path.exists(), "file removed");
            // Second removal (ENOENT) must be a no-op, not a panic.
            remove_session_file("proj-uuid-1", "win-abc-123");
        });
    }
}
