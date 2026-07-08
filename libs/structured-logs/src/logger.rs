//! Core structured logging engine.
//!
//! `StructuredLogger` manages `.grid.jsonl`, `.cast`, and `.ai.jsonl` files.
//! It accepts a `&Terminal` reference from the caller (the Rust daemon already
//! owns a Terminal instance, so we avoid double VTE parsing).
//!
//! For the C FFI path (which doesn't have a Terminal), see `handle.rs` which
//! wraps this logger with its own Terminal instance.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use immorterm_core::log::{self, PromptEvent, SnapshotTrigger};
use immorterm_core::Terminal;
use tracing::{debug, error, info, warn};

use crate::ai_extractor::{AiExtractor, AiTool, LogEventSink};
use crate::asciicast::{self, AsciicastWriter};

/// Maximum `.grid.jsonl` file size before rotation (50 MB).
const GRID_LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Interval between scrollback dumps (5 minutes).
const SCROLLBACK_DUMP_INTERVAL_SECS: u64 = 300;

/// Manages structured log files for a single terminal session.
///
/// This is the **Rust API** — it takes `&Terminal` references from the caller.
/// The daemon uses this directly. For C FFI, see `StructuredLogHandle`.
pub struct StructuredLogger {
    session_name: String,
    log_dir: PathBuf,
    grid_writer: Option<BufWriter<File>>,
    asciicast_writer: Option<AsciicastWriter>,
    grid_bytes_written: u64,
    last_sb_hash: String,
    last_sb_dump: Instant,
    terminal_changed: bool,
    in_alternate_screen: bool,
    ai_extractor: AiExtractor,
}

impl StructuredLogger {
    /// Create a new structured logger for a session.
    ///
    /// - `session_name`: Session identifier (used in filenames)
    /// - `log_dir`: Directory for log files
    /// - `cols`, `rows`: Initial terminal dimensions (for asciicast header)
    /// - `event_sink`: Optional callback for AI event notification
    pub fn new(
        session_name: &str,
        log_dir: &Path,
        cols: usize,
        rows: usize,
        event_sink: Option<Box<dyn LogEventSink>>,
    ) -> Self {
        if let Err(e) = fs::create_dir_all(log_dir) {
            error!("Failed to create log directory {:?}: {}", log_dir, e);
        }

        let grid_path = log_dir.join("grid.jsonl");
        let cast_path = log_dir.join("cast");

        let grid_writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&grid_path)
            .map(BufWriter::new)
            .map_err(|e| error!("Failed to open grid log {:?}: {}", grid_path, e))
            .ok();

        let asciicast_writer = AsciicastWriter::new(&cast_path, cols, rows)
            .map_err(|e| error!("Failed to open asciicast log {:?}: {}", cast_path, e))
            .ok();

        let ai_extractor = AiExtractor::new(session_name, log_dir, event_sink);

        info!(
            "Structured logging started: {} (grid={}, cast={})",
            session_name,
            grid_writer.is_some(),
            asciicast_writer.is_some(),
        );

