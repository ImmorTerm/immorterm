//! Channel registry — routes messages between paired ImmorTerm sessions.
//!
//! Each daemon instance manages channels for its own session. Cross-session
//! routing uses file-based IPC: messages are written to the target's inbox
//! at `~/.immorterm/channel-inbox/{immorterm_id}.jsonl`, and a file watcher
//! picks them up on the other side.
//!
//! This mirrors the team messaging pattern (team_watcher.rs) and avoids
//! needing shared state between daemon processes.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// A channel message exchanged between sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub from_immorterm_id: String,
    pub from_name: String,
    pub message: String,
    pub timestamp: u64,
}

/// Pairing state for interactive session sharing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelPairing {
    pub partner_id: String,
    pub partner_name: String,
}

/// Per-session channel state managed by the daemon.
pub struct ChannelState {
    /// This session's immorterm_id.
    immorterm_id: String,
    /// Channel server WebSocket sender (if connected).
    channel_sender: Option<mpsc::Sender<String>>,
    /// Active pairing (if interactive share is active).
    pairing: Option<ChannelPairing>,
    /// Inbox directory.
    inbox_dir: PathBuf,
}

impl ChannelState {
    pub fn new(immorterm_id: String) -> Self {
        let inbox_dir = Self::inbox_dir_path();
        Self {
            immorterm_id,
            channel_sender: None,
            pairing: None,
            inbox_dir,
        }
    }

    fn inbox_dir_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".immorterm").join("channel-inbox")
    }

    /// Register a channel server's WebSocket sender.
    pub fn register(&mut self, sender: mpsc::Sender<String>) {
        info!("Channel registered for session {}", self.immorterm_id);
        self.channel_sender = Some(sender);
    }

    /// Unregister the channel server (Claude exited).
    pub fn unregister(&mut self) {
        info!("Channel unregistered for session {}", self.immorterm_id);
        self.channel_sender = None;
    }

    /// Check if a channel server is connected.
    pub fn is_registered(&self) -> bool {
        self.channel_sender.is_some()
    }

    /// Set the interactive pairing.
    pub fn pair(&mut self, partner_id: String, partner_name: String) {
        info!(
            "Session {} paired with {} ({})",
            self.immorterm_id, partner_name, partner_id
        );
        self.pairing = Some(ChannelPairing {
            partner_id,
            partner_name,
        });
    }

    /// Clear the pairing.
    pub fn unpair(&mut self) -> Option<ChannelPairing> {
        if let Some(p) = self.pairing.take() {
            info!(
                "Session {} unpaired from {} ({})",
                self.immorterm_id, p.partner_name, p.partner_id
            );
            Some(p)
        } else {
            None
        }
    }

    /// Get the current pairing.
    pub fn pairing(&self) -> Option<&ChannelPairing> {
        self.pairing.as_ref()
    }

    /// Send a message to the paired session via file-based IPC.
    pub fn send_to_partner(&self, message: &str) -> Result<(), String> {
        let pairing = self.pairing.as_ref().ok_or("No active pairing")?;

        let msg = ChannelMessage {
            from_immorterm_id: self.immorterm_id.clone(),
            from_name: String::new(), // Filled by daemon with session display name
            message: message.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };

        write_to_inbox(&self.inbox_dir, &pairing.partner_id, &msg)
    }

    /// Send a message to a specific session (for pairing notifications).
    pub fn send_to_session(&self, target_id: &str, message: &str) -> Result<(), String> {
        let msg = ChannelMessage {
            from_immorterm_id: self.immorterm_id.clone(),
            from_name: String::new(),
            message: message.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };

        write_to_inbox(&self.inbox_dir, target_id, &msg)
    }

    /// Forward a received message to the channel server.
    pub async fn forward_to_channel(&self, msg: &ChannelMessage) {
        if let Some(sender) = &self.channel_sender {
            let json = match serde_json::to_string(&serde_json::json!({
                "type": "channel_message",
                "from_immorterm_id": msg.from_immorterm_id,
                "from_name": msg.from_name,
                "message": msg.message,
            })) {
                Ok(j) => j,
                Err(e) => {
                    error!("Failed to serialize channel message: {}", e);
                    return;
                }
            };
            if sender.send(json).await.is_err() {
                warn!("Channel server disconnected for session {}", self.immorterm_id);
            }
        }
    }

    /// Notify the channel server about a pairing event.
    pub async fn notify_paired(&self, partner_id: &str, partner_name: &str) {
        if let Some(sender) = &self.channel_sender {
            let json = serde_json::to_string(&serde_json::json!({
                "type": "session_paired",
                "partner_id": partner_id,
                "partner_name": partner_name,
            }))
            .unwrap_or_default();
            let _ = sender.send(json).await;
        }
    }

    /// Notify the channel server about an unpairing event.
    pub async fn notify_unpaired(&self) {
        if let Some(sender) = &self.channel_sender {
            let json = serde_json::to_string(&serde_json::json!({
                "type": "session_unpaired",
            }))
            .unwrap_or_default();
            let _ = sender.send(json).await;
        }
    }
}

