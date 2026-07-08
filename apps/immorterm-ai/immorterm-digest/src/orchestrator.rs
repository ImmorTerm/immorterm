//! Top-level event loop: glues FS events → debouncer → pipeline.
//!
//! Per v4 §4 + §7: two concurrent loops driven from `serve()`:
//! - event_loop: drain notify mpsc, route to per-session debouncer
//! - tick_loop: every TICK_INTERVAL, fire pipeline for sessions whose
//!   debouncer says go; every STAT_POLL_INTERVAL, stat() active
//!   transcripts for idle-detection; every GC_INTERVAL, sweep stale.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tokio::time::interval;

use crate::debouncer::Trigger;
use crate::hub_client::{HubClient, SessionEndRequest, Wal, WalEntry};
use crate::lifecycle::{LifecycleState, SessionStatus};
use crate::pipeline::{run_digest, DigestInvocation, PipelineConfig};
use crate::registry::SessionRegistry;
use crate::watcher::FsSignal;

pub const TICK_INTERVAL: Duration = Duration::from_secs(5);
pub const STAT_POLL_INTERVAL: Duration = Duration::from_secs(5);
pub const GC_INTERVAL: Duration = Duration::from_secs(300);
pub const IDLE_GRACE: Duration = Duration::from_secs(1800);

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub tick_interval: Duration,
    pub stat_poll_interval: Duration,
    pub gc_interval: Duration,
    pub idle_grace: Duration,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            tick_interval: TICK_INTERVAL,
            stat_poll_interval: STAT_POLL_INTERVAL,
            gc_interval: GC_INTERVAL,
            idle_grace: IDLE_GRACE,
        }
    }
}

