//! Claude Code Agent Teams data model.
//!
//! Parses team configs, tasks, and inbox messages from the JSON files
//! that Claude Code writes to `~/.claude/teams/` and `~/.claude/tasks/`.
//! WASM-compatible — no I/O, just serde parsing.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Team Configuration ─────────────────────────────────────────────

/// A Claude Code team (from `~/.claude/teams/{name}/config.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub lead_agent_id: String,
    #[serde(default)]
    pub lead_session_id: String,
    #[serde(default)]
    pub members: Vec<TeamMember>,
}

/// A member in a Claude Code team.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    pub agent_id: String,
    pub name: String,
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub joined_at: u64,
    #[serde(default)]
    pub tmux_pane_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub backend_type: Option<String>,
    #[serde(default)]
    pub plan_mode_required: bool,
}

impl TeamMember {
    /// Parse the CSS color string (e.g., "blue", "#FF6B6B") to RGBA [0..1].
    pub fn color_rgba(&self) -> [f32; 4] {
        match self.color.as_deref() {
            Some("blue") => [0.4, 0.6, 1.0, 1.0],
            Some("green") => [0.4, 0.9, 0.5, 1.0],
            Some("yellow") => [1.0, 0.85, 0.3, 1.0],
            Some("purple") | Some("magenta") => [0.8, 0.5, 1.0, 1.0],
            Some("red") => [1.0, 0.4, 0.4, 1.0],
            Some("cyan") => [0.3, 0.9, 0.9, 1.0],
            Some("orange") => [1.0, 0.6, 0.2, 1.0],
            Some(hex) if hex.starts_with('#') && hex.len() == 7 => {
                let r = u8::from_str_radix(&hex[1..3], 16).unwrap_or(128) as f32 / 255.0;
                let g = u8::from_str_radix(&hex[3..5], 16).unwrap_or(128) as f32 / 255.0;
                let b = u8::from_str_radix(&hex[5..7], 16).unwrap_or(128) as f32 / 255.0;
                [r, g, b, 1.0]
            }
            _ => [0.7, 0.7, 0.7, 1.0], // default gray
        }
    }

    /// Is this the team lead?
    pub fn is_lead(&self) -> bool {
        self.agent_type == "team-lead"
    }
}

// ── Tasks ──────────────────────────────────────────────────────────

/// Task status enum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Deleted,
}

/// A task in the shared task list (from `~/.claude/tasks/{name}/{id}.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamTask {
    pub id: String,
    pub subject: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub active_form: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

impl TeamTask {
    /// Short display label: "[x] Task subject" or "[>] Running..."
    pub fn display_label(&self) -> String {
        let icon = match self.status {
            TaskStatus::Completed => "\u{2713}", // checkmark
            TaskStatus::InProgress => "\u{25B6}", // play
            TaskStatus::Pending => "\u{25CB}",    // circle
            TaskStatus::Deleted => "\u{2717}",    // X
        };
        format!("{} {}", icon, self.subject)
    }
}

// ── Inbox Messages ─────────────────────────────────────────────────

/// A message in a team inbox (from `~/.claude/teams/{name}/inboxes/{agent}.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessage {
    pub from: String,
    pub text: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub read: bool,
}

impl TeamMessage {
    /// Is this an idle notification (JSON blob with type: "idle_notification")?
    pub fn is_idle_notification(&self) -> bool {
        self.text.contains("\"type\":\"idle_notification\"")
    }

    /// Display summary or first 80 chars of text.
    pub fn display_text(&self) -> &str {
        if let Some(ref s) = self.summary {
            s.as_str()
        } else if self.text.len() > 80 {
            &self.text[..80]
        } else {
            &self.text
        }
    }
}

// ── Permission Mode ───────────────────────────────────────────────

/// Claude Code permission mode (controls what tools the team lead can use).
///
/// In `Delegate` mode, the lead is restricted to coordination-only tools
/// (SendMessage, Task*, TeamCreate), forcing actual delegation to teammates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    Default,
    Plan,
    AcceptEdits,
    DontAsk,
    BypassPermissions,
    Delegate,
}

impl PermissionMode {
    /// Parse from the string value Claude Code uses in hook events and statusLine.
    pub fn from_str_value(s: &str) -> Self {
        match s {
            "plan" => Self::Plan,
            "acceptEdits" => Self::AcceptEdits,
            "dontAsk" => Self::DontAsk,
            "bypassPermissions" => Self::BypassPermissions,
            "delegate" => Self::Delegate,
            _ => Self::Default,
        }
    }

    /// Whether this is delegate mode (team lead restricted to coordination tools).
    pub fn is_delegate(&self) -> bool {
        matches!(self, Self::Delegate)
    }

