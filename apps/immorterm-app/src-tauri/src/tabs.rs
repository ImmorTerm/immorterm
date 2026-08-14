//! Tab registry — one live Webview per open project tab.
//!
//! The registry is the single source of truth for which projects are open,
//! in what order, and which one is focused. State is persisted to
//! `~/.immorterm/tabs.json`, keyed by **window label**, so Cmd+N windows
//! survive app restarts alongside the "main" window.
//!
//! See internal design notes for the full architecture.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Tab flavour — project (full ImmorTerm experience, memory/AI/sessions
/// all active) vs plain (bare shell, no ImmorTerm side-effects). Default
/// "project" so older persisted tabs deserialize unchanged.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TabMode {
    #[default]
    Project,
    Plain,
}

impl TabMode {
    pub fn as_query(self) -> &'static str {
        match self {
            TabMode::Project => "project",
            TabMode::Plain => "plain",
        }
    }
}

/// One project tab — metadata only. The actual Webview lives in tauri's
/// window as a child keyed by `tab_webview_label(window_label, tab_id)`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub project_dir: String,
    pub project_name: String,
    #[serde(default)]
    pub created_at_ms: u64,
    /// Persisted across restarts so plain tabs don't silently revert to
    /// project mode on relaunch.
    #[serde(default)]
    pub mode: TabMode,
    /// Remote ImmorTerm host this tab is bound to, if any. Empty/None for
    /// local tabs. Propagated into the gpu-terminal.html URL as `?remote=`
    /// so the renderer hits the remote-aware registry endpoint and routes
    /// session WS connections through the hub's SSH tunnel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// Set only while restoring a legacy tab whose remote field was absent
    /// and could not be resolved safely. The webview uses this to stop before
    /// attempting a local filesystem read and show an actionable picker hint.
    #[serde(default, skip_serializing_if = "is_false")]
    pub remote_identity_unresolved: bool,
    /// Candidate configured remotes that own the same project path. More than
    /// one is deliberately never auto-selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_candidates: Vec<String>,
}

impl Tab {
    pub fn new(project_dir: String, project_name: Option<String>) -> Self {
        Self::with_mode(project_dir, project_name, TabMode::Project, None)
    }

    pub fn with_mode(
        project_dir: String,
        project_name: Option<String>,
        mode: TabMode,
        remote: Option<String>,
    ) -> Self {
        let name = project_name.unwrap_or_else(|| match mode {
            TabMode::Plain => "Terminal".to_string(),
            TabMode::Project => std::path::Path::new(&project_dir)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| project_dir.clone()),
        });
        Self {
            id: next_id(),
            project_dir,
            project_name: name,
            created_at_ms: now_ms(),
            mode,
            remote,
            remote_identity_unresolved: false,
            remote_candidates: Vec::new(),
        }
    }
}

/// Per-window on-disk state — the full `tabs.json` is `HashMap<label, WindowState>`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WindowState {
    #[serde(default)]
    pub tabs: Vec<Tab>,
    #[serde(default)]
    pub active_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedTabs {
    #[serde(default = "default_version")]
    version: u32,
    /// Snapshot of every known window. Windows without entries default to
    /// an empty tabs list on spawn (picker then offers the cwd).
    #[serde(default)]
    windows: HashMap<String, WindowState>,
}

fn default_version() -> u32 {
    1
}

const CURRENT_VERSION: u32 = 3;

fn is_false(value: &bool) -> bool {
    !*value
}

/// Per-window in-memory registry. Wrap in `Arc<TabRegistry>` and hand to
/// `tauri::Manager::manage` via `WindowsState`.
pub struct TabRegistry {
    window_label: String,
    inner: Mutex<WindowState>,
    ephemeral: bool,
}

impl TabRegistry {
    /// Load the `window_label`'s slice from the on-disk store, or start
    /// empty if this window isn't in the file yet.
    pub fn load(window_label: &str) -> Self {
        let state = read_store()
            .unwrap_or_default()
            .windows
            .remove(window_label)
            .unwrap_or_default();
        Self {
            window_label: window_label.to_string(),
            inner: Mutex::new(state),
            ephemeral: false,
        }
    }

