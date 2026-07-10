//! Background task that pushes terminal log events to the OpenMemory REST API.
//!
//! Events flow: AiExtractor → mpsc channel → batch buffer → HTTP POST.
//! The channel decouples the hot PTY path from network I/O. Events are
//! batched and flushed every 10 seconds or when the buffer reaches 20 items.

use serde::Serialize;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Terminal log event matching the OpenMemory `terminal_logs` schema.
///
/// Screenshot ephemerality (BROKER-DESIGN.md): this event carries only text
/// (`content`) — there is no image/PNG field, and no code path feeds a browser
/// screenshot in here. Self-driven-browser frames are therefore structurally
/// excluded from the OpenMemory push; nothing in this file ever sees a browser
/// PNG. Keep it that way — do not add a binary/base64 field that a browser
/// frame could reach.
#[derive(Debug, Serialize, Clone)]
pub struct TerminalLogEvent {
    pub session_name: String,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_visible: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub timestamp: String,
}

/// Batch payload for `POST /api/v1/terminal-logs/batch`.
#[derive(Serialize)]
struct BatchPayload {
    user_id: String,
    events: Vec<TerminalLogEvent>,
}

const OPENMEMORY_BATCH_URL: &str = "http://localhost:8765/api/v1/terminal-logs/batch";
const BATCH_SIZE: usize = 20;
const FLUSH_INTERVAL_SECS: u64 = 10;

/// Channel capacity — 256 events before back-pressure.
pub const CHANNEL_CAPACITY: usize = 256;

/// Spawn the background push task. Returns a sender for queueing events.
///
/// Must be called inside a tokio runtime (the daemon event loop).
pub fn spawn_push_task(rx: mpsc::Receiver<TerminalLogEvent>, user_id: String) {
    tokio::spawn(push_loop(rx, user_id));
}

async fn push_loop(mut rx: mpsc::Receiver<TerminalLogEvent>, user_id: String) {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .pool_max_idle_per_host(0) // Don't hold keep-alive connections — prevents lsof port-kill collateral
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut buffer: Vec<TerminalLogEvent> = Vec::with_capacity(BATCH_SIZE);
    let mut flush_timer =
        tokio::time::interval(std::time::Duration::from_secs(FLUSH_INTERVAL_SECS));
    // Don't fire immediately on startup
    flush_timer.tick().await;

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(evt) => {
                        buffer.push(evt);
                        if buffer.len() >= BATCH_SIZE {
                            flush_batch(&client, &user_id, &mut buffer).await;
                        }
                    }
                    None => {
                        // Channel closed — final flush and exit
                        if !buffer.is_empty() {
                            flush_batch(&client, &user_id, &mut buffer).await;
                        }
                        debug!("OpenMemory push task shutting down");
                        return;
                    }
                }
            }
            _ = flush_timer.tick() => {
                if !buffer.is_empty() {
                    flush_batch(&client, &user_id, &mut buffer).await;
                }
            }
        }
    }
}

async fn flush_batch(
    client: &reqwest::Client,
    user_id: &str,
    buffer: &mut Vec<TerminalLogEvent>,
) {
    let count = buffer.len();
    let payload = BatchPayload {
        user_id: user_id.to_string(),
        events: std::mem::take(buffer),
    };

    match client.post(OPENMEMORY_BATCH_URL).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {
            debug!("Pushed {} terminal log events to OpenMemory", count);
        }
        Ok(resp) => {
            warn!(
                "OpenMemory push failed ({})",
                resp.status()
            );
        }
        Err(e) => {
            // Silently degrade — OpenMemory may not be running
            debug!("OpenMemory push error (service may be offline): {}", e);
        }
    }
}