    /// String label for display in status bar / pane chrome.
    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::AcceptEdits => "acceptEdits",
            Self::DontAsk => "dontAsk",
            Self::BypassPermissions => "bypassPermissions",
            Self::Delegate => "delegate",
        }
    }
}

// ── Team Lifecycle ────────────────────────────────────────────────

/// Team-level lifecycle state (derived from tasks + activity).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamLifecycle {
    /// Has in_progress tasks or recent activity (< 5 min).
    #[default]
    Active,
    /// All members idle, no in_progress tasks, but has pending tasks.
    Idle,
    /// All non-deleted tasks completed.
    Done,
    /// No file activity for > 1 hour.
    Stale,
}

impl TeamLifecycle {
    /// String label for display in pane chrome / badges.
    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Idle => "Idle",
            Self::Done => "Done",
            Self::Stale => "Stale",
        }
    }

    /// Whether this lifecycle means the team is effectively finished.
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Done | Self::Stale)
    }
}

// ── Aggregate Team State ───────────────────────────────────────────

/// Member runtime status (derived from tasks + messages).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatus {
    /// Actively working on a task
    Active,
    /// Idle (no in_progress tasks, sent idle notification)
    Idle,
    /// Completed all tasks
    Done,
    /// Socket connection lost to member's daemon
    Disconnected,
    /// Attempting to reconnect after disconnect
    Reconnecting,
    /// Not yet connected / no data
    Unknown,
}

impl MemberStatus {
    /// Human-readable label matching the renderer's status dot color mapping.
    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Idle => "Idle",
            Self::Done => "Done",
            Self::Disconnected => "Disconnected",
            Self::Reconnecting => "Reconnecting",
            Self::Unknown => "Unknown",
        }
    }
}

/// Complete team state: config + tasks + messages + session mappings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamState {
    pub config: TeamConfig,
    pub tasks: Vec<TeamTask>,
    /// Messages per agent (agent name → messages).
    pub inboxes: HashMap<String, Vec<TeamMessage>>,
    /// Runtime status per member (derived).
    pub member_status: HashMap<String, MemberStatus>,
    /// Map of member name → daemon session name (for WebSocket connections).
    pub member_sessions: HashMap<String, String>,
    /// Current Claude Code permission mode (pushed from statusline/hooks).
    #[serde(default)]
    pub permission_mode: PermissionMode,
    /// Team-level lifecycle (derived from tasks + activity).
    #[serde(default)]
    pub lifecycle: TeamLifecycle,
    /// Unix timestamp (seconds) of last file activity (max mtime of tasks + inboxes).
    #[serde(default)]
    pub last_activity_ts: u64,
}

impl TeamState {
    /// Build team state from parsed components.
    pub fn new(
        config: TeamConfig,
        tasks: Vec<TeamTask>,
        inboxes: HashMap<String, Vec<TeamMessage>>,
    ) -> Self {
        Self::with_activity(config, tasks, inboxes, 0)
    }

    /// Build team state with a known last-activity timestamp (Unix seconds).
    pub fn with_activity(
        config: TeamConfig,
        tasks: Vec<TeamTask>,
        inboxes: HashMap<String, Vec<TeamMessage>>,
        last_activity_ts: u64,
    ) -> Self {
        let member_status = Self::derive_statuses(&config, &tasks, &inboxes);
        let lifecycle = Self::derive_lifecycle(&tasks, &member_status, last_activity_ts);
        Self {
            config,
            tasks,
            inboxes,
            member_status,
            member_sessions: HashMap::new(),
            permission_mode: PermissionMode::Default,
            lifecycle,
            last_activity_ts,
        }
    }

    /// Derive member statuses from task ownership and inbox messages.
    fn derive_statuses(
        config: &TeamConfig,
        tasks: &[TeamTask],
        inboxes: &HashMap<String, Vec<TeamMessage>>,
    ) -> HashMap<String, MemberStatus> {
        let mut statuses = HashMap::new();

        for member in &config.members {
            let name = &member.name;

            // Check if member owns any in_progress task
            let has_active = tasks.iter().any(|t| {
                t.status == TaskStatus::InProgress && t.owner.as_deref() == Some(name)
            });

            if has_active {
                statuses.insert(name.clone(), MemberStatus::Active);
                continue;
            }

            // Check if all owned tasks are completed
            let owned: Vec<_> = tasks
                .iter()
                .filter(|t| t.owner.as_deref() == Some(name))
                .collect();
            let all_done = !owned.is_empty()
                && owned.iter().all(|t| t.status == TaskStatus::Completed);

            if all_done {
                statuses.insert(name.clone(), MemberStatus::Done);
                continue;
            }

            // Check for idle notification in the lead's inbox
            if let Some(msgs) = inboxes.get("team-lead") {
                let has_idle = msgs.iter().rev().take(5).any(|m| {
                    m.from == *name && m.is_idle_notification()
                });
                if has_idle {
                    statuses.insert(name.clone(), MemberStatus::Idle);
                    continue;
                }
            }

            statuses.insert(name.clone(), MemberStatus::Unknown);
        }

        statuses
    }

