//! App-wide user preferences persisted to `~/.immorterm/preferences.json`.
//!
//! Today this holds the window zoom level; more preferences will land here
//! as the Tauri shell grows (e.g. last-used theme override, default shell).
//! Keeping one file keeps disk I/O boring and predictable.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Per-component opt-in state for optional sidecars (memory, mcp-gateway).
/// Keyed by component id in `Preferences::sidecars`. Installed version is
/// `None` while the user has opted in but the install hasn't run yet (or
/// failed) — next app boot will retry.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SidecarPref {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub installed_version: Option<String>,
}

/// User preferences — the whole file maps 1:1 onto this struct.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default = "default_version")]
    version: u32,
    /// WKWebView page zoom level. 1.0 = 100 %. Applied to every webview
    /// (shell + project) in every window so the UI zooms uniformly.
    #[serde(default = "default_zoom")]
    pub zoom: f64,
    /// Opt-in state for optional sidecars. Empty by default — users pick
    /// what to install via the onboarding wizard or preferences UI.
    #[serde(default)]
    pub sidecars: BTreeMap<String, SidecarPref>,
    /// First-run onboarding wizard has been completed. New installs get
    /// shown the wizard; flipping this to false via a preferences reset
    /// or `--show-wizard` CLI flag would replay it.
    #[serde(default)]
    pub wizard_completed: bool,
    /// Project directories the user has explicitly trusted. Opening a
    /// cwd not in this set shows the "Enable ImmorTerm for this
    /// project?" banner. Dismissed-without-enable stays out of the set
    /// so the banner re-appears next time (matches VS Code behaviour).
    /// Stored as a sorted set for deterministic preferences.json diffs.
    #[serde(default)]
    pub trusted_projects: BTreeSet<String>,
}

fn default_version() -> u32 {
    1
}
fn default_zoom() -> f64 {
    1.0
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: default_version(),
            zoom: default_zoom(),
            sidecars: BTreeMap::new(),
            wizard_completed: false,
            trusted_projects: BTreeSet::new(),
        }
    }
}

/// Managed Tauri state — lets commands + window spawn read/mutate prefs.
#[derive(Default)]
pub struct PreferencesState {
    inner: Mutex<Preferences>,
}

/// Zoom step multiplier — Chrome/Safari use ~1.1x per tick, same feels right here.
const ZOOM_STEP: f64 = 1.1;
const ZOOM_MIN: f64 = 0.5;
const ZOOM_MAX: f64 = 3.0;

impl PreferencesState {
    pub fn load() -> Self {
        let prefs = read_prefs().unwrap_or_default();
        Self {
            inner: Mutex::new(prefs),
        }
    }

    pub fn zoom(&self) -> f64 {
        self.inner.lock().unwrap().zoom
    }

    /// Multiply the current zoom by `factor`, clamp to [MIN, MAX], persist.
    /// Returns the new zoom.
    pub fn adjust_zoom(&self, factor: f64) -> f64 {
        let new_zoom;
        {
            let mut g = self.inner.lock().unwrap();
            g.zoom = (g.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
            new_zoom = g.zoom;
        }
        self.save();
        new_zoom
    }

    pub fn zoom_in(&self) -> f64 {
        self.adjust_zoom(ZOOM_STEP)
    }

    pub fn zoom_out(&self) -> f64 {
        self.adjust_zoom(1.0 / ZOOM_STEP)
    }

    pub fn reset_zoom(&self) -> f64 {
        {
            let mut g = self.inner.lock().unwrap();
            g.zoom = 1.0;
        }
        self.save();
        1.0
    }

    /// Snapshot the opt-in state for a single sidecar component.
    pub fn sidecar(&self, id: &str) -> SidecarPref {
        self.inner
            .lock()
            .unwrap()
            .sidecars
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// Flip the enable flag for a sidecar component and persist.
    /// Returns the new state. Disabling clears the installed_version so
    /// a subsequent re-enable forces a fresh install (safer default
    /// when the user has seen install-time issues).
    pub fn set_sidecar_enabled(&self, id: &str, enabled: bool) -> SidecarPref {
        let snapshot = {
            let mut g = self.inner.lock().unwrap();
            let entry = g.sidecars.entry(id.to_string()).or_default();
            entry.enabled = enabled;
            if !enabled {
                entry.installed_version = None;
            }
            entry.clone()
        };
        self.save();
        snapshot
    }

    /// Record that an install at `version` succeeded. Intended for the
    /// downloader to call after SHA256 verification passes.
    #[allow(dead_code)]
    pub fn mark_sidecar_installed(&self, id: &str, version: String) {
        {
            let mut g = self.inner.lock().unwrap();
            let entry = g.sidecars.entry(id.to_string()).or_default();
            entry.enabled = true;
            entry.installed_version = Some(version);
        }
        self.save();
    }

    pub fn wizard_completed(&self) -> bool {
        self.inner.lock().unwrap().wizard_completed
    }

    pub fn mark_wizard_completed(&self) {
        {
            let mut g = self.inner.lock().unwrap();
            g.wizard_completed = true;
        }
        self.save();
    }

    pub fn is_project_trusted(&self, project_dir: &str) -> bool {
        let g = self.inner.lock().unwrap();
        g.trusted_projects.contains(project_dir)
    }

    /// Add or remove a project_dir from the trust set. Returns the new
    /// trusted state. Normalizes paths lightly (trim trailing slash) so
    /// `/foo/bar` and `/foo/bar/` share one entry.
    pub fn set_project_trusted(&self, project_dir: &str, trusted: bool) -> bool {
        let key = project_dir.trim_end_matches('/').to_string();
        {
            let mut g = self.inner.lock().unwrap();
            if trusted {
                g.trusted_projects.insert(key.clone());
            } else {
                g.trusted_projects.remove(&key);
            }
        }
        self.save();
        trusted
    }

    fn save(&self) {
        let snapshot = self.inner.lock().unwrap().clone();
        if let Err(e) = write_prefs(&snapshot) {
            eprintln!("[preferences] save failed: {e}");
        }
    }
}

fn prefs_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".immorterm").join("preferences.json")
}

fn read_prefs() -> std::io::Result<Preferences> {
    let bytes = fs::read(prefs_path())?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn write_prefs(p: &Preferences) -> std::io::Result<()> {
    let path = prefs_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(p)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(&path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_clamps_to_bounds() {
        let p = PreferencesState::default();
        // Spam zoom-in — should stop at MAX.
        for _ in 0..50 {
            p.zoom_in();
        }
        assert!((p.zoom() - ZOOM_MAX).abs() < 0.0001, "zoom={}", p.zoom());

        // Spam zoom-out — should stop at MIN.
        for _ in 0..50 {
            p.zoom_out();
        }
        assert!((p.zoom() - ZOOM_MIN).abs() < 0.0001, "zoom={}", p.zoom());

        assert_eq!(p.reset_zoom(), 1.0);
    }

    #[test]
    fn default_zoom_is_100_percent() {
        let p = PreferencesState::default();
        assert_eq!(p.zoom(), 1.0);
    }
}