    /// In-memory registry that never touches disk. Used in tests.
    #[allow(dead_code)]
    pub fn ephemeral(window_label: impl Into<String>) -> Self {
        Self {
            window_label: window_label.into(),
            inner: Mutex::new(WindowState::default()),
            ephemeral: true,
        }
    }

    pub fn snapshot(&self) -> (Vec<Tab>, Option<String>) {
        let g = self.inner.lock().unwrap();
        (g.tabs.clone(), g.active_id.clone())
    }

    /// True iff a tab with this id exists in this registry. Used by
    /// the control_api open_tab handler to find which window owns the
    /// requesting webview when multiple windows are open.
    pub fn has(&self, tab_id: &str) -> bool {
        self.inner.lock().unwrap().tabs.iter().any(|t| t.id == tab_id)
    }

    pub fn tabs(&self) -> Vec<Tab> {
        self.inner.lock().unwrap().tabs.clone()
    }

    pub fn active_id(&self) -> Option<String> {
        self.inner.lock().unwrap().active_id.clone()
    }

    /// Kept for backwards compat / tests. Matches a tab purely by path,
    /// ignoring the remote field. New call sites prefer
    /// `find_by_project_dir_and_remote` which disambiguates local vs
    /// remote tabs that share a project path.
    pub fn find_by_project_dir(&self, project_dir: &str) -> Option<Tab> {
        self.find_by_project_dir_and_remote(project_dir, None)
    }

    /// Like `find_by_project_dir` but disambiguates by remote host. Local
    /// `/work` and `hetzner:/work` are distinct tabs from the user's
    /// perspective — same path, different machine. Used by the picker's
    /// dedupe check so opening a remote tab doesn't focus a pre-existing
    /// local one with the same path.
    pub fn find_by_project_dir_and_remote(
        &self,
        project_dir: &str,
        remote: Option<&str>,
    ) -> Option<Tab> {
        self.inner
            .lock()
            .unwrap()
            .tabs
            .iter()
            .find(|t| t.project_dir == project_dir && t.remote.as_deref() == remote)
            .cloned()
    }

    pub fn add(&self, tab: Tab) {
        {
            let mut g = self.inner.lock().unwrap();
            if g.tabs.iter().any(|t| t.id == tab.id) {
                return;
            }
            g.tabs.push(tab.clone());
            if g.active_id.is_none() {
                g.active_id = Some(tab.id);
            }
        }
        self.persist();
    }

    pub fn set_active(&self, id: &str) -> bool {
        let ok;
        {
            let mut g = self.inner.lock().unwrap();
            ok = g.tabs.iter().any(|t| t.id == id);
            if ok {
                g.active_id = Some(id.to_string());
            }
        }
        if ok {
            self.persist();
        }
        ok
    }

    /// Remove the tab with `id`. Returns the id of the tab that should
    /// become active after removal (or `None` if the list is now empty).
    pub fn remove(&self, id: &str) -> Option<String> {
        let next_active;
        {
            let mut g = self.inner.lock().unwrap();
            let idx = match g.tabs.iter().position(|t| t.id == id) {
                Some(i) => i,
                None => return g.active_id.clone(),
            };
            g.tabs.remove(idx);
            if g.active_id.as_deref() == Some(id) {
                let fallback = g.tabs.get(idx).or_else(|| g.tabs.get(idx.saturating_sub(1)));
                g.active_id = fallback.map(|t| t.id.clone());
            }
            next_active = g.active_id.clone();
        }
        self.persist();
        next_active
    }

    #[allow(dead_code)]
    pub fn reorder(&self, ordered_ids: &[String]) {
        {
            let mut g = self.inner.lock().unwrap();
            let mut by_id: HashMap<String, Tab> =
                g.tabs.drain(..).map(|t| (t.id.clone(), t)).collect();
            for id in ordered_ids {
                if let Some(t) = by_id.remove(id) {
                    g.tabs.push(t);
                }
            }
            for (_, t) in by_id.into_iter() {
                g.tabs.push(t);
            }
        }
        self.persist();
    }

