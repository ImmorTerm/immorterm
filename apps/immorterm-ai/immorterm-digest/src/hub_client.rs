//! Typed client for the four hub endpoints the daemon needs (v4 §3).
//!
//! Endpoints:
//! - GET  /api/v1/registry/window/{window_id}        (§3.1)
//! - GET  /api/v1/registry/by-transcript?path=...    (§3.3)
//! - POST /api/v1/registry/session-link              (§3.4)
//! - POST /api/v1/registry/session-end               (§3.5)
//!
//! All hub writes (session-link, session-end) MUST go through this
//! client. When the hub is unreachable, writes are queued to
//! `~/.immorterm/digest-queue.jsonl` (WAL — append-only, one JSON object
//! per line) and drained on recovery. See v4 §3.6.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Hub HTTP root, e.g. `http://127.0.0.1:1440`.
#[derive(Debug, Clone)]
pub struct HubClient {
    base_url: String,
    http: reqwest::Client,
}

impl HubClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("reqwest build should not fail"),
        }
    }

    /// Resolve the hub URL from `$IMMORTERM_HUB_URL` or default `http://127.0.0.1:1440`.
    pub fn from_env() -> Self {
        let url = std::env::var("IMMORTERM_HUB_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:1440".to_string());
        Self::new(url)
    }

    pub async fn get_window(&self, window_id: &str) -> Result<Option<Value>> {
        let url = format!("{}/api/v1/registry/window/{}", self.base_url, urlenc(window_id));
        let resp = self.http.get(&url).send().await.context("hub get_window")?;
        match resp.status().as_u16() {
            200 => Ok(Some(resp.json().await.context("decode window")?)),
            404 => Ok(None),
            other => anyhow::bail!("hub returned {} for {}", other, url),
        }
    }

    pub async fn get_by_transcript(&self, path: &str) -> Result<Option<Value>> {
        let url = format!(
            "{}/api/v1/registry/by-transcript?path={}",
            self.base_url,
            urlenc(path)
        );
        let resp = self.http.get(&url).send().await.context("hub by_transcript")?;
        match resp.status().as_u16() {
            200 => Ok(Some(resp.json().await.context("decode by_transcript")?)),
            404 => Ok(None),
            other => anyhow::bail!("hub returned {} for {}", other, url),
        }
    }

    pub async fn post_session_link(&self, req: &SessionLinkRequest) -> Result<Value> {
        let url = format!("{}/api/v1/registry/session-link", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(req)
            .send()
            .await
            .context("hub session_link")?;
        let status = resp.status();
        let body: Value = resp.json().await.context("decode session_link")?;
        if !status.is_success() {
            anyhow::bail!("hub session-link returned {}: {}", status, body);
        }
        Ok(body)
    }

    pub async fn post_session_end(&self, req: &SessionEndRequest) -> Result<Value> {
        let url = format!("{}/api/v1/registry/session-end", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(req)
            .send()
            .await
            .context("hub session_end")?;
        let status = resp.status();
        let body: Value = resp.json().await.context("decode session_end")?;
        if !status.is_success() {
            anyhow::bail!("hub session-end returned {}: {}", status, body);
        }
        Ok(body)
    }
}

/// Trivial URL encoder for our limited use (window_id is hex/digit-only,
/// path may contain `/`, `.`, `-`, `_`). Avoids pulling in a crate just
/// for this — we only need it for two callsites.
fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            other => format!("%{:02X}", other),
        })
        .collect()
}

// ─── Request types (mirrors hub's serde shapes) ────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SessionLinkRequest {
    pub window_id: String,
    pub tool: String,
    pub session_id: String,
    pub transcript_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comm: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionEndRequest {
    pub window_id: String,
    pub vendor_session_id: String,
    pub exit_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

// ─── Write-ahead log for hub-down failover ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum WalEntry {
    SessionLink(SessionLinkRequest),
    SessionEnd(SessionEndRequest),
}

impl<'de> Deserialize<'de> for SessionLinkRequest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            window_id: String,
            tool: String,
            session_id: String,
            transcript_path: String,
            pid: Option<u32>,
            host_id: Option<String>,
            comm: Option<String>,
        }
        let r = Raw::deserialize(d)?;
        Ok(Self {
            window_id: r.window_id,
            tool: r.tool,
            session_id: r.session_id,
            transcript_path: r.transcript_path,
            pid: r.pid,
            host_id: r.host_id,
            comm: r.comm,
        })
    }
}

