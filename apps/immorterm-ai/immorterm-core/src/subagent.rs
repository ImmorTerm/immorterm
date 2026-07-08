//! Claude Code spawned subagent data model.
//!
//! Subagents are created by the `Task` tool and produce JSONL transcripts
//! at `~/.claude/projects/*/subagents/*.jsonl`. Unlike team members, they
//! cannot receive messages — they're read-only from ImmorTerm's perspective.

use serde::{Deserialize, Serialize};

/// A Claude Code spawned subagent (from Task tool, not a team member).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentInfo {
    /// Short hex ID (e.g., "a7e6d2b") — from filename.
    pub agent_id: String,
    /// Human-readable slug (e.g., "nifty-twirling-waffle") — from first JSONL line.
    pub slug: String,
    /// Parent Claude session UUID that spawned this agent.
    pub parent_session_id: String,
    /// Full path to JSONL transcript file.
    pub transcript_path: String,
    /// First message timestamp (Unix seconds).
    pub started_at: u64,
    /// Last message timestamp (Unix seconds).
    pub last_activity: u64,
    /// Number of JSONL lines (roughly = turns).
    pub message_count: usize,
    /// Inferred status.
    pub status: SubagentStatus,
}

/// Subagent status inferred from transcript file activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    /// JSONL file still being written to (modified < 30s ago).
    Running,
    /// No new writes for > 30s.
    Completed,
    /// Cannot determine status.
    Unknown,
}

impl SubagentStatus {
    /// Display label for pane chrome.
    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Unknown => "Unknown",
        }
    }
}

/// Events emitted by the subagent watcher.
#[derive(Debug, Clone)]
pub enum SubagentEvent {
    /// A new subagent JSONL file was detected.
    Detected(SubagentInfo),
    /// An existing subagent's state was updated (new lines, status change).
    Updated(SubagentInfo),
    /// A new line was appended to a subagent's transcript.
    NewTranscriptLine {
        agent_id: String,
        line: String,
    },
    /// A subagent completed (no writes for > 30s).
    Completed(String), // agent_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subagent_status_labels() {
        assert_eq!(SubagentStatus::Running.display_label(), "Running");
        assert_eq!(SubagentStatus::Completed.display_label(), "Completed");
        assert_eq!(SubagentStatus::Unknown.display_label(), "Unknown");
    }

    #[test]
    fn test_subagent_info_serde() {
        let info = SubagentInfo {
            agent_id: "a7e6d2b".into(),
            slug: "nifty-twirling-waffle".into(),
            parent_session_id: "abc-123-def".into(),
            transcript_path: "/tmp/test.jsonl".into(),
            started_at: 1000,
            last_activity: 2000,
            message_count: 42,
            status: SubagentStatus::Running,
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: SubagentInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_id, "a7e6d2b");
        assert_eq!(parsed.status, SubagentStatus::Running);
    }
}