/// Write a message to a session's inbox file.
pub fn write_to_inbox(
    inbox_dir: &Path,
    target_id: &str,
    msg: &ChannelMessage,
) -> Result<(), String> {
    // Ensure directory exists
    std::fs::create_dir_all(inbox_dir).map_err(|e| format!("mkdir failed: {}", e))?;

    let inbox_path = inbox_dir.join(format!("{}.jsonl", target_id));
    let mut line = serde_json::to_string(msg).map_err(|e| format!("serialize failed: {}", e))?;
    line.push('\n');

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&inbox_path)
        .map_err(|e| format!("open inbox failed: {}", e))?;
    file.write_all(line.as_bytes())
        .map_err(|e| format!("write failed: {}", e))?;

    Ok(())
}

/// Start watching this session's inbox for incoming messages.
/// Returns a receiver that yields ChannelMessage values.
pub fn start_inbox_watcher(
    immorterm_id: &str,
) -> mpsc::Receiver<ChannelMessage> {
    let (tx, rx) = mpsc::channel(32);
    let inbox_dir = ChannelState::inbox_dir_path();
    let inbox_file = inbox_dir.join(format!("{}.jsonl", immorterm_id));
    let id = immorterm_id.to_string();

    tokio::spawn(async move {
        // Ensure directory exists
        let _ = std::fs::create_dir_all(&inbox_dir);

        // Track file position to only read new lines
        let mut last_pos: u64 = if inbox_file.exists() {
            // Start from end of existing file (don't replay old messages)
            std::fs::metadata(&inbox_file)
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            0
        };

        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            if !inbox_file.exists() {
                continue;
            }

            let file_len = match std::fs::metadata(&inbox_file) {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            if file_len <= last_pos {
                // File was truncated or no new data
                if file_len < last_pos {
                    last_pos = 0; // Reset on truncation
                }
                continue;
            }

            // Read new lines
            match std::fs::read_to_string(&inbox_file) {
                Ok(contents) => {
                    let new_content = if last_pos as usize <= contents.len() {
                        &contents[last_pos as usize..]
                    } else {
                        &contents
                    };

                    for line in new_content.lines() {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<ChannelMessage>(trimmed) {
                            Ok(msg) => {
                                if tx.send(msg).await.is_err() {
                                    info!("Inbox watcher for {} stopping (receiver dropped)", id);
                                    return;
                                }
                            }
                            Err(e) => {
                                warn!("Malformed inbox message for {}: {}", id, e);
                            }
                        }
                    }
                    last_pos = file_len;
                }
                Err(e) => {
                    warn!("Failed to read inbox for {}: {}", id, e);
                }
            }
        }
    });

    rx
}
