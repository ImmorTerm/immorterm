//! Read-only access to `~/.immorterm/digest-checkpoints.json` (schema v2).
//!
//! Per v4 §4.1 (F2 fix), the daemon reads this file at cold-start to seed
//! `last_seen_size` from the bash extractor's persisted `byte_offset`,
//! plus `file_hash` + `msg_count` for RewriteHash-lifecycle vendors.
//!
//! **Invariant: the daemon NEVER writes this file.** The bash extractor
//! (`.immorterm/hooks/immorterm-memory-digest.sh:166`) is the sole writer.
//! Daemon-side cold-start seeding from byte_offset is what closes the
//! "RAM=0 vs disk=100MB looks like 100MB burst" race that previously
//! tripped every daemon restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DigestCheckpoints {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub files: BTreeMap<String, FileCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_digest_run: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileCheckpoint {
    #[serde(default)]
    pub byte_offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_processed: Option<String>,
    #[serde(default)]
    pub memories_extracted: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_memory_id: Option<String>,
    #[serde(default)]
    pub summary_update_count: u32,
    /// Schema v2 — present for RewriteHash-lifecycle vendors (cline, gemini).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_hash: Option<String>,
    /// Schema v2 — present for RewriteHash-lifecycle vendors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg_count: Option<u32>,
}

impl DigestCheckpoints {
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".immorterm").join("digest-checkpoints.json")
    }

    /// Read with 50ms retry on parse failure (per v4 §3.6 atomic-write
    /// recovery). If the bash extractor is mid-rename, daemon retries
    /// once before falling back to default.
    pub fn load(path: &Path) -> Self {
        for _ in 0..2 {
            match std::fs::read_to_string(path) {
                Ok(s) if s.trim().is_empty() => return Self::default(),
                Ok(s) => match serde_json::from_str::<Self>(&s) {
                    Ok(cp) => return cp,
                    Err(e) => {
                        tracing::warn!(
                            "checkpoint parse failed at {} (retrying): {}",
                            path.display(),
                            e
                        );
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
                Err(e) => {
                    tracing::warn!("checkpoint read failed at {}: {}", path.display(), e);
                    return Self::default();
                }
            }
        }
        Self::default()
    }

    pub fn lookup(&self, jsonl_path: &Path) -> Option<&FileCheckpoint> {
        self.files.get(jsonl_path.to_str()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_file_returns_default() {
        let dir = tempdir().unwrap();
        let cp = DigestCheckpoints::load(&dir.path().join("nope.json"));
        assert_eq!(cp.version, 0);
        assert!(cp.files.is_empty());
    }

    #[test]
    fn parses_schema_v2_with_hash_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cp.json");
        std::fs::write(
            &path,
            r#"{
              "version": 2,
              "files": {
                "/x.jsonl": {
                  "byte_offset": 1024,
                  "memories_extracted": 5,
                  "file_hash": "deadbeef",
                  "msg_count": 42
                }
              }
            }"#,
        )
        .unwrap();
        let cp = DigestCheckpoints::load(&path);
        assert_eq!(cp.version, 2);
        let entry = cp.files.get("/x.jsonl").unwrap();
        assert_eq!(entry.byte_offset, 1024);
        assert_eq!(entry.memories_extracted, 5);
        assert_eq!(entry.file_hash.as_deref(), Some("deadbeef"));
        assert_eq!(entry.msg_count, Some(42));
    }

    #[test]
    fn parses_schema_v1_without_hash_fields() {
        // Back-compat: v1 entries have no hash/msg_count; readers tolerate.
        let dir = tempdir().unwrap();
        let path = dir.path().join("cp.json");
        std::fs::write(
            &path,
            r#"{"version":1,"files":{"/y.jsonl":{"byte_offset":99}}}"#,
        )
        .unwrap();
        let cp = DigestCheckpoints::load(&path);
        let entry = cp.files.get("/y.jsonl").unwrap();
        assert_eq!(entry.byte_offset, 99);
        assert!(entry.file_hash.is_none());
        assert!(entry.msg_count.is_none());
    }

    #[test]
    fn lookup_returns_entry_or_none() {
        let mut cp = DigestCheckpoints::default();
        cp.files.insert(
            "/a.jsonl".into(),
            FileCheckpoint { byte_offset: 17, ..Default::default() },
        );
        assert_eq!(
            cp.lookup(Path::new("/a.jsonl")).map(|e| e.byte_offset),
            Some(17)
        );
        assert!(cp.lookup(Path::new("/missing.jsonl")).is_none());
    }

    #[test]
    fn malformed_json_returns_default_after_retry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cp.json");
        std::fs::write(&path, "not json at all").unwrap();
        let cp = DigestCheckpoints::load(&path);
        // After retry exhausted, returns default rather than panicking.
        assert_eq!(cp.version, 0);
        assert!(cp.files.is_empty());
    }
}
