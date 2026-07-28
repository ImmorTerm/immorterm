//! Self-discovery loop — reads `~/.immorterm/registry.json` periodically
//! and reconciles the daemon's `SessionRegistry` with what the hub
//! reports as live.
//!
//! This is the daemon's ONLY source of "what sessions exist." It does
//! NOT depend on Claude hooks for keepalive — `SessionStart` only needs
//! to start the daemon binary (via `ensure-digest-daemon.sh`); from
//! there the daemon self-manages from the registry.
//!
//! Per internal design notes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::Instant;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::debouncer::{Debouncer, DebouncerConfig};
use crate::hub_client::{HubClient, SessionEndRequest, Wal, WalEntry};
use crate::key::AiSessionKey;
use crate::lifecycle::{LifecycleModel, LifecycleState, SessionStatus};
use crate::registry::{SessionRegistry, SessionTrack};
use crate::watcher::WatcherHub;

/// Fallback tick. notify gives us push events for ~99% of cases; this
/// keeps us correct against missed events (FSEvents has known edge
/// cases on NFS / sleep/wake / VM snapshots). Set high — notify is the
/// primary trigger, this is just a safety net.
pub const FALLBACK_RESCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

/// Minimal slice of `RegistryEntry` we care about. Tolerant of unknown
/// fields so the hub can evolve without breaking us.
#[derive(Debug, Deserialize)]
pub(crate) struct RegistryEntryView {
    pub window_id: String,
    pub project_dir: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub ai_session_id: Option<String>,
    #[serde(default)]
    pub ai_transcript_path: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub ai_stats: Option<ClaudeStatsView>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClaudeStatsView {
    #[serde(default)]
    pub pid: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RegistryFileView {
    #[serde(default)]
    pub sessions: Vec<RegistryEntryView>,
}

/// What the reconciliation pass wants done. Pure data — caller (Rust
/// async) actually executes against `SessionRegistry` + `WatcherHub`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileAction {
    Register {
        key: AiSessionKey,
        tool: String,
        transcript_path: PathBuf,
        project_id: String,
        project_dir: PathBuf,
    },
    /// Session disappeared from registry OR its AI PID is dead.
    Unregister {
        key: AiSessionKey,
        exit_reason: &'static str,
    },
}

/// Pure reconciliation: given the registry file's current contents and
/// the set of keys the daemon currently has registered, produce the
/// register/unregister actions to converge them.
///
/// `pid_alive_fn` is injected so tests can stub it (the real version
/// uses `kill(pid, 0)`).
pub(crate) fn reconcile(
    file: &RegistryFileView,
    currently_registered: &HashSet<AiSessionKey>,
    host_id: &str,
    pid_alive_fn: impl Fn(u32) -> bool,
) -> Vec<ReconcileAction> {
    let mut want: HashSet<AiSessionKey> = HashSet::new();
    let mut new_registrations: Vec<ReconcileAction> = Vec::new();

    for entry in &file.sessions {
        let session_id = match entry.ai_session_id.as_deref() {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        // `tool` here selects the TRANSCRIPT ADAPTER, so it must describe the
        // file we are about to parse — not whoever most recently announced
        // themselves in this window.
        //
        // A window is long-lived and the user can switch vendors inside it at
        // will (that's what `tool_history` records). `tool` and
        // `ai_transcript_path` are both overwritten on each session-link, but
        // not atomically and not by the same writer, so there is a window
        // where they disagree. Trusting `tool` there hands a Codex rollout to
        // the Claude adapter and silently yields garbage.
        //
        // So: when we have a transcript path, IT is the authority — every
        // vendor writes under its own state dir, so the path names its own
        // format. `tool` is only consulted when there's no path to read,
        // where it's needed to build the convention path in the first place.
        let announced_tool = entry
            .tool
            .clone()
            .unwrap_or_else(|| "claude-code".to_string());
        let (tool, transcript) = match entry.ai_transcript_path.as_deref() {
            Some(path) if !path.is_empty() => {
                let owner = infer_tool_from_transcript(path)
                    .map(str::to_string)
                    .unwrap_or(announced_tool);
                (owner, path.to_string())
            }
            // No path recorded — the hub's tracker is supposed to populate it
            // every 30s but in practice often doesn't, so fall back to the
            // vendor's well-known convention path, which needs the announced
            // tool to know which convention to use.
            _ => {
                let path = convention_transcript_path_for(
                    &announced_tool,
                    &entry.project_dir,
                    session_id,
                );
                (announced_tool, path)
            }
        };
        // AI process must be alive. Prefer the AI tool's pid in
        // ai_stats (set by session-link); fall back to the daemon
        // registry's pid for legacy entries.
        let ai_pid = entry
            .ai_stats
            .as_ref()
            .and_then(|s| s.pid)
            .or(entry.pid);
        let alive = match ai_pid {
            Some(p) if p > 0 => pid_alive_fn(p),
            _ => false,
        };
        if !alive {
            continue;
        }

        let key = AiSessionKey::new(&entry.window_id, session_id, host_id);
        want.insert(key.clone());

        if !currently_registered.contains(&key) {
            new_registrations.push(ReconcileAction::Register {
                key,
                tool: tool.clone(),
                transcript_path: PathBuf::from(transcript),
                project_id: derive_project_id(&entry.project_dir),
                project_dir: PathBuf::from(&entry.project_dir),
            });
        }
    }

    // Anything currently registered that's no longer in `want` → unregister.
    let mut actions = new_registrations;
    for key in currently_registered {
        if !want.contains(key) {
            actions.push(ReconcileAction::Unregister {
                key: key.clone(),
                exit_reason: "pid_dead",
            });
        }
    }
    actions
}

/// Derive project_id (== user_id for memory writes).
///
/// Source of truth is `<project_dir>/.mcp.json` — specifically the slug
/// at the end of the immorterm-memory server URL
/// (`http://.../mcp/<vendor>/<slug>`). This matches what the extension's
/// `getStableProjectId()` resolves to and what the SessionStart hook's
/// `_immorterm-env.sh` exports as `IMMORTERM_PROJECT_ID`. Using basename
/// (the old behavior) caused memories to be written under a different
/// `user_id` than the one the extension queries, breaking the session
/// summary modal for any project whose folder name diverges from its
/// MCP slug (e.g. folder `immorterm`, slug `lonormaly-immorterm`).
///
/// Falls back to basename if `.mcp.json` is missing or unparseable.
fn derive_project_id(project_dir: &str) -> String {
    if let Some(slug) = read_mcp_slug(project_dir) {
        return slug;
    }
    Path::new(project_dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(project_dir)
        .to_string()
}

/// Parse `<project_dir>/.mcp.json` and return the slug at the end of the
/// first immorterm-memory server URL. Matches the regex used by
/// `_immorterm-env.sh`: `r'/mcp/[^/]+/([^/]+)$'`.
fn read_mcp_slug(project_dir: &str) -> Option<String> {
    let path = Path::new(project_dir).join(".mcp.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let servers = v.get("mcpServers")?.as_object()?;
    for (_, cfg) in servers {
        let url = cfg.get("url").and_then(|u| u.as_str()).unwrap_or("");
        if url.is_empty() {
            continue;
        }
        if let Some(idx) = url.rfind('/') {
            let slug = &url[idx + 1..];
            // Skip the trailing "/sse" sentinel some servers use.
            if !slug.is_empty() && slug != "sse" {
                return Some(slug.to_string());
            }
        }
    }
    None
}

/// Infer which vendor owns a session from its transcript path.
///
/// Each vendor writes transcripts under its own state directory, so the path
/// names the vendor with no guessing. `None` for a missing or unrecognised
/// path, so callers fall back rather than mislabel.
///
/// Mirrors `infer_tool_from_transcript` in the hub — the digest reads
/// registry.json directly and can't call into the hub crate.
fn infer_tool_from_transcript(path: &str) -> Option<&'static str> {
    if path.contains("/.codex/") {
        Some("codex")
    } else if path.contains("/.claude/") {
        Some("claude-code")
    } else {
        None
    }
}


/// Claude Code stores per-project transcripts at
/// `$HOME/.claude/projects/<encoded>/<session_id>.jsonl` where `<encoded>`
/// is the absolute project_dir with `/` replaced by `-`. e.g.
/// `/Users/example/Development/foo` → `-Users-example-Development-foo`.
/// This matches `discover_jsonl_dir()` in the bash daemon — the same
/// convention every Claude Code session has used since launch.
/// Per-vendor fallback for where a session's transcript lives on disk.
///
/// Only used when the registry has no `transcript_path` — the SessionStart
/// hook now announces the real absolute path via `/registry/session-link`, and
/// that always wins. This exists for sessions that started before the announce
/// landed, or whose hub POST failed.
fn convention_transcript_path_for(tool: &str, project_dir: &str, session_id: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("."));
    match tool {
        "codex" => codex_rollout_path_with_home(&home, session_id)
            // No rollout on disk yet (or the dir is unreadable) — return a
            // path that simply won't exist rather than a Claude-shaped one,
            // so the caller skips instead of digesting the wrong file.
            .unwrap_or_else(|| format!("{home}/.codex/sessions/{session_id}.jsonl")),
        _ => convention_transcript_path_with_home(&home, project_dir, session_id),
    }
}

/// Newest Codex rollout file whose name ends in `<session_id>.jsonl`.
///
/// Codex also records the absolute path in `state_5.sqlite` (`threads.
/// rollout_path`), which would be exact — but this is a fallback of a
/// fallback (the SessionStart hook's session-link announce supplies the real
/// path, and it wins above), so reading another product's private database,
/// or hand-parsing it to avoid a driver dependency, buys very little for the
/// fragility it adds. The walk is O(sessions) once per registration and has
/// no schema to drift.
///
/// Codex date-shards its transcripts:
///   `~/.codex/sessions/YYYY/MM/DD/rollout-<ISO8601>-<uuid>.jsonl`
/// so unlike Claude there is no single derivable path — the date prefix isn't
/// knowable from the session id. Walk the tree and match on the uuid suffix,
/// newest wins (a resumed id can appear more than once).
fn codex_rollout_path_with_home(home: &str, session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    let root = std::path::Path::new(home).join(".codex").join("sessions");
    let suffix = format!("{session_id}.jsonl");
    let mut best: Option<(std::time::SystemTime, String)> = None;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
            {
                continue;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let is_newer = best.as_ref().is_none_or(|(best_m, _)| mtime > *best_m);
            if is_newer {
                best = Some((mtime, path.to_string_lossy().into_owned()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Test-injectable variant — takes HOME explicitly so tests don't have
/// to mutate the global env (Rust 2024 flags `set_var` as unsafe).
fn convention_transcript_path_with_home(home: &str, project_dir: &str, session_id: &str) -> String {
    let encoded = project_dir.replace('/', "-");
    format!("{home}/.claude/projects/{encoded}/{session_id}.jsonl")
}

pub fn default_registry_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".immorterm").join("registry.json")
}

fn load_registry(path: &Path) -> Option<RegistryFileView> {
    let data = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<RegistryFileView>(&data) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("registry parse failed at {}: {}", path.display(), e);
            None
        }
    }
}

fn real_pid_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) is a syscall, side-effect-free for !=0 sig.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Apply a list of actions against the live state. Public so the
/// orchestrator can call this from its own tick task if it ever
/// becomes useful to drive reconciliation by hand.
pub async fn apply_actions(
    actions: Vec<ReconcileAction>,
    registry: &Arc<Mutex<SessionRegistry>>,
    watcher: &Arc<Mutex<WatcherHub>>,
    hub: &HubClient,
    wal: &Wal,
) {
    for action in actions {
        match action {
            ReconcileAction::Register {
                key,
                tool,
                transcript_path,
                project_id,
                project_dir,
            } => {
                let parent = match transcript_path.parent() {
                    Some(p) => p.to_path_buf(),
                    None => {
                        tracing::warn!("transcript has no parent dir: {:?}", transcript_path);
                        continue;
                    }
                };
                let mut hub_w = watcher.lock().await;
                if let Err(e) = hub_w.acquire(&parent) {
                    tracing::warn!("watcher.acquire({}) failed: {}", parent.display(), e);
                    continue;
                }
                drop(hub_w);

                let model = LifecycleModel::for_vendor(&tool);
                let mut reg = registry.lock().await;
                reg.insert(SessionTrack {
                    key: key.clone(),
                    tool,
                    transcript_path,
                    project_id,
                    project_dir,
                    lifecycle: LifecycleState::new(model),
                    debouncer: Debouncer::new(DebouncerConfig::default(), Instant::now()),
                    status: SessionStatus::Active,
                    registered_at: std::time::SystemTime::now(),
                    ended_at: None,
                });
                tracing::info!("registered session {key}");
            }
            ReconcileAction::Unregister { key, exit_reason } => {
                let track = {
                    let mut reg = registry.lock().await;
                    reg.remove(&key)
                };
                if let Some(t) = track {
                    if let Some(parent) = t.transcript_path.parent() {
                        let mut hub_w = watcher.lock().await;
                        if let Err(e) = hub_w.release(parent) {
                            tracing::warn!("watcher.release({}) failed: {}", parent.display(), e);
                        }
                    }
                    let req = SessionEndRequest {
                        window_id: key.window_id.clone(),
                        vendor_session_id: key.vendor_session_id.clone(),
                        exit_reason: exit_reason.to_string(),
                        host_id: Some(key.host_id.clone()),
                        ended_at: Some(chrono::Utc::now().to_rfc3339()),
                    };
                    if let Err(e) = hub.post_session_end(&req).await {
                        tracing::warn!("hub session-end failed, WAL queueing: {e}");
                        if let Err(we) = wal.append(&WalEntry::SessionEnd(req)) {
                            tracing::error!("WAL append failed: {we}");
                        }
                    }
                    tracing::info!("unregistered session {key} ({})", exit_reason);
                }
            }
        }
    }
}

/// notify-driven discovery loop. Watches `~/.immorterm/` (parent of
/// registry.json) so we don't miss events when hub does temp+rename
/// (atomic write swaps the inode; a watch on the file itself would
/// silently stop receiving events). Filters delivered paths by
/// basename = "registry.json".
///
/// Cadence:
/// - **Startup:** one immediate reconcile (notify won't fire for
///   entries that already exist in registry.json at boot).
/// - **Per notify event:** reconcile.
/// - **Every FALLBACK_RESCAN_INTERVAL:** safety-net reconcile against
///   dropped events (FSEvents can miss across sleep/wake; rare).
pub async fn run_watch_loop(
    registry_path: PathBuf,
    host_id: String,
    registry: Arc<Mutex<SessionRegistry>>,
    watcher: Arc<Mutex<WatcherHub>>,
    hub: HubClient,
    wal: Wal,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    let watch_dir = match registry_path.parent() {
        Some(p) => p.to_path_buf(),
        None => {
            tracing::error!(
                "registry_path has no parent: {}; bailing",
                registry_path.display()
            );
            return;
        }
    };
    let target_basename = registry_path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();

    // notify uses a sync mpsc; bridge into a tokio channel for select!.
    let (sync_tx, sync_rx) = std_mpsc::channel::<notify::Result<Event>>();
    let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let target_basename_for_bridge = target_basename.clone();
    std::thread::Builder::new()
        .name("digest-registry-bridge".into())
        .spawn(move || {
            while let Ok(res) = sync_rx.recv() {
                let event = match res {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!("notify error on registry.json watcher: {e}");
                        continue;
                    }
                };
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    continue;
                }
                let matched = event
                    .paths
                    .iter()
                    .any(|p| p.file_name() == Some(&target_basename_for_bridge));
                if matched {
                    let _ = async_tx.send(());
                }
            }
        })
        .ok();

