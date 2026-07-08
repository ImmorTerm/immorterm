//! In-memory `SessionRegistry` — primary state for the daemon.
//!
//! Per v4 §4: keyed by `AiSessionKey`, with reverse indices for hot
//! lookups: `by_transcript: PathBuf → AiSessionKey` and
//! `by_window: window_id → Vec<AiSessionKey>` (tool_history may have
//! multiple). Per-session debouncer + lifecycle state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::debouncer::Debouncer;
use crate::key::AiSessionKey;
use crate::lifecycle::{LifecycleState, SessionStatus};
#[cfg(test)]
use crate::lifecycle::LifecycleModel;

pub struct SessionTrack {
    pub key: AiSessionKey,
    pub tool: String,
    pub transcript_path: PathBuf,
    pub project_id: String,
    pub project_dir: PathBuf,

    pub lifecycle: LifecycleState,
    pub debouncer: Debouncer,
    pub status: SessionStatus,

    pub registered_at: SystemTime,
    pub ended_at: Option<SystemTime>,
}

impl std::fmt::Debug for SessionTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTrack")
            .field("key", &self.key)
            .field("tool", &self.tool)
            .field("transcript_path", &self.transcript_path)
            .field("status", &self.status)
            .field("lifecycle_model", &self.lifecycle.model())
            .finish()
    }
}

#[derive(Default)]
pub struct SessionRegistry {
    by_session: HashMap<AiSessionKey, SessionTrack>,
    by_transcript: HashMap<PathBuf, Vec<AiSessionKey>>, // Vec for F5 path-collision tolerance
    by_window: HashMap<String, Vec<AiSessionKey>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, track: SessionTrack) {
        let key = track.key.clone();
        let path = canonicalize_or_self(&track.transcript_path);
        let window = key.window_id.clone();

        self.by_transcript.entry(path).or_default().push(key.clone());
        self.by_window.entry(window).or_default().push(key.clone());
        self.by_session.insert(key, track);
    }

    pub fn remove(&mut self, key: &AiSessionKey) -> Option<SessionTrack> {
        let track = self.by_session.remove(key)?;
        let path = canonicalize_or_self(&track.transcript_path);
        if let Some(v) = self.by_transcript.get_mut(&path) {
            v.retain(|k| k != key);
            if v.is_empty() {
                self.by_transcript.remove(&path);
            }
        }
        if let Some(v) = self.by_window.get_mut(&key.window_id) {
            v.retain(|k| k != key);
            if v.is_empty() {
                self.by_window.remove(&key.window_id);
            }
        }
        Some(track)
    }

    pub fn get(&self, key: &AiSessionKey) -> Option<&SessionTrack> {
        self.by_session.get(key)
    }

    pub fn get_mut(&mut self, key: &AiSessionKey) -> Option<&mut SessionTrack> {
        self.by_session.get_mut(key)
    }

    /// Lookup all sessions whose transcript matches `path`. Usually 1; can
    /// be >1 in F5 path-collision cases (Codex transcript reuse, symlinks).
    pub fn keys_by_transcript(&self, path: &Path) -> Vec<AiSessionKey> {
        let canonical = canonicalize_or_self(path);
        self.by_transcript
            .get(&canonical)
            .cloned()
            .unwrap_or_default()
    }

    pub fn keys_by_window(&self, window_id: &str) -> Vec<AiSessionKey> {
        self.by_window.get(window_id).cloned().unwrap_or_default()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&AiSessionKey, &SessionTrack)> {
        self.by_session.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&AiSessionKey, &mut SessionTrack)> {
        self.by_session.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.by_session.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.by_session.is_empty()
    }

    pub fn watched_parent_dirs(&self) -> Vec<PathBuf> {
        let mut set = std::collections::HashSet::new();
        for path in self.by_transcript.keys() {
            if let Some(parent) = path.parent() {
                set.insert(parent.to_path_buf());
            }
        }
        set.into_iter().collect()
    }
}