    /// Drop this window's section from the on-disk store. Called when a
    /// window closes so the file doesn't accumulate dead entries.
    #[allow(dead_code)]
    pub fn forget_on_disk(&self) {
        if self.ephemeral {
            return;
        }
        let mut store = read_store().unwrap_or_default();
        if store.windows.remove(&self.window_label).is_some() {
            let _ = write_store(&store);
        }
    }

    fn persist(&self) {
        if self.ephemeral {
            return;
        }
        let snapshot = self.inner.lock().unwrap().clone();
        let mut store = read_store().unwrap_or_default();
        store.version = CURRENT_VERSION;
        store.windows.insert(self.window_label.clone(), snapshot);
        if let Err(e) = write_store(&store) {
            eprintln!("[tabs] persist failed: {e}");
        }
    }
}

/// List every window label that has a persisted tab list. Used at app
/// boot to decide which windows to recreate.
pub fn persisted_window_labels() -> Vec<String> {
    read_store()
        .map(|s| s.windows.keys().cloned().collect())
        .unwrap_or_default()
}

fn store_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".immorterm").join("tabs.json")
}

fn read_store() -> std::io::Result<PersistedTabs> {
    let bytes = fs::read(store_path())?;
    let mut store: PersistedTabs = serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if migrate_legacy_remote_identity(&mut store) {
        write_store(&store)?;
    }
    Ok(store)
}

/// Upgrade pre-v3 tabs which did not reliably retain remote identity.
/// A path is migrated only when exactly one persisted remote tab owns it and
/// the path does not exist locally. Multiple candidates are recorded for an
/// actionable UI, never guessed. A missing local path with no evidence is
/// marked unresolved so Files never leaks a raw macOS read_dir error.
fn migrate_legacy_remote_identity(store: &mut PersistedTabs) -> bool {
    if store.version >= CURRENT_VERSION {
        return false;
    }
    let mut owners: HashMap<String, Vec<String>> = HashMap::new();
    for state in store.windows.values() {
        for tab in &state.tabs {
            if let Some(remote) = tab.remote.as_deref().filter(|remote| !remote.is_empty()) {
                let remotes = owners.entry(tab.project_dir.clone()).or_default();
                if !remotes.iter().any(|candidate| candidate == remote) {
                    remotes.push(remote.to_string());
                }
            }
        }
    }
    for remotes in owners.values_mut() {
        remotes.sort();
    }

    for state in store.windows.values_mut() {
        for tab in &mut state.tabs {
            if tab.remote.is_some() || std::path::Path::new(&tab.project_dir).exists() {
                continue;
            }
            let candidates = owners.get(&tab.project_dir).cloned().unwrap_or_default();
            if candidates.len() == 1 {
                tab.remote = candidates.first().cloned();
                tab.remote_identity_unresolved = false;
                tab.remote_candidates.clear();
            } else {
                tab.remote_identity_unresolved = true;
                tab.remote_candidates = candidates;
            }
        }
    }
    store.version = CURRENT_VERSION;
    true
}

