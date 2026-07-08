//! External task-file watcher — notifies webviews when something outside
//! the hub mutates ~/.immorterm/tasks/*.json (the primary use-case is the
//! daemon's MCP tool set writing a new task while the user is typing).
//!
//! This is the Rust port of the `fs.watch(path.dirname(this.filePath), …)`
//! branch in apps/extension/src/tasks/storage.ts::watchFile(). Debounces
//! 150 ms to absorb the N events Darwin fires for atomic .tmp → rename.

use std::path::PathBuf;
use std::time::Duration;

use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::time::sleep;

use crate::events::{publish, HubEvent};

fn tasks_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".immorterm/tasks")
}

pub fn start() {
    tokio::spawn(async move {
        let dir = tasks_dir();
        // Create the directory if it doesn't exist yet — otherwise
        // notify::Watcher::watch errors out and the whole loop dies.
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("[task-watcher] failed to create {:?}: {}", dir, e);
            return;
        }

        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        let watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            let Ok(ev) = res else { return };
            if !matches!(
                ev.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) {
                return;
            }
            // Only care about *.json files; ignore the atomic .tmp sidecars.
            for p in ev.paths {
                let ok = p
                    .extension()
                    .map(|e| e == "json")
                    .unwrap_or(false);
                if ok {
                    let _ = ev_tx.send(p);
                }
            }
        });

        let mut watcher = match watcher {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("[task-watcher] watcher init failed: {}", e);
                return;
            }
        };
        if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
            tracing::warn!("[task-watcher] watch({:?}) failed: {}", dir, e);
            return;
        }

        // Debounce: coalesce storms of events per project_id into a single
        // TasksChanged publish.
        let mut pending: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut flush_handle: Option<tokio::task::JoinHandle<()>> = None;

        while let Some(path) = ev_rx.recv().await {
            let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            pending.insert(stem);
            if let Some(h) = flush_handle.take() {
                h.abort();
            }
            let ids: Vec<String> = pending.iter().cloned().collect();
            pending.clear();
            flush_handle = Some(tokio::spawn(async move {
                sleep(Duration::from_millis(150)).await;
                for id in ids {
                    tracing::info!("[task-watcher] TasksChanged project_id={}", id);
                    publish(HubEvent::TasksChanged { project_id: id });
                }
            }));
        }
    });
}
