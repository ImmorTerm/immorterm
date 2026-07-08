//! Team file watcher — monitors `~/.claude/teams/` and `~/.claude/tasks/`
//! for changes, re-parses team state, and broadcasts updates.
//!
//! Uses the `notify` crate for filesystem events with debouncing to coalesce
//! rapid writes (Claude Code writes many small files during team operations).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, error, info, warn};

use immorterm_core::team::{
    self, parse_inbox, parse_task, parse_team_config, TeamMessage, TeamState, TeamTask,
};

use crate::websocket::TeamEvent;

/// Shared team state, protected by RwLock for concurrent reads.
pub type SharedTeamState = Arc<RwLock<HashMap<String, TeamState>>>;

/// Team state change notification.
#[derive(Debug, Clone)]
pub struct TeamStateChange {
    pub team_name: String,
    pub state: TeamState,
    pub events: Vec<TeamEvent>,
}

/// Start watching `~/.claude/teams/` and `~/.claude/tasks/` for changes.
///
/// Returns:
/// - `SharedTeamState`: read-only view of all team states
/// - `broadcast::Receiver<TeamStateChange>`: subscribe for change notifications
pub async fn start_team_watcher() -> anyhow::Result<(
    SharedTeamState,
    broadcast::Sender<TeamStateChange>,
)> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let teams_dir = PathBuf::from(team::teams_dir(&home));
    let tasks_dir = PathBuf::from(team::tasks_dir(&home));

    // Ensure directories exist
    std::fs::create_dir_all(&teams_dir).ok();
    std::fs::create_dir_all(&tasks_dir).ok();

    // Initial scan
    let initial_states = scan_all_teams(&home).await;
    let shared_state: SharedTeamState = Arc::new(RwLock::new(initial_states));

    // Broadcast channel for change notifications (64 buffer)
    let (change_tx, _) = broadcast::channel::<TeamStateChange>(64);

    // Debounce channel: filesystem events → coalesced re-parse
    let (debounce_tx, debounce_rx) = mpsc::channel::<PathBuf>(256);

    // Start the notify watcher (sync → sends to debounce channel)
    let tx_clone = debounce_tx.clone();
    let teams_dir_clone = teams_dir.clone();
    let tasks_dir_clone = tasks_dir.clone();

    std::thread::spawn(move || {
        let _rt = tokio::runtime::Handle::try_current();
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    for path in event.paths {
                        // Only care about JSON files
                        if path.extension().is_some_and(|e| e == "json") {
                            let _ = tx_clone.try_send(path);
                        }
                    }
                }
            },
        ) {
            Ok(w) => w,
            Err(e) => {
                error!("Failed to create file watcher: {}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(&teams_dir_clone, RecursiveMode::Recursive) {
            warn!("Cannot watch teams dir {:?}: {}", teams_dir_clone, e);
        }
        if let Err(e) = watcher.watch(&tasks_dir_clone, RecursiveMode::Recursive) {
            warn!("Cannot watch tasks dir {:?}: {}", tasks_dir_clone, e);
        }

        info!(
            "Team watcher started: watching {:?} and {:?}",
            teams_dir_clone, tasks_dir_clone
        );

        // Keep the thread alive (watcher is dropped when thread ends)
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    });

    // Start the debounce processor (async task)
    let state_clone = shared_state.clone();
    let change_tx_clone = change_tx.clone();
    tokio::spawn(debounce_loop(debounce_rx, state_clone, change_tx_clone, home));

    Ok((shared_state, change_tx))
}

