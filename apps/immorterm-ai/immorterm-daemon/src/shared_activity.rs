//! Durable per-session audit log for Interactive sharing.
//!
//! The live chat overlay is intentionally ephemeral. This log is the source
//! for the discoverable Shared Activity panel and survives unpair/restart.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
static EVENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedActivityEvent {
    pub id: String,
    pub timestamp: u64,
    pub event: String,
    pub direction: String,
    pub peer_id: String,
    pub peer_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_mode: Option<String>,
    pub status: String,
}

impl SharedActivityEvent {
    pub fn new(event: &str, direction: &str, peer_id: &str, peer_name: &str) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            id: format!(
                "{}-{}-{}",
                std::process::id(),
                timestamp,
                EVENT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            timestamp,
            event: event.into(),
            direction: direction.into(),
            peer_id: peer_id.into(),
            peer_name: peer_name.into(),
            message: None,
            file_path: None,
            media_type: None,
            delivery_mode: None,
            status: "delivered".into(),
        }
    }
}

fn log_path(window_id: &str) -> Option<std::path::PathBuf> {
    if window_id.is_empty()
        || !window_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".immorterm")
            .join("shared-activity")
            .join(format!("{}.jsonl", window_id)),
    )
}

pub fn append(window_id: &str, event: &SharedActivityEvent) -> Result<(), String> {
    let path = log_path(window_id).ok_or("invalid session id for shared activity")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if path.metadata().map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES {
        let archived = path.with_extension("jsonl.1");
        let _ = std::fs::remove_file(&archived);
        std::fs::rename(&path, archived).map_err(|e| e.to_string())?;
    }
    let mut line = serde_json::to_vec(event).map_err(|e| e.to_string())?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(&line).map_err(|e| e.to_string())
}

/// Read the newest activity entries, including the immediately previous
/// rotated segment. Malformed or partially-written JSONL lines are skipped.
pub fn load_recent(window_id: &str, limit: usize) -> Result<Vec<SharedActivityEvent>, String> {
    let path = log_path(window_id).ok_or("invalid session id for shared activity")?;
    let mut events = Vec::new();
    for candidate in [path.with_extension("jsonl.1"), path] {
        let content = match std::fs::read_to_string(candidate) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        events.extend(
            content
                .lines()
                .filter_map(|line| serde_json::from_str::<SharedActivityEvent>(line).ok()),
        );
    }
    let keep_from = events.len().saturating_sub(limit);
    Ok(events.split_off(keep_from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ids_that_escape_the_activity_directory() {
        assert!(log_path("").is_none());
        assert!(log_path("../../other").is_none());
        assert!(log_path("12345-abcd_ef").is_some());
    }
}