    /// Derive team lifecycle from tasks, member statuses, and last activity time.
    fn derive_lifecycle(
        tasks: &[TeamTask],
        member_status: &HashMap<String, MemberStatus>,
        last_activity_ts: u64,
    ) -> TeamLifecycle {
        let non_deleted: Vec<_> = tasks
            .iter()
            .filter(|t| t.status != TaskStatus::Deleted)
            .collect();

        // If all non-deleted tasks are completed → Done
        if !non_deleted.is_empty()
            && non_deleted
                .iter()
                .all(|t| t.status == TaskStatus::Completed)
        {
            return TeamLifecycle::Done;
        }

        // Check staleness: no file activity for > 1 hour
        if last_activity_ts > 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if now.saturating_sub(last_activity_ts) > 3600 {
                return TeamLifecycle::Stale;
            }
        }

        // Any task in_progress → Active
        let has_in_progress = non_deleted
            .iter()
            .any(|t| t.status == TaskStatus::InProgress);
        if has_in_progress {
            return TeamLifecycle::Active;
        }

        // Recent activity (< 5 min) → Active
        if last_activity_ts > 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if now.saturating_sub(last_activity_ts) < 300 {
                return TeamLifecycle::Active;
            }
        }

        // Has pending tasks but all members idle → Idle
        let has_pending = non_deleted.iter().any(|t| t.status == TaskStatus::Pending);
        let all_idle_or_done = member_status
            .values()
            .all(|s| matches!(s, MemberStatus::Idle | MemberStatus::Done | MemberStatus::Unknown));
        if has_pending && all_idle_or_done {
            return TeamLifecycle::Idle;
        }

        TeamLifecycle::Active
    }

    /// Get all recent messages across all inboxes, sorted by timestamp.
    pub fn recent_messages(&self, limit: usize) -> Vec<&TeamMessage> {
        let mut all: Vec<&TeamMessage> = self
            .inboxes
            .values()
            .flat_map(|msgs| msgs.iter())
            .filter(|m| !m.is_idle_notification())
            .collect();

        // Sort by timestamp descending (newest first)
        all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        all.truncate(limit);
        all
    }

    /// Task summary: (pending, in_progress, completed).
    pub fn task_counts(&self) -> (usize, usize, usize) {
        let mut pending = 0;
        let mut in_progress = 0;
        let mut completed = 0;
        for t in &self.tasks {
            match t.status {
                TaskStatus::Pending => pending += 1,
                TaskStatus::InProgress => in_progress += 1,
                TaskStatus::Completed => completed += 1,
                TaskStatus::Deleted => {}
            }
        }
        (pending, in_progress, completed)
    }

    /// Get non-lead members (the ones that need panes).
    pub fn agent_members(&self) -> Vec<&TeamMember> {
        self.config
            .members
            .iter()
            .filter(|m| !m.is_lead())
            .collect()
    }
}

// ── Parsing Functions ──────────────────────────────────────────────

/// Parse a team config from JSON string.
pub fn parse_team_config(json: &str) -> Result<TeamConfig, serde_json::Error> {
    serde_json::from_str(json)
}

/// Parse a single task from JSON string.
pub fn parse_task(json: &str) -> Result<TeamTask, serde_json::Error> {
    serde_json::from_str(json)
}

/// Parse an inbox (array of messages) from JSON string.
pub fn parse_inbox(json: &str) -> Result<Vec<TeamMessage>, serde_json::Error> {
    serde_json::from_str(json)
}

// ── Path Helpers (for native targets) ──────────────────────────────

/// Standard teams directory: `~/.claude/teams/`
pub fn teams_dir(home: &str) -> String {
    format!("{}/.claude/teams", home)
}

/// Standard tasks directory: `~/.claude/tasks/`
pub fn tasks_dir(home: &str) -> String {
    format!("{}/.claude/tasks", home)
}

/// Config file for a team: `~/.claude/teams/{name}/config.json`
pub fn team_config_path(home: &str, team_name: &str) -> String {
    format!("{}/.claude/teams/{}/config.json", home, team_name)
}

/// Tasks directory for a team: `~/.claude/tasks/{name}/`
pub fn team_tasks_dir(home: &str, team_name: &str) -> String {
    format!("{}/.claude/tasks/{}", home, team_name)
}

