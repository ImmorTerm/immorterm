//! Shared structured terminal logging library.
//!
//! Used by both the Rust daemon (via `StructuredLogger` Rust API) and the
//! C binary (via `StructuredLogHandle` C FFI). Both produce identical output:
//!
//! - `.grid.jsonl` — grid snapshots for restoration and search
//! - `.cast` — asciicast v2 stream for replay
//! - `.ai.jsonl` — AI conversation extraction for memory integration

pub mod ai_extractor;
pub mod asciicast;
pub mod ffi;
pub mod handle;
pub mod logger;
pub mod restore;

// Re-exports for convenience
pub use ai_extractor::{AiEvent, AiExtractor, AiTool, LogEventSink};
pub use asciicast::AsciicastWriter;
pub use handle::StructuredLogHandle;
pub use logger::StructuredLogger;