        Self {
            session_name: session_name.to_string(),
            log_dir: log_dir.to_path_buf(),
            grid_writer,
            asciicast_writer,
            grid_bytes_written: 0,
            last_sb_hash: String::new(),
            last_sb_dump: Instant::now(),
            terminal_changed: false,
            in_alternate_screen: false,
            ai_extractor,
        }
    }

    /// Path to the grid log file.
    pub fn grid_log_path(&self) -> PathBuf {
        self.log_dir.join("grid.jsonl")
    }

    /// Path to the asciicast log file.
    pub fn cast_log_path(&self) -> PathBuf {
        self.log_dir.join("cast")
    }

    /// Path to the AI conversation log file.
    pub fn ai_log_path(&self) -> PathBuf {
        self.log_dir.join("ai.jsonl")
    }

    /// Called after PTY output is processed through the terminal emulator.
    ///
    /// Feeds raw bytes to the asciicast writer (filtered when in alternate screen).
    pub fn on_pty_output(&mut self, data: &[u8], terminal: &Terminal) {
        self.terminal_changed = true;

        // Track alternate screen transitions
        let new_alt = terminal.modes.alternate_screen;
        if new_alt != self.in_alternate_screen {
            self.in_alternate_screen = new_alt;
            if new_alt {
                debug!("Entered alternate screen — pausing asciicast capture");
            } else {
                debug!("Left alternate screen — resuming asciicast capture");
            }
        }

        asciicast::maybe_write_asciicast(
            &mut self.asciicast_writer,
            data,
            self.in_alternate_screen,
        );
    }

    /// Called when prompt events are detected (OSC 133).
    ///
    /// Multiple events (e.g. CommandDone + PromptStart) often arrive in the
    /// same PTY read. We deduplicate to avoid near-identical snapshots.
    pub fn on_prompt_events(&mut self, events: &[PromptEvent], terminal: &Terminal) {
        let has_snapshot_trigger = events.iter().any(|e| {
            matches!(
                e,
                PromptEvent::PromptStart | PromptEvent::CommandDone { .. }
            )
        });
        if has_snapshot_trigger {
            self.take_snapshot(terminal, SnapshotTrigger::Prompt);
        }
    }

    /// Called on periodic timer tick (every 30 seconds).
    pub fn on_periodic_tick(&mut self, terminal: &Terminal) {
        if self.terminal_changed {
            self.take_snapshot(terminal, SnapshotTrigger::Periodic);
            self.terminal_changed = false;
        }

        if self.last_sb_dump.elapsed().as_secs() >= SCROLLBACK_DUMP_INTERVAL_SECS {
            self.dump_scrollback(terminal);
        }
    }

    /// Called on session shutdown — final flush.
    pub fn on_shutdown(&mut self, terminal: &Terminal) {
        info!(
            "Structured logger shutting down for session {}",
            self.session_name
        );

        self.take_snapshot(terminal, SnapshotTrigger::Shutdown);
        self.dump_scrollback(terminal);
        self.ai_extractor.on_shutdown();

        if let Some(ref mut w) = self.grid_writer {
            let _ = w.flush();
        }
        if let Some(ref mut w) = self.asciicast_writer {
            let _ = w.flush();
        }
    }

    /// Called on terminal resize.
    pub fn on_resize(&mut self, cols: usize, rows: usize) {
        if let Some(ref mut writer) = self.asciicast_writer
            && let Err(e) = writer.write_resize(cols, rows) {
                warn!("Asciicast resize write error: {}", e);
            }
    }

    /// Take a manual snapshot (triggered via IPC).
    pub fn take_manual_snapshot(&mut self, terminal: &Terminal) {
        self.take_snapshot(terminal, SnapshotTrigger::Manual);
    }

    /// Last user prompt text (cleaned, truncated to 200 chars).
    /// Delegates to the inner `AiExtractor`.
    pub fn last_user_prompt(&self) -> Option<&str> {
        self.ai_extractor.last_user_prompt()
    }

    /// Called when AI tool state changes.
    pub fn on_ai_state_change(
        &mut self,
        tool: Option<AiTool>,
        pid: Option<u32>,
        transcript_path: Option<&str>,
        cost_usd: Option<f64>,
    ) {
        self.ai_extractor
            .on_ai_state_change(tool, pid, transcript_path, cost_usd);
    }

    // ── Internal ─────────────────────────────────────────────────────

    fn take_snapshot(&mut self, terminal: &Terminal, trigger: SnapshotTrigger) {
        // Dump scrollback before the snapshot so sb_hash is up-to-date.
        // Only dump on prompt/manual/shutdown triggers to avoid excessive I/O
        // from periodic snapshots (those still rely on the 5-minute timer).
        if !terminal.scrollback.is_empty()
            && matches!(
                trigger,
                SnapshotTrigger::Prompt | SnapshotTrigger::Manual | SnapshotTrigger::Shutdown
            )
        {
            self.dump_scrollback(terminal);
        }

        let snapshot = log::grid_to_snapshot(
            &terminal.grid,
            &terminal.scrollback,
            terminal.cursor.col,
            terminal.cursor.row,
            terminal.cols(),
            terminal.rows(),
            &terminal.cwd,
            terminal.last_exit_code,
            trigger,
            &self.last_sb_hash,
        );

        self.write_grid_entry(&snapshot);
        self.ai_extractor.on_snapshot(&snapshot);
    }

    fn write_grid_entry<T: serde::Serialize>(&mut self, entry: &T) {
        let Some(ref mut writer) = self.grid_writer else {
            return;
        };

        match serde_json::to_string(entry) {
            Ok(json) => {
                let bytes = json.len() as u64 + 1;
                if let Err(e) = writeln!(writer, "{}", json) {
                    warn!("Grid log write error: {}", e);
                    return;
                }
                if let Err(e) = writer.flush() {
                    warn!("Grid log flush error: {}", e);
                }
                self.grid_bytes_written += bytes;

                if self.grid_bytes_written > GRID_LOG_MAX_BYTES {
                    self.rotate_grid_log();
                }
            }
            Err(e) => {
                warn!("Grid snapshot serialization error: {}", e);
            }
        }
    }

    fn dump_scrollback(&mut self, terminal: &Terminal) {
        let dump = log::scrollback_to_dump(&terminal.scrollback);

        if dump.hash == self.last_sb_hash {
            debug!("Scrollback unchanged, skipping dump");
            return;
        }

        self.last_sb_hash = dump.hash.clone();
        self.last_sb_dump = Instant::now();
        self.write_grid_entry(&dump);
        info!(
            "Scrollback dumped: {} lines, hash={}",
            dump.lines.len(),
            &dump.hash[..8.min(dump.hash.len())]
        );
    }

    fn rotate_grid_log(&mut self) {
        let current = self.grid_log_path();
        let rotated = self.log_dir.join("grid.1.jsonl");

        if let Some(ref mut w) = self.grid_writer {
            let _ = w.flush();
        }
        self.grid_writer = None;

        if let Err(e) = fs::rename(&current, &rotated) {
            warn!("Grid log rotation failed: {}", e);
        } else {
            info!("Grid log rotated to {:?}", rotated);
        }

        self.grid_writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&current)
            .map(BufWriter::new)
            .map_err(|e| error!("Failed to reopen grid log: {}", e))
            .ok();
        self.grid_bytes_written = 0;
    }
}
