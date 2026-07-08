//! `StructuredLogHandle` — C FFI entry point.
//!
//! Wraps `StructuredLogger` + its own `Terminal` instance for use by the
//! C binary. The C binary feeds raw PTY bytes → this handle parses them
//! through the Rust VTE parser → produces identical structured output to
//! the Rust daemon.

use std::path::Path;

use immorterm_core::Terminal;

use crate::ai_extractor::LogEventSink;
use crate::logger::StructuredLogger;

/// Opaque handle for C FFI.
///
/// Owns a `Terminal` (VTE parser) so the C binary doesn't need to share its
/// own parser state. The C binary's `WriteString` still runs for display;
/// this handle drives a parallel parse purely for structured logging.
pub struct StructuredLogHandle {
    terminal: Terminal,
    logger: StructuredLogger,
    /// Counter for simulating periodic ticks from the C side.
    periodic_counter: u32,
}

/// How many `process()` calls between periodic tick checks.
/// At ~60 reads/sec from PTY, 1800 calls ≈ 30 seconds.
const PERIODIC_TICK_INTERVAL: u32 = 1800;

impl StructuredLogHandle {
    /// Create a new handle for a session.
    pub fn new(
        session_name: &str,
        log_dir: &Path,
        cols: usize,
        rows: usize,
        event_sink: Option<Box<dyn LogEventSink>>,
    ) -> Self {
        let terminal = Terminal::new(cols, rows);
        let logger = StructuredLogger::new(session_name, log_dir, cols, rows, event_sink);

        Self {
            terminal,
            logger,
            periodic_counter: 0,
        }
    }

    /// Feed raw PTY output bytes.
    ///
    /// Parses through the internal VTE terminal and feeds the result
    /// to the structured logger (asciicast + grid change tracking).
    pub fn process(&mut self, data: &[u8]) {
        // Parse through our own terminal
        self.terminal.process(data);

        // Feed to logger (asciicast capture, change tracking)
        self.logger.on_pty_output(data, &self.terminal);

        // Check for prompt events
        let events = self.terminal.drain_prompt_events();
        if !events.is_empty() {
            self.logger.on_prompt_events(&events, &self.terminal);
        }

        // Simulate periodic tick (C binary doesn't have a tokio timer)
        self.periodic_counter += 1;
        if self.periodic_counter >= PERIODIC_TICK_INTERVAL {
            self.periodic_counter = 0;
            self.logger.on_periodic_tick(&self.terminal);
        }
    }

    /// Notify of terminal resize.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.terminal.resize(cols, rows);
        self.logger.on_resize(cols, rows);
    }

    /// Flush all writers and shut down.
    pub fn shutdown(&mut self) {
        self.logger.on_shutdown(&self.terminal);
    }
}