    let mut watcher_handle: Option<RecommendedWatcher> =
        match notify::recommended_watcher(move |res| {
            let _ = sync_tx.send(res);
        }) {
            Ok(mut w) => {
                if let Err(e) = w.watch(&watch_dir, RecursiveMode::NonRecursive) {
                    tracing::error!(
                        "watch({}) failed: {} — falling back to fallback-interval rescans only",
                        watch_dir.display(),
                        e
                    );
                    None
                } else {
                    Some(w)
                }
            }
            Err(e) => {
                tracing::error!(
                    "create registry watcher failed: {} — falling back to fallback-interval rescans only",
                    e
                );
                None
            }
        };

    // Initial reconcile — pick up sessions that existed before daemon started.
    do_reconcile(&registry_path, &host_id, &registry, &watcher, &hub, &wal).await;

    let mut fallback = tokio::time::interval(FALLBACK_RESCAN_INTERVAL);
    fallback.tick().await; // consume the immediate tick (we just reconciled)
    loop {
        tokio::select! {
            biased;
            _ = cancel.changed() => if *cancel.borrow() { break; },
            ev = async_rx.recv() => match ev {
                Some(()) => {
                    do_reconcile(&registry_path, &host_id, &registry, &watcher, &hub, &wal).await;
                }
                None => break,
            },
            _ = fallback.tick() => {
                do_reconcile(&registry_path, &host_id, &registry, &watcher, &hub, &wal).await;
            }
        }
    }
    // Explicit drop on exit for clarity.
    drop(watcher_handle.take());
}

