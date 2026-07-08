//! Ref-counted FS-watcher hub: one `notify::RecommendedWatcher` per
//! transcript-parent directory.
//!
//! Per v4 §7 #16 (F3 fix), watcher topology is `HashMap<PathBuf,
//! RecommendedWatcher>` keyed by `transcript_path.parent()`. Ref-counted
//! by the count of `AiSessionKey`s whose transcript lives in that dir.
//! `RecursiveMode::NonRecursive` — vendor transcripts live directly in
//! the watched dir, not subtrees. Recursive would balloon FD count on
//! `~/.claude/projects/`.
//!
//! Stat-polling for idle-detection happens in the orchestrator — this
//! module only delivers burst events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::thread;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum FsSignal {
    Modify(PathBuf),
    Create(PathBuf),
    Remove(PathBuf),
}

/// Hub owning the global event channel + per-directory watchers.
pub struct WatcherHub {
    sync_tx: std_mpsc::Sender<notify::Result<Event>>,
    by_dir: HashMap<PathBuf, DirWatch>,
    _bridge: thread::JoinHandle<()>,
}

struct DirWatch {
    // RAII keep-alive: dropping the watcher stops the FS watch.
    #[allow(dead_code)]
    watcher: RecommendedWatcher,
    refcount: u32,
}

impl WatcherHub {
    /// Spawn the global bridge thread and return (hub, receiver of FsSignal).
    pub fn start() -> Result<(Self, mpsc::UnboundedReceiver<FsSignal>)> {
        let (sync_tx, sync_rx) = std_mpsc::channel::<notify::Result<Event>>();
        let (async_tx, async_rx) = mpsc::unbounded_channel();

        let bridge = thread::Builder::new()
            .name("digest-fs-bridge".into())
            .spawn(move || {
                while let Ok(res) = sync_rx.recv() {
                    let signals = match res {
                        Ok(ev) => translate(ev),
                        Err(e) => {
                            tracing::warn!("notify error: {e}");
                            continue;
                        }
                    };
                    for s in signals {
                        if async_tx.send(s).is_err() {
                            // receiver dropped — daemon shutting down
                            return;
                        }
                    }
                }
            })
            .context("spawn fs bridge thread")?;

        Ok((Self { sync_tx, by_dir: HashMap::new(), _bridge: bridge }, async_rx))
    }

    /// Acquire a watch on `dir`. Idempotent — refcount++ if already watched.
    /// Returns whether the watch was newly created (true) or refcount bumped (false).
    pub fn acquire(&mut self, dir: &Path) -> Result<bool> {
        let key = canonicalize_or_self(dir);
        if let Some(dw) = self.by_dir.get_mut(&key) {
            dw.refcount += 1;
            return Ok(false);
        }

        let mut watcher = make_watcher(self.sync_tx.clone())?;
        watcher
            .watch(&key, RecursiveMode::NonRecursive)
            .with_context(|| format!("watch {}", key.display()))?;
        self.by_dir.insert(key, DirWatch { watcher, refcount: 1 });
        Ok(true)
    }

    /// Release a watch on `dir`. Refcount--; if zero, drop watcher.
    /// Returns whether the watcher was dropped.
    pub fn release(&mut self, dir: &Path) -> Result<bool> {
        let key = canonicalize_or_self(dir);
        let dropped = if let Some(dw) = self.by_dir.get_mut(&key) {
            dw.refcount = dw.refcount.saturating_sub(1);
            dw.refcount == 0
        } else {
            return Ok(false);
        };
        if dropped {
            self.by_dir.remove(&key);
        }
        Ok(dropped)
    }

    pub fn refcount(&self, dir: &Path) -> u32 {
        let key = canonicalize_or_self(dir);
        self.by_dir.get(&key).map(|d| d.refcount).unwrap_or(0)
    }

    pub fn watched_dirs(&self) -> impl Iterator<Item = &Path> {
        self.by_dir.keys().map(|p| p.as_path())
    }
}

/// macOS canonicalizes `/var/folders/...` to `/private/var/folders/...`.
/// We canonicalize at watch-set entry so the watched-key matches what
/// notify will emit (it emits canonical paths).
fn canonicalize_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn make_watcher(tx: std_mpsc::Sender<notify::Result<Event>>) -> Result<RecommendedWatcher> {
    let watcher = notify::recommended_watcher(move |res| {
        // ignore SendError — bridge thread is gone, daemon shutting down
        let _ = tx.send(res);
    })
    .context("create notify watcher")?;
    Ok(watcher)
}

fn translate(ev: Event) -> Vec<FsSignal> {
    match ev.kind {
        EventKind::Modify(_) => ev.paths.into_iter().map(FsSignal::Modify).collect(),
        EventKind::Create(_) => ev.paths.into_iter().map(FsSignal::Create).collect(),
        EventKind::Remove(_) => ev.paths.into_iter().map(FsSignal::Remove).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::time::timeout;

    async fn await_filename(
        rx: &mut mpsc::UnboundedReceiver<FsSignal>,
        target: &str,
        budget: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match timeout(remaining, rx.recv()).await {
                Ok(Some(FsSignal::Modify(p) | FsSignal::Create(p)))
                    if p.file_name().and_then(|f| f.to_str()) == Some(target) =>
                {
                    return true;
                }
                Ok(Some(_)) => continue,
                Ok(None) => return false,
                Err(_) => return false,
            }
        }
    }

    #[tokio::test]
    async fn acquire_creates_watcher_and_fires_on_write() {
        let dir = tempdir().unwrap();
        let (mut hub, mut rx) = WatcherHub::start().unwrap();
        let newly_created = hub.acquire(dir.path()).unwrap();
        assert!(newly_created);
        assert_eq!(hub.refcount(dir.path()), 1);

        tokio::time::sleep(Duration::from_millis(100)).await; // FSEvents prime
        std::fs::write(dir.path().join("alpha.jsonl"), b"hi").unwrap();

        assert!(await_filename(&mut rx, "alpha.jsonl", Duration::from_secs(3)).await);
    }

    #[tokio::test]
    async fn acquire_twice_increments_refcount_without_double_watching() {
        let dir = tempdir().unwrap();
        let (mut hub, _rx) = WatcherHub::start().unwrap();
        let newly = hub.acquire(dir.path()).unwrap();
        assert!(newly);
        let newly2 = hub.acquire(dir.path()).unwrap();
        assert!(!newly2, "second acquire should not create new watcher");
        assert_eq!(hub.refcount(dir.path()), 2);
    }

    #[tokio::test]
    async fn release_drops_watcher_when_refcount_hits_zero() {
        let dir = tempdir().unwrap();
        let (mut hub, mut rx) = WatcherHub::start().unwrap();
        hub.acquire(dir.path()).unwrap();
        hub.acquire(dir.path()).unwrap();
        // Two releases — first refcount→1, second refcount→0
        let dropped_first = hub.release(dir.path()).unwrap();
        assert!(!dropped_first);
        let dropped_second = hub.release(dir.path()).unwrap();
        assert!(dropped_second);
        assert_eq!(hub.refcount(dir.path()), 0);

        // After release, writes should not deliver
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(dir.path().join("ghost.jsonl"), b"x").unwrap();
        assert!(!await_filename(&mut rx, "ghost.jsonl", Duration::from_millis(400)).await);
    }

    #[tokio::test]
    async fn release_unknown_dir_is_noop() {
        let dir = tempdir().unwrap();
        let (mut hub, _rx) = WatcherHub::start().unwrap();
        let dropped = hub.release(dir.path()).unwrap();
        assert!(!dropped);
    }
}
