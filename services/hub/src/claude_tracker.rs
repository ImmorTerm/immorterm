//! Claude sync orchestrator — port of `apps/extension/src/claude-sync.ts`.
//!
//! Runs a 30-second loop that:
//!   1. Tells SessionManager to poll C-binary claude-ctx files once (batch).
//!   2. Rolls every adapter's cached ClaudeState into registry.json in one
//!      read-modify-write via `routes::registry::batch_sync_claude_state`.
//!   3. Fires a non-blocking heartbeat to the memory service's
//!      `/api/v1/sessions/heartbeat`.
//!
//! It deliberately delegates the polling, WS state accumulation, and stats
//! formatting to SessionManager (which already owns those) so we don't
//! duplicate logic. The lifecycle checks for memory / mcp-gateway that the
//! TS version also runs are deferred to Phase 5 of the migration plan —
//! hub already probes both services on every /api/v1/services call, and
//! exposes that to the webview for the same data.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use serde_json::json;
use tokio::time::interval;

use crate::routes::registry::{
    batch_sync_claude_state, ClaudeStatsSnapshot, ClaudeSyncUpdate,
};
use crate::session_manager::SessionManager;

pub fn start(manager: Arc<SessionManager>) {
    tokio::spawn(async move {
        let http = match Client::builder().timeout(Duration::from_secs(3)).build() {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut t = interval(Duration::from_secs(30));
        loop {
            t.tick().await;
            tick(&manager, &http).await;
        }
    });
}

async fn tick(manager: &SessionManager, http: &Client) {
    // 1. Batch-refresh regular (C-binary) context files. AI adapters push
    //    via WS and don't need a periodic poke.
    manager.poll_all_context_files().await;

    // 2. Snapshot per-window state and reduce to registry update entries.
    let states = manager.all_claude_states().await;
    let total_sessions = manager.all_sessions().await.len();
    tracing::info!(
        "[claude-tracker] tick: {} adapter(s), {} active claude state(s)",
        total_sessions,
        states.len(),
    );
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut updates: Vec<ClaudeSyncUpdate> = Vec::with_capacity(states.len());
    let mut heartbeat_sessions: Vec<serde_json::Value> = Vec::new();
    for (window_id, state) in &states {
        let active = state.active && state.session_id.is_some();
        let stats = if active {
            Some(ClaudeStatsSnapshot {
                pid: state.pid.unwrap_or(0),
                rss_kb: state.rss_kb,
                cpu_percent: state.cpu_percent,
                start_time: now_secs.saturating_sub(state.runtime_secs),
                runtime_secs: state.runtime_secs,
            })
        } else { None };
        updates.push(ClaudeSyncUpdate {
            window_id: window_id.clone(),
            active,
            session_id: state.session_id.clone(),
            transcript_path: state.transcript_path.clone(),
            stats,
            // Heartbeat sync doesn't know which AI tool is driving the
            // window — that's set once via `POST /api/v1/registry/session-link`
            // when the hook fires. None = leave `tool` on disk untouched.
            tool: None,
        });
        if active {
            heartbeat_sessions.push(json!({
                "session_id": state.session_id.as_deref().unwrap_or(""),
                "terminal_name": window_id,
            }));
        }
    }
    if let Err(e) = batch_sync_claude_state(&updates) {
        tracing::warn!("[claude-tracker] batch sync failed: {}", e);
    }

    // 3. Heartbeat to memory service — fire-and-forget.
    if heartbeat_sessions.is_empty() { return; }
    let Some(memory_url) = crate::config::discover_memory_url() else { return };
    let url = format!("{}/api/v1/sessions/heartbeat", memory_url.trim_end_matches('/'));
    let body = json!({ "sessions": heartbeat_sessions });
    let http = http.clone();
    tokio::spawn(async move {
        if let Err(e) = http.post(&url).json(&body).send().await {
            tracing::debug!("[claude-tracker] heartbeat skipped: {}", e);
        }
    });
}