/// Process one FS signal. Route to all sessions whose transcript_path
/// canonicalizes to the same as the event path. Multiple sessions per
/// path is the F5 codex-reuse / symlink case.
pub async fn handle_fs_signal(sig: FsSignal, registry: &Arc<Mutex<SessionRegistry>>) {
    let path = match sig {
        FsSignal::Modify(p) | FsSignal::Create(p) => p,
        FsSignal::Remove(_) => return, // transcript deletion -> orchestrator GC handles
    };
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(m) => m,
        Err(_) => return,
    };
    let new_size = metadata.len();
    let mtime = metadata.modified().ok();

    let mut reg = registry.lock().await;
    let keys = reg.keys_by_transcript(&path);
    let now = Instant::now();
    for key in keys {
        if let Some(track) = reg.get_mut(&key) {
            let delta = match &mut track.lifecycle {
                LifecycleState::JsonlAppend { last_seen_size, last_seen_mtime } => {
                    let d = new_size.saturating_sub(*last_seen_size);
                    *last_seen_size = new_size;
                    *last_seen_mtime = mtime;
                    d
                }
                LifecycleState::RewriteHash {
                    last_seen_size,
                    last_seen_mtime,
                    ..
                } => {
                    // F19 optimization: only hash if (mtime, size) changed.
                    // For now, surface a synthetic 1-unit delta to feed the
                    // debouncer; the bash extractor recomputes the hash on
                    // its own invocation.
                    let changed = *last_seen_size != new_size || *last_seen_mtime != mtime;
                    *last_seen_size = new_size;
                    *last_seen_mtime = mtime;
                    if changed { 1 } else { 0 }
                }
                LifecycleState::SharedFilePidLiveness { last_extraction_at, .. } => {
                    *last_extraction_at = Some(std::time::SystemTime::now());
                    1
                }
            };
            if delta > 0 {
                track.debouncer.on_activity(delta, now);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct FireAction {
    project_id: String,
    project_dir: PathBuf,
    window_id: String,
    vendor_session_id: String,
    // Carried for dispatch context; only read via the derived Debug today.
    #[allow(dead_code)]
    host_id: String,
    tool: String,
    transcript_path: PathBuf,
    trigger: Trigger,
}

/// Collect tick actions from all sessions. Returns the list (does NOT
/// dispatch — caller spawns subprocesses on its own time).
pub async fn collect_tick_actions(registry: &Arc<Mutex<SessionRegistry>>) -> Vec<FireAction> {
    let now = Instant::now();
    let mut reg = registry.lock().await;
    reg.iter_mut()
        .filter_map(|(key, track)| {
            track.debouncer.tick(now).map(|trig| FireAction {
                project_id: track.project_id.clone(),
                project_dir: track.project_dir.clone(),
                window_id: key.window_id.clone(),
                vendor_session_id: key.vendor_session_id.clone(),
                host_id: key.host_id.clone(),
                tool: track.tool.clone(),
                transcript_path: track.transcript_path.clone(),
                trigger: trig,
            })
        })
        .collect()
}

/// GC sweep — per v4 §7 #8 four-axis: idle-grace AND pid-dead-with-comm
/// AND size-stable-3-polls AND byte_offset==file_size. Phase 1 uses only
/// the simpler idle-grace axis to avoid pulling in /proc inspection;
/// production hardening tracked as a follow-up.
pub async fn gc_stale_sessions(
    registry: &Arc<Mutex<SessionRegistry>>,
    cfg: &OrchestratorConfig,
    hub: &HubClient,
    wal: &Wal,
) {
    let now = std::time::SystemTime::now();
    let mut to_end: Vec<(crate::key::AiSessionKey, String)> = Vec::new();
    {
        let reg = registry.lock().await;
        for (key, track) in reg.iter() {
            if track.status == SessionStatus::Ended {
                continue;
            }
            // Read last-modified mtime via lifecycle state (JsonlAppend +
            // RewriteHash both carry it; SharedFile uses last_extraction_at).
            let last_seen = match &track.lifecycle {
                LifecycleState::JsonlAppend { last_seen_mtime, .. } => *last_seen_mtime,
                LifecycleState::RewriteHash { last_seen_mtime, .. } => *last_seen_mtime,
                LifecycleState::SharedFilePidLiveness { last_extraction_at, .. } => *last_extraction_at,
            };
            if let Some(t) = last_seen
                && let Ok(elapsed) = now.duration_since(t)
                    && elapsed >= cfg.idle_grace {
                        to_end.push((key.clone(), "idle_timeout".to_string()));
                    }
        }
    }
    for (key, reason) in to_end {
        let req = SessionEndRequest {
            window_id: key.window_id.clone(),
            vendor_session_id: key.vendor_session_id.clone(),
            exit_reason: reason,
            host_id: Some(key.host_id.clone()),
            ended_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        match hub.post_session_end(&req).await {
            Ok(_) => tracing::info!("GC ended session {key}"),
            Err(e) => {
                tracing::warn!("hub session-end failed, queueing to WAL: {e}");
                if let Err(we) = wal.append(&WalEntry::SessionEnd(req)) {
                    tracing::error!("wal append failed: {we}");
                }
            }
        }
        let mut reg = registry.lock().await;
        if let Some(track) = reg.get_mut(&key) {
            track.status = SessionStatus::Ended;
            track.ended_at = Some(std::time::SystemTime::now());
        }
    }
}

/// Dispatch a fired action to the bash extractor as a detached task.
async fn dispatch_action(action: FireAction, cfg: OrchestratorConfig) {
    let pipeline_cfg = {
        let mut c = PipelineConfig::for_workspace(&action.project_dir);
        c.timeout = Duration::from_secs(600);
        c
    };
    let outcome = run_digest(
        &pipeline_cfg,
        DigestInvocation {
            project_id: &action.project_id,
            window_id: &action.window_id,
            tool: &action.tool,
            vendor_session_id: &action.vendor_session_id,
            transcript_path: &action.transcript_path,
            trigger: action.trigger.as_wire(),
            exit_reason: None,
            dry_run: false,
        },
    )
    .await;
    let _ = cfg;
    match outcome {
        Ok(o) if o.exit_code == Some(0) => {
            tracing::info!(
                "digest ok session={} trigger={} dur={:?}",
                action.vendor_session_id,
                action.trigger.as_wire(),
                o.duration
            );
        }
        Ok(o) => tracing::warn!(
            "digest exit={:?} session={} stderr_tail={}",
            o.exit_code,
            action.vendor_session_id,
            o.stderr_tail
        ),
        Err(e) => tracing::warn!("digest spawn failed for {}: {e}", action.vendor_session_id),
    }
}

/// Main event-loop task: route FS signals → debouncer.
pub async fn run_event_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<FsSignal>,
    registry: Arc<Mutex<SessionRegistry>>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.changed() => if *cancel.borrow() { break; },
            sig = rx.recv() => match sig {
                Some(s) => handle_fs_signal(s, &registry).await,
                None => break,
            }
        }
    }
}

/// Main tick task: periodic debouncer.tick + GC.
pub async fn run_tick_loop(
    registry: Arc<Mutex<SessionRegistry>>,
    cfg: OrchestratorConfig,
    hub: HubClient,
    wal: Wal,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    let mut tick = interval(cfg.tick_interval);
    let mut gc = interval(cfg.gc_interval);
    loop {
        tokio::select! {
            biased;
            _ = cancel.changed() => if *cancel.borrow() { break; },
            _ = tick.tick() => {
                let actions = collect_tick_actions(&registry).await;
                for a in actions {
                    let cfg_clone = cfg.clone();
                    tokio::spawn(async move { dispatch_action(a, cfg_clone).await; });
                }
            }
            _ = gc.tick() => gc_stale_sessions(&registry, &cfg, &hub, &wal).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debouncer::Debouncer;
    use crate::key::AiSessionKey;
    use crate::lifecycle::{LifecycleModel, LifecycleState, SessionStatus};
    use crate::registry::SessionTrack;
    use std::time::SystemTime;
    use tempfile::tempdir;

    fn mk_track(window: &str, sid: &str, transcript: PathBuf, project_dir: PathBuf) -> SessionTrack {
        SessionTrack {
            key: AiSessionKey::new(window, sid, "h1"),
            tool: "claude-code".into(),
            transcript_path: transcript,
            project_id: "p".into(),
            project_dir,
            lifecycle: LifecycleState::new(LifecycleModel::JsonlAppend),
            debouncer: Debouncer::new(Default::default(), Instant::now()),
            status: SessionStatus::Active,
            registered_at: SystemTime::now(),
            ended_at: None,
        }
    }

    #[tokio::test]
    async fn fs_signal_routes_to_owning_session() {
        let dir = tempdir().unwrap();
        let t = dir.path().join("a.jsonl");
        std::fs::write(&t, b"hello world").unwrap();
        let reg = Arc::new(Mutex::new(SessionRegistry::new()));
        reg.lock()
            .await
            .insert(mk_track("w1", "s1", t.clone(), dir.path().to_path_buf()));

        handle_fs_signal(FsSignal::Modify(t.clone()), &reg).await;

        let reg = reg.lock().await;
        let key = AiSessionKey::new("w1", "s1", "h1");
        let track = reg.get(&key).unwrap();
        match &track.lifecycle {
            LifecycleState::JsonlAppend { last_seen_size, .. } => {
                assert_eq!(*last_seen_size, 11, "should record file size");
            }
            _ => panic!("expected JsonlAppend"),
        }
    }

    #[tokio::test]
    async fn fs_signal_for_unknown_path_is_noop() {
        let dir = tempdir().unwrap();
        let t = dir.path().join("a.jsonl");
        std::fs::write(&t, b"x").unwrap();
        let reg = Arc::new(Mutex::new(SessionRegistry::new()));
        reg.lock()
            .await
            .insert(mk_track("w1", "s1", t, dir.path().to_path_buf()));

        let unrelated = dir.path().join("elsewhere.jsonl");
        std::fs::write(&unrelated, b"x").unwrap();
        handle_fs_signal(FsSignal::Modify(unrelated), &reg).await;

        let reg = reg.lock().await;
        let key = AiSessionKey::new("w1", "s1", "h1");
        let track = reg.get(&key).unwrap();
        match &track.lifecycle {
            LifecycleState::JsonlAppend { last_seen_size, .. } => assert_eq!(*last_seen_size, 0),
            _ => panic!("expected JsonlAppend"),
        }
    }

    #[tokio::test]
    async fn collect_tick_actions_empty_for_fresh_registry() {
        let reg = Arc::new(Mutex::new(SessionRegistry::new()));
        let dir = tempdir().unwrap();
        let t = dir.path().join("a.jsonl");
        std::fs::write(&t, b"x").unwrap();
        reg.lock().await.insert(mk_track("w1", "s1", t, dir.path().to_path_buf()));

        let actions = collect_tick_actions(&reg).await;
        assert!(actions.is_empty(), "fresh session should not fire");
    }
}
