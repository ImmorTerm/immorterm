//! Process-wide event channel. Shared by SessionManager (claude updates)
//! and the task-file watcher so webviews subscribe to a single stream.
//!
//! DRY: rather than a second broadcast channel for every event source,
//! both publishers hand off to this OnceLock. A future /api/v1/events
//! WebSocket endpoint will subscribe once and fan frames out.

use std::sync::OnceLock;

use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum HubEvent {
    /// Claude state changed for a given window.
    ClaudeUpdate {
        window_id: String,
        state: crate::session_manager::ClaudeState,
    },
    ClaudeExited { window_id: String },
    SessionClosing { window_id: String },
    SessionAdded { info: crate::session_manager::SessionInfo },
    SessionRemoved { window_id: String },
    /// Tasks file changed externally (MCP write, manual edit). Carries
    /// the project_id so webviews filter by the project they care about.
    TasksChanged { project_id: String },
}

static EVENTS: OnceLock<broadcast::Sender<HubEvent>> = OnceLock::new();

pub fn sender() -> broadcast::Sender<HubEvent> {
    EVENTS
        .get_or_init(|| broadcast::channel(256).0)
        .clone()
}

/// Fire-and-forget publish. Err ignored — no subscribers is fine.
pub fn publish(ev: HubEvent) {
    let _ = sender().send(ev);
}

pub fn subscribe() -> broadcast::Receiver<HubEvent> {
    sender().subscribe()
}
