//! Per-vendor lifecycle strategy.
//!
//! Per v4 §2.1+§2.2 (F1 fix), three lifecycle models cover the 9
//! supported vendors:
//!
//! | Model                       | Vendors                                | Mechanism                        |
//! |---|---|---|
//! | `JsonlAppend`               | claude-code, codex, windsurf, opencode, cursor, copilot | mtime + last_seen_size delta |
//! | `RewriteHash`               | cline, gemini                          | hash(file) + msg_count delta     |
//! | `SharedFilePidLiveness`     | aider                                  | PID-alive + idle-since-extraction |
//!
//! Each lifecycle state stores only the bookkeeping its model needs.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LifecycleModel {
    /// Transcript is append-only JSONL. Burst signal = size delta.
    JsonlAppend,
    /// Transcript is rewritten on every update; size non-monotonic.
    /// Burst signal = hash change + message-count delta.
    RewriteHash,
    /// Transcript is shared across multiple concurrent sessions in the
    /// same project (aider). File-mtime can't identify which session.
    /// Lifecycle = vendor PID liveness + idle-since-last-extraction.
    SharedFilePidLiveness,
}

impl LifecycleModel {
    /// Pick the lifecycle model for a vendor identifier as it appears in
    /// `RegistryEntry.tool`. Unknown vendors default to `JsonlAppend` —
    /// that's the safest assumption for an adapter we haven't classified
    /// (we'll see what the file looks like and adjust).
    pub fn for_vendor(tool: &str) -> Self {
        match tool {
            "cline" | "gemini" => Self::RewriteHash,
            "aider" => Self::SharedFilePidLiveness,
            // claude-code, codex, windsurf, opencode, cursor, copilot, and
            // any unknown.
            _ => Self::JsonlAppend,
        }
    }
}

/// State stored by the daemon per session, gated by lifecycle model.
#[derive(Debug, Clone)]
pub enum LifecycleState {
    JsonlAppend {
        last_seen_size: u64,
        last_seen_mtime: Option<SystemTime>,
    },
    RewriteHash {
        last_hash: Option<String>,
        last_msg_count: u32,
        last_seen_mtime: Option<SystemTime>,
        last_seen_size: u64, // for skip-when-(mtime,size)-unchanged optimization (F19)
    },
    SharedFilePidLiveness {
        vendor_pid: Option<u32>,
        vendor_comm: Option<String>, // for PID-recycling defense
        last_extraction_at: Option<SystemTime>,
    },
}

impl LifecycleState {
    pub fn new(model: LifecycleModel) -> Self {
        match model {
            LifecycleModel::JsonlAppend => Self::JsonlAppend {
                last_seen_size: 0,
                last_seen_mtime: None,
            },
            LifecycleModel::RewriteHash => Self::RewriteHash {
                last_hash: None,
                last_msg_count: 0,
                last_seen_mtime: None,
                last_seen_size: 0,
            },
            LifecycleModel::SharedFilePidLiveness => Self::SharedFilePidLiveness {
                vendor_pid: None,
                vendor_comm: None,
                last_extraction_at: None,
            },
        }
    }

    pub fn model(&self) -> LifecycleModel {
        match self {
            Self::JsonlAppend { .. } => LifecycleModel::JsonlAppend,
            Self::RewriteHash { .. } => LifecycleModel::RewriteHash,
            Self::SharedFilePidLiveness { .. } => LifecycleModel::SharedFilePidLiveness,
        }
    }
}

/// Status of a session as observed by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    Active,
    IdleGrace,
    Ended,
}

/// Reason for ending. Mirrors v4 §3.5's allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    IdleTimeout,
    PidDead,
    Superseded,
    HookSessionEnd,
    HookStop,
    SizeStable,
}

impl ExitReason {
    pub fn as_wire(&self) -> &'static str {
        match self {
            Self::IdleTimeout => "idle_timeout",
            Self::PidDead => "pid_dead",
            Self::Superseded => "superseded",
            Self::HookSessionEnd => "hook_session_end",
            Self::HookStop => "hook_stop",
            Self::SizeStable => "size_stable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cline_and_gemini_are_rewrite_hash() {
        assert_eq!(LifecycleModel::for_vendor("cline"), LifecycleModel::RewriteHash);
        assert_eq!(LifecycleModel::for_vendor("gemini"), LifecycleModel::RewriteHash);
    }

    #[test]
    fn aider_is_shared_file() {
        assert_eq!(
            LifecycleModel::for_vendor("aider"),
            LifecycleModel::SharedFilePidLiveness
        );
    }

    #[test]
    fn jsonl_vendors_are_append() {
        for v in &["claude-code", "codex", "windsurf", "opencode", "cursor", "copilot"] {
            assert_eq!(
                LifecycleModel::for_vendor(v),
                LifecycleModel::JsonlAppend,
                "{v} should be JsonlAppend"
            );
        }
    }

    #[test]
    fn unknown_vendor_defaults_to_jsonl() {
        assert_eq!(
            LifecycleModel::for_vendor("nonexistent-tool"),
            LifecycleModel::JsonlAppend
        );
    }

    #[test]
    fn state_new_matches_model() {
        for model in &[
            LifecycleModel::JsonlAppend,
            LifecycleModel::RewriteHash,
            LifecycleModel::SharedFilePidLiveness,
        ] {
            let s = LifecycleState::new(*model);
            assert_eq!(s.model(), *model);
        }
    }

    #[test]
    fn exit_reason_wire_format() {
        assert_eq!(ExitReason::IdleTimeout.as_wire(), "idle_timeout");
        assert_eq!(ExitReason::HookSessionEnd.as_wire(), "hook_session_end");
        assert_eq!(ExitReason::SizeStable.as_wire(), "size_stable");
    }
}