async fn do_reconcile(
    registry_path: &Path,
    host_id: &str,
    registry: &Arc<Mutex<SessionRegistry>>,
    watcher: &Arc<Mutex<WatcherHub>>,
    hub: &HubClient,
    wal: &Wal,
) {
    let file = match load_registry(registry_path) {
        Some(f) => f,
        None => return,
    };
    let snapshot: HashSet<AiSessionKey> = {
        let r = registry.lock().await;
        r.iter().map(|(k, _)| k.clone()).collect()
    };
    let actions = reconcile(&file, &snapshot, host_id, real_pid_alive);
    if !actions.is_empty() {
        tracing::info!("registry reconcile: {} action(s)", actions.len());
        apply_actions(actions, registry, watcher, hub, wal).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_alive(
        window_id: &str,
        session_id: &str,
        transcript: &str,
        project_dir: &str,
    ) -> RegistryEntryView {
        RegistryEntryView {
            window_id: window_id.into(),
            project_dir: project_dir.into(),
            pid: Some(1),
            ai_session_id: Some(session_id.into()),
            ai_transcript_path: Some(transcript.into()),
            tool: Some("claude-code".into()),
            ai_stats: Some(ClaudeStatsView { pid: Some(42) }),
        }
    }

    #[test]
    fn empty_registry_produces_no_actions() {
        let file = RegistryFileView { sessions: vec![] };
        let actions = reconcile(&file, &HashSet::new(), "h1", |_| true);
        assert!(actions.is_empty());
    }

    #[test]
    fn new_alive_entry_produces_register_action() {
        let file = RegistryFileView {
            sessions: vec![entry_alive("w1", "s1", "/tmp/a.jsonl", "/tmp/p")],
        };
        let actions = reconcile(&file, &HashSet::new(), "h1", |_| true);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ReconcileAction::Register {
                key,
                tool,
                transcript_path,
                project_id,
                ..
            } => {
                assert_eq!(key.window_id, "w1");
                assert_eq!(key.vendor_session_id, "s1");
                assert_eq!(key.host_id, "h1");
                assert_eq!(tool, "claude-code");
                assert_eq!(transcript_path, &PathBuf::from("/tmp/a.jsonl"));
                assert_eq!(project_id, "p");
            }
            other => panic!("expected Register, got {other:?}"),
        }
    }

    #[test]
    fn already_registered_produces_no_action() {
        let file = RegistryFileView {
            sessions: vec![entry_alive("w1", "s1", "/tmp/a.jsonl", "/tmp/p")],
        };
        let mut have = HashSet::new();
        have.insert(AiSessionKey::new("w1", "s1", "h1"));
        let actions = reconcile(&file, &have, "h1", |_| true);
        assert!(actions.is_empty(), "no churn on stable state");
    }

    #[test]
    fn dead_pid_skips_registration() {
        let file = RegistryFileView {
            sessions: vec![entry_alive("w1", "s1", "/tmp/a.jsonl", "/tmp/p")],
        };
        // pid_alive_fn returns false → entry treated as dead
        let actions = reconcile(&file, &HashSet::new(), "h1", |_| false);
        assert!(actions.is_empty());
    }

    #[test]
    fn dead_pid_for_registered_produces_unregister() {
        let file = RegistryFileView {
            sessions: vec![entry_alive("w1", "s1", "/tmp/a.jsonl", "/tmp/p")],
        };
        let mut have = HashSet::new();
        have.insert(AiSessionKey::new("w1", "s1", "h1"));
        let actions = reconcile(&file, &have, "h1", |_| false);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ReconcileAction::Unregister { key, exit_reason } => {
                assert_eq!(key.vendor_session_id, "s1");
                assert_eq!(*exit_reason, "pid_dead");
            }
            other => panic!("expected Unregister, got {other:?}"),
        }
    }

    #[test]
    fn entry_disappearing_from_file_produces_unregister() {
        let file = RegistryFileView { sessions: vec![] };
        let mut have = HashSet::new();
        have.insert(AiSessionKey::new("w1", "s1", "h1"));
        let actions = reconcile(&file, &have, "h1", |_| true);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], ReconcileAction::Unregister { .. }));
    }

    #[test]
    fn entry_without_ai_session_id_is_skipped() {
        let mut e = entry_alive("w1", "", "/tmp/a.jsonl", "/tmp/p");
        e.ai_session_id = None;
        let file = RegistryFileView { sessions: vec![e] };
        let actions = reconcile(&file, &HashSet::new(), "h1", |_| true);
        assert!(actions.is_empty(), "no session_id → not yet linked, skip");
    }


    #[test]
    fn legacy_entry_without_tool_defaults_to_claude_code() {
        let mut e = entry_alive("w1", "s1", "/tmp/a.jsonl", "/tmp/p");
        e.tool = None;
        let file = RegistryFileView { sessions: vec![e] };
        let actions = reconcile(&file, &HashSet::new(), "h1", |_| true);
        assert_eq!(actions.len(), 1);
        if let ReconcileAction::Register { tool, .. } = &actions[0] {
            assert_eq!(tool, "claude-code");
        } else {
            panic!("expected Register");
        }
    }

    #[test]
    fn entry_without_transcript_path_falls_back_to_convention() {
        // Hub's claude_tracker is supposed to populate
        // ai_transcript_path but often doesn't for live sessions.
        // Daemon must still pick the session up via the well-known
        // Claude Code path convention.
        let mut e = entry_alive("w1", "abc-uuid", "/tmp/a.jsonl", "/Users/test/Development/foo");
        e.ai_transcript_path = None;
        let file = RegistryFileView { sessions: vec![e] };
        let actions = reconcile(&file, &HashSet::new(), "h1", |_| true);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            ReconcileAction::Register { transcript_path, .. } => {
                // Production path uses real $HOME; here we just verify
                // the convention-encoded segment is present.
                assert!(
                    transcript_path
                        .to_string_lossy()
                        .contains("/.claude/projects/-Users-test-Development-foo/abc-uuid.jsonl"),
                    "got: {}", transcript_path.display()
                );
            }
            other => panic!("expected Register with convention path, got {other:?}"),
        }
    }

    #[test]
    fn convention_path_encoding_matches_bash_daemon() {
        assert_eq!(
            convention_transcript_path_with_home("/Users/u", "/Users/u/Development/proj", "uuid-1"),
            "/Users/u/.claude/projects/-Users-u-Development-proj/uuid-1.jsonl"
        );
    }

    /// Codex date-shards its rollouts, so unlike Claude the path can't be
    /// derived from the session id — it has to be found. Newest wins, because
    /// a resumed session id can appear under more than one date.
    #[test]
    fn codex_rollout_lookup_finds_newest_match_in_date_shards() {
        let tmp = std::env::temp_dir().join(format!("imcodexroll-{}", std::process::id()));
        let old_dir = tmp.join(".codex/sessions/2026/07/26");
        let new_dir = tmp.join(".codex/sessions/2026/07/27");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();

        let sid = "019fa2c1-1b29-70e3-ae4e-1d3b8a64e988";
        let older = old_dir.join(format!("rollout-2026-07-26T09-00-00-{sid}.jsonl"));
        let newer = new_dir.join(format!("rollout-2026-07-27T11-46-32-{sid}.jsonl"));
        // A different session in the same shard must not be picked up.
        let other = new_dir.join("rollout-2026-07-27T12-00-00-deadbeef-0000-0000-0000-000000000000.jsonl");
        std::fs::write(&older, b"{}\n").unwrap();
        std::fs::write(&other, b"{}\n").unwrap();
        std::fs::write(&newer, b"{}\n").unwrap();
        // Make `newer` unambiguously the most recent.
        let home = tmp.to_string_lossy().to_string();
        filetime_touch(&older, 1_000_000);
        filetime_touch(&newer, 2_000_000);

        let found = codex_rollout_path_with_home(&home, sid).expect("rollout found");
        assert_eq!(found, newer.to_string_lossy());

        // Unknown session id → no match, so the caller skips rather than
        // digesting some other session's transcript.
        assert!(codex_rollout_path_with_home(&home, "no-such-uuid").is_none());
        assert!(codex_rollout_path_with_home(&home, "").is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Vendor dispatch: Claude keeps its derived path, Codex must not get one.
    #[test]
    fn convention_path_is_vendor_dispatched() {
        let claude = convention_transcript_path_with_home("/Users/u", "/Users/u/proj", "uuid-1");
        assert!(claude.contains("/.claude/projects/"));
        // Codex has no derivable path — a missing rollout yields a path under
        // ~/.codex/sessions that simply won't exist, never a Claude-shaped one.
        let codex = codex_rollout_path_with_home("/nonexistent-home", "uuid-1");
        assert!(codex.is_none());
    }

    fn filetime_touch(path: &std::path::Path, secs: u64) {
        // Portable-enough mtime bump without pulling in a crate: rewrite the
        // file after sleeping is too slow, so use utimensat via libc-free std.
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn host_id_isolates_keys() {
        let file = RegistryFileView {
            sessions: vec![entry_alive("w1", "s1", "/tmp/a.jsonl", "/tmp/p")],
        };
        // Same registry entry, two different hosts → each gets its own key
        let a1 = reconcile(&file, &HashSet::new(), "h1", |_| true);
        let a2 = reconcile(&file, &HashSet::new(), "h2", |_| true);
        match (&a1[0], &a2[0]) {
            (ReconcileAction::Register { key: k1, .. }, ReconcileAction::Register { key: k2, .. }) => {
                assert_ne!(k1, k2);
                assert_eq!(k1.host_id, "h1");
                assert_eq!(k2.host_id, "h2");
            }
            _ => panic!("expected two Register actions"),
        }
    }

    #[test]
    fn project_id_derived_from_dir_basename() {
        assert_eq!(derive_project_id("/Users/example/Development/foo"), "foo");
        assert_eq!(derive_project_id("/tmp/single"), "single");
        assert_eq!(derive_project_id(""), "");
    }

    #[test]
    fn project_id_prefers_mcp_slug_when_present() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let mcp = dir.path().join(".mcp.json");
        std::fs::write(
            &mcp,
            r#"{
              "mcpServers": {
                "immorterm-memory": {
                  "url": "http://127.0.0.1:8765/mcp/claude-code/lonormaly-immorterm"
                }
              }
            }"#,
        )
        .unwrap();
        let project_id = derive_project_id(dir.path().to_str().unwrap());
        assert_eq!(project_id, "lonormaly-immorterm");
    }

    #[test]
    fn project_id_falls_back_to_basename_when_no_mcp_json() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let base = dir.path().file_name().unwrap().to_str().unwrap().to_string();
        assert_eq!(derive_project_id(dir.path().to_str().unwrap()), base);
    }

    #[test]
    fn project_id_skips_sse_sentinel_slug() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let mcp = dir.path().join(".mcp.json");
        std::fs::write(
            &mcp,
            r#"{
              "mcpServers": {
                "decoy": { "url": "http://127.0.0.1:9100/puppeteer/sse" },
                "immorterm-memory": {
                  "url": "http://127.0.0.1:8765/mcp/claude-code/lonormaly-immorterm"
                }
              }
            }"#,
        )
        .unwrap();
        // Map iteration order is non-deterministic; just verify we never
        // settle on the "sse" sentinel.
        for _ in 0..10 {
            assert_ne!(derive_project_id(dir.path().to_str().unwrap()), "sse");
        }
    }

    /// A window is long-lived and the user can switch vendors inside it at any
    /// time. `tool` and `ai_transcript_path` are updated by different writers
    /// and not atomically, so they disagree during the switch. `tool` drives
    /// the TRANSCRIPT ADAPTER, so the path has to win — otherwise a Codex
    /// rollout gets handed to the Claude parser and quietly yields nothing.
    #[test]
    fn transcript_path_beats_a_stale_announced_tool() {
        let mut entry = entry_alive(
            "w1",
            "sess-1",
            "/Users/u/.codex/sessions/2026/07/28/rollout-abc.jsonl",
            "/proj",
        );
        entry.tool = Some("claude-code".into()); // stale: user switched to Codex
        let file = RegistryFileView { sessions: vec![entry] };

        let actions = reconcile(&file, &HashSet::new(), "host", |_| true);
        match &actions[0] {
            ReconcileAction::Register { tool, .. } => assert_eq!(tool, "codex"),
            other => panic!("expected Register, got {:?}", other),
        }
    }

    /// The mirror image: no transcript path to read, so the announced tool is
    /// all we have and it must still pick the right convention path.
    #[test]
    fn announced_tool_is_used_when_no_transcript_path_recorded() {
        let mut entry = entry_alive("w1", "sess-1", "", "/proj");
        entry.ai_transcript_path = None;
        entry.tool = Some("codex".into());
        let file = RegistryFileView { sessions: vec![entry] };

        let actions = reconcile(&file, &HashSet::new(), "host", |_| true);
        match &actions[0] {
            ReconcileAction::Register { tool, .. } => assert_eq!(tool, "codex"),
            other => panic!("expected Register, got {:?}", other),
        }
    }

    /// An unrecognised path must not override an explicit announce.
    #[test]
    fn unknown_transcript_path_keeps_the_announced_tool() {
        let mut entry = entry_alive("w1", "sess-1", "/var/tmp/custom.jsonl", "/proj");
        entry.tool = Some("cursor".into());
        let file = RegistryFileView { sessions: vec![entry] };

        let actions = reconcile(&file, &HashSet::new(), "host", |_| true);
        match &actions[0] {
            ReconcileAction::Register { tool, .. } => assert_eq!(tool, "cursor"),
            other => panic!("expected Register, got {:?}", other),
        }
    }
}