impl<'de> Deserialize<'de> for SessionEndRequest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            window_id: String,
            vendor_session_id: String,
            exit_reason: String,
            host_id: Option<String>,
            ended_at: Option<String>,
        }
        let r = Raw::deserialize(d)?;
        Ok(Self {
            window_id: r.window_id,
            vendor_session_id: r.vendor_session_id,
            exit_reason: r.exit_reason,
            host_id: r.host_id,
            ended_at: r.ended_at,
        })
    }
}

pub struct Wal {
    path: PathBuf,
}

impl Wal {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".immorterm").join("digest-queue.jsonl")
    }

    /// Append a request to the WAL. Each entry is one JSON object per line.
    pub fn append(&self, entry: &WalEntry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut line = serde_json::to_string(entry).context("serialize wal entry")?;
        line.push('\n');
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open wal {}", self.path.display()))?;
        f.write_all(line.as_bytes()).context("write wal entry")?;
        f.flush().context("flush wal")?;
        Ok(())
    }

    /// Read all pending entries. Caller drains via post + delete.
    pub fn pending(&self) -> Result<Vec<WalEntry>> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => s
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|l| serde_json::from_str(l).context("parse wal line"))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(anyhow::Error::new(e).context("read wal")),
        }
    }

    /// Atomically clear the WAL after successful drain.
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow::Error::new(e).context("clear wal")),
        }
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_link() -> SessionLinkRequest {
        SessionLinkRequest {
            window_id: "w1".into(),
            tool: "claude-code".into(),
            session_id: "s1".into(),
            transcript_path: "/tmp/s1.jsonl".into(),
            pid: Some(123),
            host_id: Some("h1".into()),
            comm: Some("claude".into()),
        }
    }

    fn sample_end() -> SessionEndRequest {
        SessionEndRequest {
            window_id: "w1".into(),
            vendor_session_id: "s1".into(),
            exit_reason: "idle_timeout".into(),
            host_id: Some("h1".into()),
            ended_at: Some("2026-05-12T12:00:00Z".into()),
        }
    }

    #[test]
    fn urlenc_passes_simple_chars() {
        assert_eq!(urlenc("/Users/example/x.jsonl"), "/Users/example/x.jsonl");
        assert_eq!(urlenc("22611-c81a8c92"), "22611-c81a8c92");
    }

    #[test]
    fn urlenc_escapes_special() {
        assert_eq!(urlenc("a b"), "a%20b");
        assert_eq!(urlenc("?key=val"), "%3Fkey%3Dval");
    }

    #[test]
    fn session_link_serializes_with_only_provided_fields() {
        let mut req = sample_link();
        req.pid = None;
        req.comm = None;
        let s = serde_json::to_string(&req).unwrap();
        // pid and comm omitted (skip_serializing_if None)
        assert!(!s.contains("\"pid\""));
        assert!(!s.contains("\"comm\""));
        assert!(s.contains("\"host_id\":\"h1\""));
    }

    #[test]
    fn wal_append_and_pending_roundtrip() {
        let dir = tempdir().unwrap();
        let wal = Wal::at(dir.path().join("queue.jsonl"));
        wal.append(&WalEntry::SessionLink(sample_link())).unwrap();
        wal.append(&WalEntry::SessionEnd(sample_end())).unwrap();
        let pending = wal.pending().unwrap();
        assert_eq!(pending.len(), 2);
        match &pending[0] {
            WalEntry::SessionLink(r) => assert_eq!(r.window_id, "w1"),
            _ => panic!("expected SessionLink first"),
        }
        match &pending[1] {
            WalEntry::SessionEnd(r) => assert_eq!(r.exit_reason, "idle_timeout"),
            _ => panic!("expected SessionEnd second"),
        }
    }

    #[test]
    fn wal_clear_removes_file() {
        let dir = tempdir().unwrap();
        let wal = Wal::at(dir.path().join("queue.jsonl"));
        wal.append(&WalEntry::SessionLink(sample_link())).unwrap();
        assert!(wal.path().exists());
        wal.clear().unwrap();
        assert!(!wal.path().exists());
        // Clear of missing file is a no-op
        wal.clear().unwrap();
    }

    #[test]
    fn wal_pending_on_missing_file_is_empty() {
        let dir = tempdir().unwrap();
        let wal = Wal::at(dir.path().join("never-created.jsonl"));
        assert!(wal.pending().unwrap().is_empty());
    }
}