fn write_store(s: &PersistedTabs) -> std::io::Result<()> {
    let p = store_path();
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(&p, bytes)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn next_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}{:x}", now_ms(), n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_remove_picks_neighbour() {
        let reg = TabRegistry::ephemeral("test");
        let a = Tab::new("/a".into(), Some("A".into()));
        let b = Tab::new("/b".into(), Some("B".into()));
        let c = Tab::new("/c".into(), Some("C".into()));
        reg.add(a.clone());
        reg.add(b.clone());
        reg.add(c.clone());
        assert!(reg.set_active(&b.id));

        let next = reg.remove(&b.id);
        assert_eq!(next, Some(c.id.clone()));
        assert_eq!(reg.tabs().len(), 2);
    }

    #[test]
    fn reorder_preserves_unknown_tabs() {
        let reg = TabRegistry::ephemeral("test");
        let a = Tab::new("/a".into(), None);
        let b = Tab::new("/b".into(), None);
        reg.add(a.clone());
        reg.add(b.clone());
        reg.reorder(&[b.id.clone()]);
        let tabs = reg.tabs();
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].id, b.id);
        assert_eq!(tabs[1].id, a.id);
    }

    #[test]
    fn find_by_project_dir() {
        let reg = TabRegistry::ephemeral("test");
        let a = Tab::new("/projects/foo".into(), None);
        reg.add(a.clone());
        let found = reg.find_by_project_dir("/projects/foo").unwrap();
        assert_eq!(found.id, a.id);
        assert!(reg.find_by_project_dir("/projects/bar").is_none());
    }

    #[test]
    fn remove_last_clears_active() {
        let reg = TabRegistry::ephemeral("test");
        let a = Tab::new("/a".into(), None);
        reg.add(a.clone());
        let next = reg.remove(&a.id);
        assert_eq!(next, None);
        assert_eq!(reg.active_id(), None);
    }

    fn legacy_tab(id: &str, path: &str, remote: Option<&str>) -> Tab {
        Tab {
            id: id.into(),
            project_dir: path.into(),
            project_name: "Landing".into(),
            created_at_ms: 1,
            mode: TabMode::Project,
            remote: remote.map(str::to_string),
            remote_identity_unresolved: false,
            remote_candidates: Vec::new(),
        }
    }

    #[test]
    fn legacy_restore_recovers_one_unambiguous_remote_and_persists_v3() {
        let path = "/definitely-not-local/landing-remote-migration";
        let mut store = PersistedTabs {
            version: 2,
            windows: HashMap::from([
                (
                    "main".into(),
                    WindowState {
                        tabs: vec![legacy_tab("remote", path, Some("docker"))],
                        active_id: None,
                    },
                ),
                (
                    "window-4".into(),
                    WindowState {
                        tabs: vec![legacy_tab("legacy", path, None)],
                        active_id: None,
                    },
                ),
            ]),
        };
        assert!(migrate_legacy_remote_identity(&mut store));
        let restored = &store.windows["window-4"].tabs[0];
        assert_eq!(restored.remote.as_deref(), Some("docker"));
        assert!(!restored.remote_identity_unresolved);
        assert_eq!(store.version, CURRENT_VERSION);
        assert!(
            !migrate_legacy_remote_identity(&mut store),
            "migration is one-shot"
        );
    }

    #[test]
    fn legacy_restore_never_guesses_between_multiple_remotes() {
        let path = "/definitely-not-local/shared-remote-path";
        let mut store = PersistedTabs {
            version: 2,
            windows: HashMap::from([
                (
                    "main".into(),
                    WindowState {
                        tabs: vec![
                            legacy_tab("docker", path, Some("docker")),
                            legacy_tab("prod", path, Some("prod")),
                        ],
                        active_id: None,
                    },
                ),
                (
                    "window-4".into(),
                    WindowState {
                        tabs: vec![legacy_tab("legacy", path, None)],
                        active_id: None,
                    },
                ),
            ]),
        };
        migrate_legacy_remote_identity(&mut store);
        let restored = &store.windows["window-4"].tabs[0];
        assert_eq!(restored.remote, None);
        assert!(restored.remote_identity_unresolved);
        assert_eq!(restored.remote_candidates, vec!["docker", "prod"]);
    }

    #[test]
    fn remote_identity_survives_store_round_trip() {
        let tab = legacy_tab("remote", "/root/projects/landing", Some("docker"));
        let store = PersistedTabs {
            version: CURRENT_VERSION,
            windows: HashMap::from([(
                "main".into(),
                WindowState {
                    tabs: vec![tab],
                    active_id: Some("remote".into()),
                },
            )]),
        };

        let encoded = serde_json::to_vec(&store).unwrap();
        let restored: PersistedTabs = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.windows["main"].tabs[0].remote.as_deref(), Some("docker"));
    }
}
