//! Subagent watcher — monitors `~/.claude/projects/*/subagents/` for JSONL
//! transcript files written by Claude Code Task-spawned agents.
//!
//! Each JSONL file = one subagent session. First line contains metadata
//! (agentId, slug, sessionId). Subsequent lines are user/assistant turns.
//! We detect Running vs Completed based on file modification time.

use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use immorterm_core::subagent::{SubagentEvent, SubagentInfo, SubagentStatus};

/// Threshold in seconds: if no writes for this long, agent is Completed.
const COMPLETION_THRESHOLD_SECS: u64 = 30;

/// Start the subagent watcher. Returns a broadcast sender for events.
pub fn start_subagent_watcher() -> anyhow::Result<broadcast::Sender<SubagentEvent>> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let projects_dir = PathBuf::from(format!("{}/.claude/projects", home));

    let (event_tx, _) = broadcast::channel::<SubagentEvent>(128);
    let (debounce_tx, debounce_rx) = mpsc::channel::<PathBuf>(256);

    // Start filesystem watcher thread
    let tx_clone = debounce_tx.clone();
    let projects_dir_clone = projects_dir.clone();

    std::thread::spawn(move || {
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    for path in event.paths {
                        // Only care about JSONL files in subagents/ directories
                        if path.extension().is_some_and(|e| e == "jsonl")
                            && path
                                .parent()
                                .and_then(|p| p.file_name())
                                .is_some_and(|n| n == "subagents")
                        {
                            let _ = tx_clone.try_send(path);
                        }
                    }
                }
            },
        ) {
            Ok(w) => w,
            Err(e) => {
                error!("Failed to create subagent watcher: {}", e);
                return;
            }
        };

        if projects_dir_clone.exists()
            && let Err(e) = watcher.watch(&projects_dir_clone, RecursiveMode::Recursive) {
                warn!(
                    "Cannot watch projects dir {:?}: {}",
                    projects_dir_clone, e
                );
            }

        info!(
            "Subagent watcher started: watching {:?}",
            projects_dir_clone
        );

        // Keep thread alive
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    });

    // Start the debounce + tracking task
    let event_tx_clone = event_tx.clone();
    tokio::spawn(subagent_debounce_loop(debounce_rx, event_tx_clone));

    Ok(event_tx)
}

/// Track per-agent state for detecting new lines and status changes.
struct TrackedAgent {
    info: SubagentInfo,
    /// Byte offset we've read up to in the JSONL file.
    read_offset: u64,
}

/// Debounce loop: collect file events, re-parse affected agents, broadcast changes.
async fn subagent_debounce_loop(
    mut rx: mpsc::Receiver<PathBuf>,
    event_tx: broadcast::Sender<SubagentEvent>,
) {
    let debounce_duration = Duration::from_millis(300);
    let mut tracked: HashMap<String, TrackedAgent> = HashMap::new();
    let mut pending_paths: Vec<PathBuf> = Vec::new();

    loop {
        // Wait for first event
        let path = match rx.recv().await {
            Some(p) => p,
            None => break, // Channel closed
        };
        pending_paths.push(path);

        // Drain any additional events within debounce window
        while let Ok(Some(p)) = tokio::time::timeout(debounce_duration, rx.recv()).await {
            pending_paths.push(p);
        }

        // Deduplicate paths
        pending_paths.sort();
        pending_paths.dedup();

        // Process each changed JSONL file
        for path in pending_paths.drain(..) {
            if let Some(agent_id) = extract_agent_id(&path) {
                process_jsonl_file(&path, &agent_id, &mut tracked, &event_tx);
            }
        }

        // Periodic completion check: mark agents as Completed if no recent writes
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        for agent in tracked.values_mut() {
            if agent.info.status == SubagentStatus::Running
                && now_secs.saturating_sub(agent.info.last_activity) > COMPLETION_THRESHOLD_SECS
            {
                agent.info.status = SubagentStatus::Completed;
                let _ = event_tx.send(SubagentEvent::Completed(agent.info.agent_id.clone()));
            }
        }
    }
}

/// Extract agent_id from filename like "agent-a033dc8.jsonl" → "a033dc8".
fn extract_agent_id(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("agent-"))
        .map(|s| s.to_string())
}

