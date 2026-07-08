//! Configuration for immorterm-hub — paths, state file, config.json reading.

use std::path::PathBuf;
use std::sync::OnceLock;

/// Canonical hub port. Single source of truth inside the hub crate —
/// the CLI `--port` default and the per-remote `hub_port` default both
/// derive from it. Mirrors HUB_PORT in the VS Code (hub-sidecar.ts)
/// and Tauri (hub_sidecar.rs) sidecars.
pub const DEFAULT_HUB_PORT: u16 = 1440;

/// Static dir resolved at startup. Handlers read it via `static_dir_path()`
/// without threading it through extractor state.
static STATIC_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn set_static_dir(path: PathBuf) {
    let _ = STATIC_DIR.set(path);
}

pub fn static_dir_path() -> PathBuf {
    STATIC_DIR.get().cloned().unwrap_or_else(default_static_dir)
}

/// Base directory for all ImmorTerm data.
fn data_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(".immorterm")
}

/// PID file path.
pub fn pid_path() -> PathBuf {
    data_dir().join("hub.pid")
}

/// State file path — written on startup with pid, port, version.
pub fn state_path() -> PathBuf {
    data_dir().join("hub.state.json")
}

/// Default static directory: the VS Code extension resources folder.
pub fn default_static_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("Development/immorterm/apps/extension/resources")
}

/// Read the port from the state file (if it exists and the process is alive).
pub fn read_state_port() -> Option<u16> {
    let content = std::fs::read_to_string(state_path()).ok()?;
    let state: serde_json::Value = serde_json::from_str(&content).ok()?;
    let pid = state.get("pid")?.as_u64()? as u32;
    let port = state.get("port")?.as_u64()? as u16;

    let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
    if alive {
        Some(port)
    } else {
        let _ = std::fs::remove_file(state_path());
        None
    }
}

/// Write the state file after successful port binding.
pub fn write_state(port: u16) -> anyhow::Result<()> {
    let state = serde_json::json!({
        "pid": std::process::id(),
        "port": port,
        "startedAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        "version": env!("CARGO_PKG_VERSION"),
    });
    // Ensure parent dir exists
    if let Some(parent) = state_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(state_path(), serde_json::to_string_pretty(&state)?)?;
    Ok(())
}

/// Delete the state file (called during graceful shutdown).
pub fn delete_state() {
    let _ = std::fs::remove_file(state_path());
}

/// Read ~/.immorterm/config.json for theme, preferences, etc.
pub fn read_config() -> serde_json::Value {
    let path = data_dir().join("config.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Read per-project config from `<project_dir>/.immorterm/config.json`.
pub fn read_project_config(project_dir: &str) -> serde_json::Value {
    let path = std::path::Path::new(project_dir).join(".immorterm/config.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Write per-project config at `<project_dir>/.immorterm/config.json`. Creates
/// the parent directory if needed. Returns the path written.
pub fn write_project_config(
    project_dir: &str,
    config: &serde_json::Value,
) -> anyhow::Result<PathBuf> {
    let base = std::path::Path::new(project_dir).join(".immorterm");
    std::fs::create_dir_all(&base)?;
    let path = base.join("config.json");
    std::fs::write(&path, serde_json::to_string_pretty(config)?)?;
    Ok(path)
}

/// Discover the memory service URL from its state file.
pub fn discover_memory_url() -> Option<String> {
    let path = data_dir().join("memory.state.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let state: serde_json::Value = serde_json::from_str(&content).ok()?;
    let pid = state.get("pid")?.as_u64()? as u32;
    let port = state.get("port")?.as_u64()? as u16;

    let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
    if alive {
        Some(format!("http://localhost:{}", port))
    } else {
        None
    }
}
