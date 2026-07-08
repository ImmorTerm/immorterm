//! Lazy-download manifest for optional sidecars (memory, mcp-gateway).
//!
//! The manifest is embedded at compile time via `include_str!` — a
//! freshly-installed, offline user still sees the menu of optional
//! components in the onboarding wizard and can opt in or out. When they
//! opt in, the install path uses the per-triple URL + SHA256 recorded
//! here.
//!
//! Phase 1 (this module): parse + look up entries. The downloader +
//! hash verification lands alongside the onboarding wizard UI.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

const MANIFEST_JSON: &str = include_str!("../../../../manifests/sidecars.json");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    #[serde(default)]
    pub components: BTreeMap<String, Component>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Component {
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub size_mb_approx: u32,
    #[serde(default)]
    pub default_enabled: bool,
    /// Name of the executable inside the installed tree. Windows gets
    /// `.exe` appended automatically at install time — the manifest
    /// keeps one canonical name so other platforms match it bare.
    pub binary_name: String,
    pub current_version: String,
    #[serde(default)]
    pub versions: BTreeMap<String, BTreeMap<String, Artifact>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
}

/// Parse the embedded manifest. Panics on malformed JSON — that's a
/// build-time bug, not a runtime failure, so fail loud.
pub fn load() -> Manifest {
    serde_json::from_str(MANIFEST_JSON).expect("manifests/sidecars.json malformed at build time")
}

/// Host target triple the app was built for. `std::env::consts` doesn't
/// expose the full triple, so we accept Rust's `env!("TARGET")` at build
/// time — Cargo sets it for build scripts but not for the crate itself,
/// so we synthesize from arch + os + env at runtime instead. Good enough
/// for artifact lookup; matches what rustc prints as `host:`.
pub fn host_triple() -> String {
    format!(
        "{}-{}-{}",
        std::env::consts::ARCH,
        vendor(),
        target_os_env(),
    )
}

fn vendor() -> &'static str {
    // rustc's canonical triples use `apple` on macOS/iOS, `pc` on
    // Windows, `unknown` on Linux/BSD. Good enough to match manifest
    // keys 1:1.
    match std::env::consts::OS {
        "macos" | "ios" => "apple",
        "windows" => "pc",
        _ => "unknown",
    }
}

fn target_os_env() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".to_string(),
        "windows" => "windows-msvc".to_string(),
        "linux" => "linux-gnu".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parses() {
        let m = load();
        assert_eq!(m.schema_version, 1);
        assert!(m.components.contains_key("memory"));
        assert!(m.components.contains_key("mcp-gateway"));
    }

    #[test]
    fn memory_has_artifact_for_current_host() {
        let m = load();
        let memory = &m.components["memory"];
        let version = &memory.versions[&memory.current_version];
        // Current dev host must be present — catches missed triple updates.
        let triple = host_triple();
        assert!(
            version.contains_key(&triple),
            "no memory artifact for host triple {triple}"
        );
    }

    #[test]
    fn host_triple_matches_rustc_conventions() {
        let triple = host_triple();
        // Must look like <arch>-<vendor>-<os[-env]>.
        let parts: Vec<_> = triple.split('-').collect();
        assert!(parts.len() >= 3, "bad triple: {triple}");
    }
}
