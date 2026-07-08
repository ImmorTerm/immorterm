//! Logical line restoration from `.grid.jsonl` files.
//!
//! Reads the last grid snapshot and scrollback dump, joins wrapped rows
//! into logical lines, and produces ANSI output for terminal restoration.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use immorterm_core::log::{GridSnapshot, ScrollbackDump};
use tracing::{debug, warn};

/// Restored session data.
///
/// Two consumption paths:
/// - Screen-auto (regular terminals): uses `ansi` — full ANSI replay including
///   scrollback. Goes through screen's PTY which has no scrollback API.
/// - AI daemon: uses `grid_ansi` for the viewport ONLY, plus `scrollback_dump`
///   to inject scrollback rows DIRECTLY into `terminal.scrollback` without
///   going through the emulator's process pipeline. This avoids the
///   compounding bug where each shelve+reattach cycle re-emulated the prior
///   scrollback as ANSI, causing it to be appended to the new emulator's
///   scrollback again — doubling on every cycle.
pub struct RestoreData {
    /// Full ANSI (scrollback + grid). For screen-auto / restore-dump CLI use.
    pub ansi: String,
    /// Grid-only ANSI (no scrollback). For AI daemon direct restore — pair
    /// with `scrollback_dump` injection.
    pub grid_ansi: String,
    /// Parsed scrollback dump for direct row-level injection by the AI daemon.
    pub scrollback_dump: Option<ScrollbackDump>,
    pub cols: usize,
    pub rows: usize,
}

/// Restore a session from its `.grid.jsonl` file.
///
/// Returns ANSI-encoded text and the original terminal dimensions.
/// Returns `None` if the grid log doesn't exist or can't be parsed.
pub fn restore_session(log_dir: &Path, session_name: &str) -> Option<RestoreData> {
    // Try new per-session directory format first (grid.jsonl), then old flat format
    let grid_path = {
        let new_path = log_dir.join("grid.jsonl");
        if new_path.exists() {
            new_path
        } else {
            let old_path = log_dir.join(format!("{}.grid.jsonl", session_name));
            if old_path.exists() {
                old_path
            } else {
                debug!("No grid log found at {:?} or {:?}", new_path, old_path);
                return None;
            }
        }
    };

    let file = File::open(&grid_path)
        .map_err(|e| warn!("Failed to open grid log {:?}: {}", grid_path, e))
        .ok()?;
    let reader = BufReader::new(file);

    let mut last_snapshot: Option<GridSnapshot> = None;
    let mut last_scrollback: Option<ScrollbackDump> = None;

    // Scan through the JSONL file — keep the last snapshot and scrollback dump
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.is_empty() {
            continue;
        }

        // Try parsing as snapshot first (has "grid" field)
        if line.contains("\"grid\"") {
            if let Ok(snap) = serde_json::from_str::<GridSnapshot>(&line) {
                last_snapshot = Some(snap);
            }
        }
        // Try parsing as scrollback dump (has "lines" and "hash" fields)
        else if line.contains("\"hash\"") && line.contains("\"lines\"")
            && let Ok(dump) = serde_json::from_str::<ScrollbackDump>(&line) {
                last_scrollback = Some(dump);
            }
    }

    let snapshot = last_snapshot?;
    let cols = snapshot.cols;
    let rows = snapshot.rows;

    // Full ANSI (with scrollback) — screen-auto path (regular terminals).
    let ansi = immorterm_core::log::snapshot_to_ansi(&snapshot, last_scrollback.as_ref());
    // Grid-only ANSI — AI daemon path. Scrollback comes via direct row
    // injection so it never re-enters the emulator's scroll pipeline.
    let grid_ansi = immorterm_core::log::snapshot_to_ansi(&snapshot, None);
    Some(RestoreData {
        ansi,
        grid_ansi,
        scrollback_dump: last_scrollback,
        cols,
        rows,
    })
}
