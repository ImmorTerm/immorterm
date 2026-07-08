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
    pub claude_session_id: Option<String>,
    #[serde(default)]
    pub claude_transcript_path: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub claude_stats: Option<ClaudeStatsView>,
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
        let session_id = match entry.claude_session_id.as_deref() {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        // Prefer registry's claude_transcript_path; fall back to Claude
        // Code's well-known convention path when missing. The hub's
        // claude_tracker is supposed to populate this field every 30s,
        // but in practice many live entries are missing it (hub chain
        // unreliable). The bash daemon already does this dir-convention
        // discovery; mirror it here so the Rust daemon picks up the
        // same sessions.
        let transcript: String = match entry.claude_transcript_path.as_deref() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => convention_transcript_path(&entry.project_dir, session_id),
        };
        // AI process must be alive. Prefer the AI tool's pid in
        // claude_stats (set by session-link); fall back to the daemon
        // registry's pid for legacy entries.
        let ai_pid = entry
            .claude_stats
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
                tool: entry.tool.clone().unwrap_or_else(|| "claude-code".to_string()),
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

/// Claude Code stores per-project transcripts at
/// `$HOME/.claude/projects/<encoded>/<session_id>.jsonl` where `<encoded>`
/// is the absolute project_dir with `/` replaced by `-`. e.g.
/// `/Users/example/Development/foo` → `-Users-example-Development-foo`.
/// This matches `discover_jsonl_dir()` in the bash daemon — the same
/// convention every Claude Code session has used since launch.
fn convention_transcript_path(project_dir: &str, session_id: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| String::from("."));
    convention_transcript_path_with_home(&home, project_dir, session_id)
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
            claude_session_id: Some(session_id.into()),
            claude_transcript_path: Some(transcript.into()),
            tool: Some("claude-code".into()),
            claude_stats: Some(ClaudeStatsView { pid: Some(42) }),
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
    fn entry_without_claude_session_id_is_skipped() {
        let mut e = entry_alive("w1", "", "/tmp/a.jsonl", "/tmp/p");
        e.claude_session_id = None;
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
        // claude_transcript_path but often doesn't for live sessions.
        // Daemon must still pick the session up via the well-known
        // Claude Code path convention.
        let mut e = entry_alive("w1", "abc-uuid", "/tmp/a.jsonl", "/Users/test/Development/foo");
        e.claude_transcript_path = None;
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
}