/// Inboxes directory for a team: `~/.claude/teams/{name}/inboxes/`
pub fn team_inboxes_dir(home: &str, team_name: &str) -> String {
    format!("{}/.claude/teams/{}/inboxes", home, team_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_team_config() {
        let json = r#"{
            "name": "test-team",
            "description": "A test team",
            "createdAt": 1772124231866,
            "leadAgentId": "team-lead@test-team",
            "leadSessionId": "abc-123",
            "members": [
                {
                    "agentId": "team-lead@test-team",
                    "name": "team-lead",
                    "agentType": "team-lead",
                    "model": "claude-opus-4-6",
                    "joinedAt": 1772124231866,
                    "tmuxPaneId": "",
                    "cwd": "/Users/test"
                },
                {
                    "agentId": "researcher@test-team",
                    "name": "researcher",
                    "agentType": "Explore",
                    "model": "sonnet",
                    "color": "blue",
                    "joinedAt": 1772124292722,
                    "tmuxPaneId": "in-process",
                    "cwd": "/Users/test",
                    "backendType": "in-process"
                }
            ]
        }"#;

        let config = parse_team_config(json).unwrap();
        assert_eq!(config.name, "test-team");
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.members[0].name, "team-lead");
        assert!(config.members[0].is_lead());
        assert_eq!(config.members[1].name, "researcher");
        assert_eq!(config.members[1].color_rgba(), [0.4, 0.6, 1.0, 1.0]);
        assert!(!config.members[1].is_lead());
    }

    #[test]
    fn test_parse_task() {
        let json = r#"{
            "id": "1",
            "subject": "Research the codebase",
            "description": "Deep dive into the architecture",
            "activeForm": "Researching codebase",
            "owner": "researcher",
            "status": "in_progress",
            "blocks": [],
            "blockedBy": []
        }"#;

        let task = parse_task(json).unwrap();
        assert_eq!(task.id, "1");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.owner.as_deref(), Some("researcher"));
    }

    #[test]
    fn test_parse_inbox() {
        let json = r#"[
            {
                "from": "researcher",
                "text": "Found the implementation in src/main.rs",
                "summary": "Implementation found",
                "timestamp": "2026-02-26T16:46:52.240Z",
                "color": "blue",
                "read": true
            },
            {
                "from": "researcher",
                "text": "{\"type\":\"idle_notification\",\"from\":\"researcher\",\"timestamp\":\"2026-02-26T16:46:58.566Z\",\"idleReason\":\"available\"}",
                "timestamp": "2026-02-26T16:46:58.566Z",
                "color": "blue",
                "read": true
            }
        ]"#;

        let messages = parse_inbox(json).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(!messages[0].is_idle_notification());
        assert!(messages[1].is_idle_notification());
        assert_eq!(messages[0].display_text(), "Implementation found");
    }

    #[test]
    fn test_team_state_statuses() {
        let config = TeamConfig {
            name: "test".into(),
            description: String::new(),
            created_at: 0,
            lead_agent_id: "team-lead@test".into(),
            lead_session_id: String::new(),
            members: vec![
                TeamMember {
                    agent_id: "team-lead@test".into(),
                    name: "team-lead".into(),
                    agent_type: "team-lead".into(),
                    model: "opus".into(),
                    color: None,
                    prompt: None,
                    joined_at: 0,
                    tmux_pane_id: String::new(),
                    cwd: String::new(),
                    backend_type: None,
                    plan_mode_required: false,
                },
                TeamMember {
                    agent_id: "coder@test".into(),
                    name: "coder".into(),
                    agent_type: "general-purpose".into(),
                    model: "sonnet".into(),
                    color: Some("green".into()),
                    prompt: None,
                    joined_at: 0,
                    tmux_pane_id: String::new(),
                    cwd: String::new(),
                    backend_type: Some("in-process".into()),
                    plan_mode_required: false,
                },
            ],
        };

        let tasks = vec![TeamTask {
            id: "1".into(),
            subject: "Write code".into(),
            description: String::new(),
            active_form: Some("Writing code".into()),
            owner: Some("coder".into()),
            status: TaskStatus::InProgress,
            blocks: vec![],
            blocked_by: vec![],
        }];

        let state = TeamState::new(config, tasks, HashMap::new());
        assert_eq!(
            state.member_status.get("coder"),
            Some(&MemberStatus::Active)
        );
        assert_eq!(state.task_counts(), (0, 1, 0));
        assert_eq!(state.agent_members().len(), 1);
    }

    #[test]
    fn test_color_parsing() {
        let member = TeamMember {
            agent_id: String::new(),
            name: String::new(),
            agent_type: String::new(),
            model: String::new(),
            color: Some("#FF6B6B".into()),
            prompt: None,
            joined_at: 0,
            tmux_pane_id: String::new(),
            cwd: String::new(),
            backend_type: None,
            plan_mode_required: false,
        };
        let c = member.color_rgba();
        assert!((c[0] - 1.0).abs() < 0.01);
        assert!((c[1] - 0.42).abs() < 0.02);
        assert!((c[2] - 0.42).abs() < 0.02);
    }
}
