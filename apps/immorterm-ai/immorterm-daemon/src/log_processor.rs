//! C binary sidecar — tails a raw `.log` file and produces structured output.
//!
//! The C terminal binary (screen fork) writes raw PTY bytes to `.log` files.
//! This module tails those files, feeds the bytes through the VTE parser
//! (via `Terminal::process()`), and uses `StructuredLogger` to produce
//! `.grid.jsonl` and `.cast` files — the same structured output that the
//! Rust daemon produces natively.
//!
//! ## Usage
//!
//! ```text
//! immorterm-ai log-process /path/to/session.log
//! ```
//!
//! The sidecar runs until SIGTERM, at which point it takes a final snapshot
//! and exits cleanly.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use immorterm_core::Terminal;
use tracing::{debug, info};

use structured_logs::StructuredLogger;

/// Poll interval for checking new data in the log file.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Interval for periodic structured log snapshots.
const PERIODIC_TICK_INTERVAL: Duration = Duration::from_secs(30);

/// Default terminal dimensions (resize events in the raw log will adjust).
const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;

/// Run the log processor sidecar.
///
/// Tails `log_path`, feeds bytes through a VTE terminal emulator, and writes
/// structured log files (`.grid.jsonl` and `.cast`) to the same directory.
///
/// Runs until SIGTERM is received, then performs a clean shutdown with a
/// final snapshot.
pub fn run_log_processor(log_path: &Path) -> Result<()> {
    // Derive output directory and session name from the log path
    let log_dir = log_path
        .parent()
        .context("Log file has no parent directory")?;
    let session_name = log_path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("Cannot derive session name from log filename")?;

    info!(
        "Log processor starting: session={}, log={:?}, output_dir={:?}",
        session_name, log_path, log_dir
    );

    // Set up SIGTERM + SIGINT handlers for clean shutdown
    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&shutdown))
        .context("Failed to register SIGTERM handler")?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&shutdown))
        .context("Failed to register SIGINT handler")?;

    // Create terminal emulator and structured logger
    let mut terminal = Terminal::new(DEFAULT_COLS, DEFAULT_ROWS);
    let mut logger = StructuredLogger::new(session_name, log_dir, DEFAULT_COLS, DEFAULT_ROWS, None);

    // Open the log file
    let mut file = File::open(log_path)
        .with_context(|| format!("Cannot open log file: {:?}", log_path))?;

    // Process existing content first
    let file_size = file
        .metadata()
        .map(|m| m.len())
        .unwrap_or(0);

    if file_size > 0 {
        info!("Processing {} bytes of existing log data", file_size);
        process_file_content(&mut file, &mut terminal, &mut logger, file_size as usize)?;
    }

    // Enter tail loop: poll for new data
    let mut last_periodic = Instant::now();
    let mut read_buf = vec![0u8; 64 * 1024]; // 64KB read buffer

    info!("Entering tail loop (poll interval: {:?})", POLL_INTERVAL);

    while !shutdown.load(Ordering::Relaxed) {
        // Check for new data
        let bytes_read = file.read(&mut read_buf).unwrap_or(0);

        if bytes_read > 0 {
            let data = &read_buf[..bytes_read];

            // Feed through terminal emulator
            terminal.process(data);

            // Notify logger of PTY output
            logger.on_pty_output(data, &terminal);

            // Drain and process prompt events
            let events = terminal.drain_prompt_events();
            if !events.is_empty() {
                logger.on_prompt_events(&events, &terminal);
            }

            debug!("Processed {} bytes, {} prompt events", bytes_read, events.len());
        }

        // Periodic tick for snapshots
        if last_periodic.elapsed() >= PERIODIC_TICK_INTERVAL {
            logger.on_periodic_tick(&terminal);
            last_periodic = Instant::now();
        }

        // Sleep if no data was available
        if bytes_read == 0 {
            thread::sleep(POLL_INTERVAL);
        }
    }

    // Clean shutdown
    info!("SIGTERM received — performing clean shutdown");
    logger.on_shutdown(&terminal);
    info!("Log processor exiting cleanly");

    Ok(())
}

/// Process a chunk of file content through the terminal and logger.
fn process_file_content(
    file: &mut File,
    terminal: &mut Terminal,
    logger: &mut StructuredLogger,
    total_bytes: usize,
) -> Result<()> {
    let mut buf = vec![0u8; 128 * 1024]; // 128KB chunks
    let mut processed = 0usize;

    while processed < total_bytes {
        let to_read = buf.len().min(total_bytes - processed);
        let n = file.read(&mut buf[..to_read])?;
        if n == 0 {
            break;
        }

        let data = &buf[..n];
        terminal.process(data);
        logger.on_pty_output(data, terminal);

        let events = terminal.drain_prompt_events();
        if !events.is_empty() {
            logger.on_prompt_events(&events, terminal);
        }

        processed += n;
    }

    debug!("Processed {} bytes of existing content", processed);

    // Take an initial snapshot after processing existing content
    logger.on_periodic_tick(terminal);

    Ok(())
}
