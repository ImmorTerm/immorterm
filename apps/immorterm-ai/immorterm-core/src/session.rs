//! Session metadata — serializable state for persistence.

use serde::{Deserialize, Serialize};

/// Session metadata for persistence and display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Session name (e.g., "project-windowId")
    pub name: String,
    /// PID of the daemon process managing this session
    pub pid: u32,
    /// Whether a client is currently attached
    pub attached: bool,
    /// Window title (set via OSC 0/2 or -X title)
    pub title: String,
    /// Number of columns
    pub cols: usize,
    /// Number of rows
    pub rows: usize,
    /// Scrollback buffer max lines
    pub scrollback_max: usize,
    /// Environment variables set via -X setenv
    pub env: std::collections::HashMap<String, String>,
    /// Log file path (if logging is enabled)
    pub log_file: Option<String>,
    /// Whether logging is active
    pub logging: bool,
    /// Unix timestamp of session creation
    pub created_at: u64,
}

impl SessionMeta {
    pub fn new(name: String, pid: u32, cols: usize, rows: usize) -> Self {
        Self {
            name,
            pid,
            attached: false,
            title: String::new(),
            cols,
            rows,
            scrollback_max: 50_000,
            env: std::collections::HashMap::new(),
            log_file: None,
            logging: false,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}