/// Debounce loop: collects filesystem events, waits for quiet period, then re-parses.
async fn debounce_loop(
    mut rx: mpsc::Receiver<PathBuf>,
    shared_state: SharedTeamState,
    change_tx: broadcast::Sender<TeamStateChange>,
    home: String,
) {
    let debounce_duration = Duration::from_millis(200);

    loop {
        // Wait for first event
        let first = match rx.recv().await {
            Some(p) => p,
            None => break, // Channel closed
        };

        // Collect team names that changed
        let mut changed_teams = std::collections::HashSet::new();
        if let Some(name) = extract_team_name(&first, &home) {
            changed_teams.insert(name);
        }

        // Drain additional events within the debounce window
        while let Ok(Some(path)) = tokio::time::timeout(debounce_duration, rx.recv()).await {
            if let Some(name) = extract_team_name(&path, &home) {
                changed_teams.insert(name);
            }
        }

        // Re-parse each changed team
        for team_name in changed_teams {
            debug!("Re-parsing team state: {}", team_name);
            let new_state = load_team_state(&home, &team_name).await;

            if let Some(new_state) = new_state {
                // Diff against old state to generate events
                let events = {
                    let old_states = shared_state.read().await;
                    if let Some(old) = old_states.get(&team_name) {
                        diff_team_state(old, &new_state)
                    } else {
                        vec![TeamEvent::ConfigChanged]
                    }
                };

                // Update shared state
                {
                    let mut states = shared_state.write().await;
                    states.insert(team_name.clone(), new_state.clone());
                }

                // Broadcast change
                let change = TeamStateChange {
                    team_name: team_name.clone(),
                    state: new_state,
                    events,
                };
                let _ = change_tx.send(change);
            }
        }
    }
}

/// Extract the team name from a changed file path.
///
/// Paths look like:
/// - `~/.claude/teams/{name}/config.json`
/// - `~/.claude/teams/{name}/inboxes/{agent}.json`
/// - `~/.claude/tasks/{name}/{id}.json`
fn extract_team_name(path: &Path, home: &str) -> Option<String> {
    let path_str = path.to_string_lossy();

    // Check if it's under teams dir
    let teams_prefix = format!("{}/.claude/teams/", home);
    if let Some(rest) = path_str.strip_prefix(&teams_prefix) {
        // rest = "{team_name}/config.json" or "{team_name}/inboxes/foo.json"
        return rest.split('/').next().map(|s| s.to_string());
    }

    // Check if it's under tasks dir
    let tasks_prefix = format!("{}/.claude/tasks/", home);
    if let Some(rest) = path_str.strip_prefix(&tasks_prefix) {
        // rest = "{team_name}/{id}.json"
        return rest.split('/').next().map(|s| s.to_string());
    }

    None
}

/// Scan all teams from disk.
async fn scan_all_teams(home: &str) -> HashMap<String, TeamState> {
    let mut states = HashMap::new();
    let teams_path = PathBuf::from(team::teams_dir(home));

    let entries = match std::fs::read_dir(&teams_path) {
        Ok(e) => e,
        Err(_) => return states,
    };

    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(state) = load_team_state(home, &name).await {
                info!("Loaded team: {} ({} members, {} tasks)",
                    name, state.config.members.len(), state.tasks.len());
                states.insert(name, state);
            }
        }
    }

    states
}

/// Load full team state from disk.
async fn load_team_state(home: &str, team_name: &str) -> Option<TeamState> {
    // Parse config
    let config_path = team::team_config_path(home, team_name);
    let config_json = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) => {
            debug!("Cannot read team config {}: {}", config_path, e);
            return None;
        }
    };
    let config = match parse_team_config(&config_json) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to parse team config {}: {}", config_path, e);
            return None;
        }
    };

    // Parse tasks
    let tasks = load_tasks(home, team_name);

    // Parse inboxes
    let inboxes = load_inboxes(home, team_name);

    // Derive last_activity_ts from file mtimes
    let last_activity_ts = get_last_activity_ts(home, team_name);

    Some(TeamState::with_activity(config, tasks, inboxes, last_activity_ts))
}

/// Load all tasks for a team from disk (public for IPC handler).
pub fn load_tasks_pub(home: &str, team_name: &str) -> Vec<TeamTask> {
    load_tasks(home, team_name)
}

/// Load all tasks for a team from disk.
fn load_tasks(home: &str, team_name: &str) -> Vec<TeamTask> {
    let dir = team::team_tasks_dir(home, team_name);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json")
            && let Ok(json) = std::fs::read_to_string(&path) {
                match parse_task(&json) {
                    Ok(task) => tasks.push(task),
                    Err(e) => debug!("Failed to parse task {:?}: {}", path, e),
                }
            }
    }

    // Sort by ID (numeric if possible)
    tasks.sort_by(|a, b| {
        let a_num: Result<u64, _> = a.id.parse();
        let b_num: Result<u64, _> = b.id.parse();
        match (a_num, b_num) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            _ => a.id.cmp(&b.id),
        }
    });

    tasks
}

