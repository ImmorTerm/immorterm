//! Daemon adapter for the shared structured logging library.
//!
//! Re-exports `structured_logs::StructuredLogger` and provides the
//! `OpenMemoryEventSink` adapter that bridges the shared library's
//! `LogEventSink` trait to the daemon's tokio mpsc push channel.

use structured_logs::ai_extractor::{format_iso_ts, AiEvent, LogEventSink};
use tokio::sync::mpsc;

use crate::openmemory_push::TerminalLogEvent;

// Re-export the shared library's StructuredLogger as the daemon's type.
pub use structured_logs::StructuredLogger;

/// Adapter that implements `LogEventSink` and forwards AI events
/// to the daemon's OpenMemory push channel.
pub struct OpenMemoryEventSink {
    session_name: String,
    tx: mpsc::Sender<TerminalLogEvent>,
}

impl OpenMemoryEventSink {
    pub fn new(session_name: &str, tx: mpsc::Sender<TerminalLogEvent>) -> Self {
        Self {
            session_name: session_name.to_string(),
            tx,
        }
    }
}

impl LogEventSink for OpenMemoryEventSink {
    fn on_event(&mut self, event: &AiEvent) {
        let log_event = TerminalLogEvent {
            session_name: self.session_name.clone(),
            event_type: event.event.clone(),
            ai_tool: event.tool.clone(),
            role: event.role.clone(),
            content: event.content.clone(),
            tools_visible: event.tools_visible.clone(),
            transcript_path: event.transcript_path.clone(),
            duration_s: event.duration_s,
            cost_usd: event.cost_usd,
            cwd: None,
            exit_code: None,
            timestamp: format_iso_ts(event.ts),
        };
        // try_send: never block the PTY path — drop events if channel is full
        let _ = self.tx.try_send(log_event);
    }
}