/// Process a JSONL transcript file: parse metadata, detect new lines, emit events.
fn process_jsonl_file(
    path: &Path,
    agent_id: &str,
    tracked: &mut HashMap<String, TrackedAgent>,
    event_tx: &broadcast::Sender<SubagentEvent>,
) {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            debug!("Cannot open subagent JSONL {:?}: {}", path, e);
            return;
        }
    };

    let file_meta = match file.metadata() {
        Ok(m) => m,
        Err(_) => return,
    };

    let file_mtime = file_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let status = if now_secs.saturating_sub(file_mtime) < COMPLETION_THRESHOLD_SECS {
        SubagentStatus::Running
    } else {
        SubagentStatus::Completed
    };

    if let Some(existing) = tracked.get_mut(agent_id) {
        // Known agent — read new lines from last offset
        let mut reader = std::io::BufReader::new(&file);
        if reader.seek(SeekFrom::Start(existing.read_offset)).is_ok() {
            let mut new_lines = 0usize;
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() {
                            new_lines += 1;
                            let _ = event_tx.send(SubagentEvent::NewTranscriptLine {
                                agent_id: agent_id.to_string(),
                                line: trimmed.to_string(),
                            });
                        }
                    }
                    Err(_) => break,
                }
            }

            existing.read_offset = reader.stream_position().unwrap_or(existing.read_offset);
            existing.info.message_count += new_lines;
            existing.info.last_activity = file_mtime;

            let old_status = existing.info.status;
            existing.info.status = status;

            if new_lines > 0 || old_status != status {
                let _ = event_tx.send(SubagentEvent::Updated(existing.info.clone()));
            }
        }
    } else {
        // New agent — parse first line for metadata
        let mut reader = std::io::BufReader::new(&file);
        let mut first_line = String::new();
        if reader.read_line(&mut first_line).is_err() || first_line.trim().is_empty() {
            return;
        }

        let parsed: serde_json::Value = match serde_json::from_str(first_line.trim()) {
            Ok(v) => v,
            Err(e) => {
                debug!("Cannot parse first line of {:?}: {}", path, e);
                return;
            }
        };

        let slug = parsed
            .get("slug")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let parent_session_id = parsed
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let started_at = parsed
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(parse_iso_timestamp)
            .unwrap_or(file_mtime);

        // Count total lines
        reader.seek(SeekFrom::Start(0)).ok();
        let mut line_count = 0usize;
        let mut buf = String::new();
        while reader.read_line(&mut buf).unwrap_or(0) > 0 {
            line_count += 1;
            buf.clear();
        }
        let read_offset = reader.stream_position().unwrap_or(0);

        let info = SubagentInfo {
            agent_id: agent_id.to_string(),
            slug,
            parent_session_id,
            transcript_path: path.to_string_lossy().to_string(),
            started_at,
            last_activity: file_mtime,
            message_count: line_count,
            status,
        };

        info!(
            "Detected subagent: {} ({}) — {} lines, {:?}",
            info.agent_id, info.slug, info.message_count, info.status
        );

        let _ = event_tx.send(SubagentEvent::Detected(info.clone()));

        tracked.insert(
            agent_id.to_string(),
            TrackedAgent { info, read_offset },
        );
    }
}

/// Parse ISO-8601 timestamp to Unix seconds (best-effort).
fn parse_iso_timestamp(s: &str) -> Option<u64> {
    // Format: "2026-01-29T10:30:38.894Z"
    // We do a simple parse — chrono is overkill for this.
    let s = s.trim().trim_end_matches('Z');
    let parts: Vec<&str> = s.split('T').collect();
    if parts.len() != 2 {
        return None;
    }

    let date_parts: Vec<u64> = parts[0].split('-').filter_map(|p| p.parse().ok()).collect();
    let time_parts: Vec<&str> = parts[1].split(':').collect();

    if date_parts.len() != 3 || time_parts.len() < 2 {
        return None;
    }

    // Rough calculation (not accounting for leap years etc.)
    let year = date_parts[0];
    let month = date_parts[1];
    let day = date_parts[2];

    let hour: u64 = time_parts[0].parse().ok()?;
    let min: u64 = time_parts[1].parse().ok()?;
    let sec: u64 = time_parts
        .get(2)
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Days from epoch to year start (approximate)
    let days_to_year = (year - 1970) * 365 + (year - 1969) / 4;
    let month_days: [u64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let days_to_month = month_days.get((month - 1) as usize).copied().unwrap_or(0);
    let total_days = days_to_year + days_to_month + day - 1;

    Some(total_days * 86400 + hour * 3600 + min * 60 + sec)
}

/// Discover all subagent JSONL files for a given parent session ID.
pub fn discover_subagents(parent_session_id: &str) -> Vec<SubagentInfo> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let projects_dir = PathBuf::from(format!("{}/.claude/projects", home));

    let mut agents = Vec::new();

    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return agents,
    };

    for project_entry in entries.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }

        // Look in {project}/{session_id}/subagents/
        let subagents_dir = project_path.join(parent_session_id).join("subagents");
        if !subagents_dir.is_dir() {
            continue;
        }

        let sub_entries = match std::fs::read_dir(&subagents_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in sub_entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }

            if let Some(agent_id) = extract_agent_id(&path)
                && let Some(info) = parse_subagent_file(&path, &agent_id, parent_session_id) {
                    agents.push(info);
                }
        }
    }

    agents
}

/// Parse a single JSONL file into SubagentInfo.
fn parse_subagent_file(
    path: &Path,
    agent_id: &str,
    parent_session_id: &str,
) -> Option<SubagentInfo> {
    let file = std::fs::File::open(path).ok()?;
    let file_meta = file.metadata().ok()?;
    let file_mtime = file_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let status = if now_secs.saturating_sub(file_mtime) < COMPLETION_THRESHOLD_SECS {
        SubagentStatus::Running
    } else {
        SubagentStatus::Completed
    };

    let mut reader = std::io::BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).ok()?;

    let parsed: serde_json::Value = serde_json::from_str(first_line.trim()).ok()?;

    let slug = parsed
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let started_at = parsed
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(parse_iso_timestamp)
        .unwrap_or(file_mtime);

    let line_count = reader.lines().count() + 1; // +1 for first line already read

    Some(SubagentInfo {
        agent_id: agent_id.to_string(),
        slug,
        parent_session_id: parent_session_id.to_string(),
        transcript_path: path.to_string_lossy().to_string(),
        started_at,
        last_activity: file_mtime,
        message_count: line_count,
        status,
    })
}
