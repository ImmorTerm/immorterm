//! The primary identifier types for the digest daemon.
//!
//! Per v4 §1.1, the unit of registration is `AiSessionKey = (window_id,
//! vendor_session_id, host_id)`. `window_id` alone collapses compaction
//! (loses prior conversation). `vendor_session_id` alone loses the window
//! grouping. The tuple is exactly what `tool_history` already encodes
//! per registry.json — one row per `AiSessionKey`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// THE digest unit. One row in any history, one debouncer, one in-flight
/// subprocess, one extractor invocation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AiSessionKey {
    pub window_id: String,
    pub vendor_session_id: String,
    pub host_id: String,
}

impl AiSessionKey {
    pub fn new(window_id: impl Into<String>, vendor_session_id: impl Into<String>, host_id: impl Into<String>) -> Self {
        Self {
            window_id: window_id.into(),
            vendor_session_id: vendor_session_id.into(),
            host_id: host_id.into(),
        }
    }
}

impl fmt::Display for AiSessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // host_id intentionally first so logs sort host-cluster-aware.
        write!(f, "{}/{}/{}", self.host_id, self.window_id, self.vendor_session_id)
    }
}

/// Resolve the local machine's stable host identifier. Order:
/// 1. `/etc/machine-id` (Linux)
/// 2. `/var/lib/dbus/machine-id` (older Linux)
/// 3. `gethostname()` + per-process startup-nonce fallback (macOS, anywhere else)
///
/// Per v4 §1.1 + §3.4, the host_id is required on every session-link /
/// session-end request so multi-host federation can disambiguate.
pub fn resolve_host_id() -> String {
    for path in &["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(s) = std::fs::read_to_string(path) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    // Fallback: hostname + startup-time nonce. Stable for the daemon's
    // lifetime, distinct from other hosts running the same hostname IF
    // they happen to share one (e.g., "MacBook-Pro.local" duplicates).
    let host = hostname_via_libc().unwrap_or_else(|| "unknown-host".to_string());
    let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    format!("{host}-{nonce:x}")
}

fn hostname_via_libc() -> Option<String> {
    let mut buf = vec![0u8; 256];
    // SAFETY: gethostname writes up to len bytes (NUL-terminated if it fits).
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) };
    if rc != 0 {
        return None;
    }
    // Find first NUL.
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..len].to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_format_is_host_window_session() {
        let k = AiSessionKey::new("11111-aaaa", "uuid-1", "machine-id-abc");
        assert_eq!(format!("{k}"), "machine-id-abc/11111-aaaa/uuid-1");
    }

    #[test]
    fn equality_is_field_wise() {
        let a = AiSessionKey::new("w", "s", "h");
        let b = AiSessionKey::new("w", "s", "h");
        let c = AiSessionKey::new("w", "s", "h2");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn serde_roundtrips_struct() {
        let a = AiSessionKey::new("w", "s", "h");
        let s = serde_json::to_string(&a).unwrap();
        let b: AiSessionKey = serde_json::from_str(&s).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn resolve_host_id_returns_nonempty() {
        let id = resolve_host_id();
        assert!(!id.is_empty(), "host_id must be nonempty");
        // Subsequent call may differ if it took the nonce fallback path
        // (timestamp-based). Just check it's stable on Linux /etc/machine-id.
        if std::path::Path::new("/etc/machine-id").exists() {
            assert_eq!(id, resolve_host_id(), "/etc/machine-id should be stable");
        }
    }
}