/// Load all inboxes for a team from disk (public for IPC handler).
pub fn load_inboxes_pub(home: &str, team_name: &str) -> HashMap<String, Vec<TeamMessage>> {
    load_inboxes(home, team_name)
}

/// Load all inboxes for a team from disk.
fn load_inboxes(home: &str, team_name: &str) -> HashMap<String, Vec<TeamMessage>> {
    let dir = team::team_inboxes_dir(home, team_name);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return HashMap::new(),
    };

    let mut inboxes = HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            let agent_name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if let Ok(json) = std::fs::read_to_string(&path) {
                match parse_inbox(&json) {
                    Ok(messages) => {
                        inboxes.insert(agent_name, messages);
                    }
                    Err(e) => debug!("Failed to parse inbox {:?}: {}", path, e),
                }
            }
        }
    }

    inboxes
}

/// Get the most recent file modification time across tasks and inboxes (Unix seconds).
fn get_last_activity_ts(home: &str, team_name: &str) -> u64 {
    let mut max_mtime: u64 = 0;

    // Check task files
    let tasks_dir = team::team_tasks_dir(home, team_name);
    if let Ok(entries) = std::fs::read_dir(&tasks_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified() {
                    let ts = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    max_mtime = max_mtime.max(ts);
                }
        }
    }

    // Check inbox files
    let inboxes_dir = team::team_inboxes_dir(home, team_name);
    if let Ok(entries) = std::fs::read_dir(&inboxes_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified() {
                    let ts = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    max_mtime = max_mtime.max(ts);
                }
        }
    }

    // Also check config.json mtime
    let config_path = team::team_config_path(home, team_name);
    if let Ok(meta) = std::fs::metadata(&config_path)
        && let Ok(modified) = meta.modified() {
            let ts = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            max_mtime = max_mtime.max(ts);
        }

    max_mtime
}

/// Generate diff events between old and new team states.
fn diff_team_state(old: &TeamState, new: &TeamState) -> Vec<TeamEvent> {
    let mut events = Vec::new();

    // Check for config changes (member count)
    if old.config.members.len() != new.config.members.len() {
        // Find new members
        let old_names: std::collections::HashSet<&str> = old
            .config
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        let new_names: std::collections::HashSet<&str> = new
            .config
            .members
            .iter()
            .map(|m| m.name.as_str())
            .collect();

        for name in new_names.difference(&old_names) {
            events.push(TeamEvent::MemberJoined {
                name: name.to_string(),
            });
        }
        for name in old_names.difference(&new_names) {
            events.push(TeamEvent::MemberLeft {
                name: name.to_string(),
            });
        }
    }

    // Check for task changes
    let old_tasks: HashMap<&str, &TeamTask> = old.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    for new_task in &new.tasks {
        if let Some(old_task) = old_tasks.get(new_task.id.as_str()) {
            if old_task.status != new_task.status || old_task.owner != new_task.owner {
                events.push(TeamEvent::TaskChanged {
                    task_id: new_task.id.clone(),
                    status: format!("{:?}", new_task.status).to_lowercase(),
                    owner: new_task.owner.clone(),
                });
            }
        } else {
            // New task
            events.push(TeamEvent::TaskChanged {
                task_id: new_task.id.clone(),
                status: format!("{:?}", new_task.status).to_lowercase(),
                owner: new_task.owner.clone(),
            });
        }
    }

    // Check for new messages (compare inbox lengths)
    for (agent, new_msgs) in &new.inboxes {
        let old_len = old.inboxes.get(agent).map_or(0, |m| m.len());
        if new_msgs.len() > old_len {
            // New messages arrived
            for msg in &new_msgs[old_len..] {
                if !msg.is_idle_notification() {
                    events.push(TeamEvent::MessageReceived {
                        from: msg.from.clone(),
                        to: agent.clone(),
                        summary: msg.display_text().to_string(),
                    });
                }
            }
        }
    }

    // Check for lifecycle changes
    if old.lifecycle != new.lifecycle {
        events.push(TeamEvent::LifecycleChanged {
            old: old.lifecycle,
            new: new.lifecycle,
        });
    }

    if events.is_empty() && old.config.name != new.config.name {
        events.push(TeamEvent::ConfigChanged);
    }

    events
}