fn canonicalize_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tempfile::tempdir;

    fn mk_track(window: &str, sid: &str, host: &str, transcript: PathBuf) -> SessionTrack {
        let key = AiSessionKey::new(window, sid, host);
        SessionTrack {
            key,
            tool: "claude-code".into(),
            transcript_path: transcript,
            project_id: "p".into(),
            project_dir: PathBuf::from("/tmp/p"),
            lifecycle: LifecycleState::new(LifecycleModel::JsonlAppend),
            debouncer: Debouncer::new(Default::default(), Instant::now()),
            status: SessionStatus::Active,
            registered_at: SystemTime::now(),
            ended_at: None,
        }
    }

    #[test]
    fn insert_and_get() {
        let mut reg = SessionRegistry::new();
        let dir = tempdir().unwrap();
        let t = dir.path().join("a.jsonl");
        std::fs::write(&t, b"x").unwrap();
        reg.insert(mk_track("w1", "s1", "h1", t.clone()));

        let key = AiSessionKey::new("w1", "s1", "h1");
        let track = reg.get(&key).expect("inserted");
        assert_eq!(track.tool, "claude-code");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn by_transcript_lookup_finds_session() {
        let mut reg = SessionRegistry::new();
        let dir = tempdir().unwrap();
        let t = dir.path().join("session-a.jsonl");
        std::fs::write(&t, b"x").unwrap();
        reg.insert(mk_track("w1", "s1", "h1", t.clone()));

        let keys = reg.keys_by_transcript(&t);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].vendor_session_id, "s1");
    }

    #[test]
    fn by_window_returns_all_sessions_in_history() {
        let mut reg = SessionRegistry::new();
        let dir = tempdir().unwrap();
        // Two sessions in the same window (compaction case).
        let t1 = dir.path().join("s1.jsonl");
        let t2 = dir.path().join("s2.jsonl");
        std::fs::write(&t1, b"x").unwrap();
        std::fs::write(&t2, b"x").unwrap();
        reg.insert(mk_track("w1", "s1", "h1", t1));
        reg.insert(mk_track("w1", "s2", "h1", t2));

        let keys = reg.keys_by_window("w1");
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn remove_cleans_all_indices() {
        let mut reg = SessionRegistry::new();
        let dir = tempdir().unwrap();
        let t = dir.path().join("a.jsonl");
        std::fs::write(&t, b"x").unwrap();
        let key = AiSessionKey::new("w1", "s1", "h1");
        reg.insert(mk_track("w1", "s1", "h1", t.clone()));

        let removed = reg.remove(&key);
        assert!(removed.is_some());
        assert_eq!(reg.len(), 0);
        assert!(reg.keys_by_transcript(&t).is_empty());
        assert!(reg.keys_by_window("w1").is_empty());
    }

    #[test]
    fn path_collision_via_codex_reuse_keeps_both_sessions() {
        // F5 — two sessions in registry both claim the same transcript_path.
        let mut reg = SessionRegistry::new();
        let dir = tempdir().unwrap();
        let t = dir.path().join("shared.jsonl");
        std::fs::write(&t, b"x").unwrap();
        reg.insert(mk_track("w1", "s1", "h1", t.clone()));
        reg.insert(mk_track("w2", "s2", "h1", t.clone()));

        let keys = reg.keys_by_transcript(&t);
        assert_eq!(keys.len(), 2, "both sessions returned for shared path");
    }

    #[test]
    fn watched_parent_dirs_dedupes() {
        let mut reg = SessionRegistry::new();
        let dir = tempdir().unwrap();
        let t1 = dir.path().join("a.jsonl");
        let t2 = dir.path().join("b.jsonl");
        std::fs::write(&t1, b"x").unwrap();
        std::fs::write(&t2, b"x").unwrap();
        reg.insert(mk_track("w1", "s1", "h1", t1));
        reg.insert(mk_track("w2", "s2", "h1", t2));

        let dirs = reg.watched_parent_dirs();
        // Both transcripts share the same parent.
        assert_eq!(dirs.len(), 1);
    }
}
