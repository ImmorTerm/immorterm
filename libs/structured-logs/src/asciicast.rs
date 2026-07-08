//! Asciicast v2 writer — standard terminal replay format.
//!
//! Produces `.cast` files compatible with asciinema, svg-term, and other tools.
//! Each file starts with a JSON header followed by newline-delimited event records.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use tracing::warn;

/// Asciicast v2 format writer.
pub struct AsciicastWriter {
    writer: BufWriter<File>,
    start_time: Instant,
}

impl AsciicastWriter {
    /// Create a new asciicast writer at the given path.
    ///
    /// Writes the v2 header immediately.
    pub fn new(path: &Path, cols: usize, rows: usize) -> std::io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let header = serde_json::json!({
            "version": 2,
            "width": cols,
            "height": rows,
            "timestamp": ts,
            "env": {
                "TERM": "xterm-256color",
                "SHELL": std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
            }
        });
        writeln!(writer, "{}", header)?;
        writer.flush()?;

        Ok(Self {
            writer,
            start_time: Instant::now(),
        })
    }

    /// Write an output event (what the terminal displayed).
    pub fn write_output(&mut self, data: &[u8]) -> std::io::Result<()> {
        let offset = self.start_time.elapsed().as_secs_f64();
        let escaped = String::from_utf8_lossy(data);
        let event = serde_json::json!([offset, "o", escaped]);
        writeln!(self.writer, "{}", event)?;
        Ok(())
    }

    /// Write a resize event.
    pub fn write_resize(&mut self, cols: usize, rows: usize) -> std::io::Result<()> {
        let offset = self.start_time.elapsed().as_secs_f64();
        let event = serde_json::json!([offset, "r", format!("{}x{}", cols, rows)]);
        writeln!(self.writer, "{}", event)?;
        Ok(())
    }

    /// Flush buffered writes to disk.
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

/// Write raw bytes to the asciicast writer, respecting alternate screen state.
///
/// Returns quietly if the writer is `None` or we're in alternate screen mode.
pub fn maybe_write_asciicast(
    writer: &mut Option<AsciicastWriter>,
    data: &[u8],
    in_alternate_screen: bool,
) {
    if in_alternate_screen {
        return;
    }
    if let Some(w) = writer
        && let Err(e) = w.write_output(data) {
            warn!("Asciicast write error: {}", e);
        }
}