/// Write a message to a teammate's inbox file.
pub fn send_team_message(
    home: &str,
    team_name: &str,
    recipient: &str,
    content: &str,
) -> anyhow::Result<()> {
    let inbox_path = format!(
        "{}/{}.json",
        team::team_inboxes_dir(home, team_name),
        recipient
    );

    // Read existing inbox
    let mut messages: Vec<serde_json::Value> = if let Ok(json) = std::fs::read_to_string(&inbox_path) {
        serde_json::from_str(&json).unwrap_or_default()
    } else {
        Vec::new()
    };

    // Append new message
    let now = chrono_lite_timestamp();
    let msg = serde_json::json!({
        "from": "immorterm",
        "text": content,
        "summary": if content.len() > 60 { &content[..60] } else { content },
        "timestamp": now,
        "color": "purple",
        "read": false
    });
    messages.push(msg);

    // Ensure inbox directory exists
    let inbox_dir = team::team_inboxes_dir(home, team_name);
    std::fs::create_dir_all(&inbox_dir)?;

    // Write back
    let json = serde_json::to_string_pretty(&messages)?;
    std::fs::write(&inbox_path, json)?;

    info!("Sent message to {} in team {}", recipient, team_name);
    Ok(())
}

/// Simple timestamp without chrono dependency.
fn chrono_lite_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{}", secs)
}

/// Discover all team names from `~/.claude/teams/`.
pub fn discover_teams(home: &str) -> Vec<String> {
    let teams_path = PathBuf::from(team::teams_dir(home));
    let entries = match std::fs::read_dir(&teams_path) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            // Verify config.json exists
            let config = teams_path.join(&name).join("config.json");
            if config.exists() {
                Some(name)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_team_name() {
        let home = "/Users/test";

        assert_eq!(
            extract_team_name(
                Path::new("/Users/test/.claude/teams/my-team/config.json"),
                home
            ),
            Some("my-team".to_string())
        );

        assert_eq!(
            extract_team_name(
                Path::new("/Users/test/.claude/teams/my-team/inboxes/leader.json"),
                home
            ),
            Some("my-team".to_string())
        );

        assert_eq!(
            extract_team_name(
                Path::new("/Users/test/.claude/tasks/my-team/1.json"),
                home
            ),
            Some("my-team".to_string())
        );

        assert_eq!(
            extract_team_name(Path::new("/other/path/file.json"), home),
            None
        );
    }

    #[test]
    fn test_diff_detects_task_changes() {
        use immorterm_core::team::*;

        let config = TeamConfig {
            name: "test".into(),
            description: String::new(),
            created_at: 0,
            lead_agent_id: String::new(),
            lead_session_id: String::new(),
            members: vec![],
        };

        let old_tasks = vec![TeamTask {
            id: "1".into(),
            subject: "Task 1".into(),
            description: String::new(),
            active_form: None,
            owner: Some("coder".into()),
            status: TaskStatus::Pending,
            blocks: vec![],
            blocked_by: vec![],
        }];

        let new_tasks = vec![TeamTask {
            id: "1".into(),
            subject: "Task 1".into(),
            description: String::new(),
            active_form: Some("Working on it".into()),
            owner: Some("coder".into()),
            status: TaskStatus::InProgress,
            blocks: vec![],
            blocked_by: vec![],
        }];

        let old = TeamState::new(config.clone(), old_tasks, HashMap::new());
        let new = TeamState::new(config, new_tasks, HashMap::new());

        let events = diff_team_state(&old, &new);
        // Expect 2 events: TaskChanged (Pending→InProgress) + LifecycleChanged (Idle→Active)
        assert_eq!(events.len(), 2);
        match &events[0] {
            TeamEvent::TaskChanged { task_id, status, .. } => {
                assert_eq!(task_id, "1");
                assert_eq!(status, "inprogress");
            }
            _ => panic!("Expected TaskChanged event, got {:?}", events[0]),
        }
        match &events[1] {
            TeamEvent::LifecycleChanged { .. } => {}
            _ => panic!("Expected LifecycleChanged event, got {:?}", events[1]),
        }
    }
}
