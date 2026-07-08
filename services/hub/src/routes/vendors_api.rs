//! Vendor detection — probes the host PATH for each AI tool's CLI binary
//! and returns a per-vendor installation map. Backs the vendor-selection
//! wizard's "\u2713 Detected" badges so users see at a glance which vendors
//! are ready to use vs. need installation.
//!
//! The detection is intentionally cheap: we only check `command -v <bin>`
//! and read a known config-file path for each vendor. We do NOT run the
//! CLI to ask its version (would block the wizard for 5\u201310 seconds on
//! cold start across 8 binaries) \u2014 the wizard can show the version on
//! demand if the user asks.

use axum::Json;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One probe per vendor. The id matches the VendorId in libs/config so
/// the wizard can map directly into services.vendors.{id}.enabled.
#[derive(Debug, Serialize)]
pub struct VendorProbe {
    pub id: &'static str,
    pub display: &'static str,
    pub bin: &'static str,
    pub installed: bool,
    pub configured: bool,
    /// Path where vendor stores its OAuth/auth config; presence implies the
    /// user has logged in interactively at least once.
    pub config_path: Option<String>,
}

const VENDORS: &[(&str, &str, &str, &[&str])] = &[
    // (vendorId, display, bin-on-path, [auth-or-state-paths-relative-to-$HOME])
    //
    // The state paths are presence-indicators only \u2014 we just check
    // existence, not contents. Vendors store OAuth tokens in many places
    // (macOS Keychain, ~/Library/Application Support, etc.) so the most
    // reliable "user has used this tool" signal is the per-tool state
    // directory the tool creates on first run (sessions, projects,
    // history). False negatives (configured=true even after first run)
    // are far less harmful than false positives, so we err on detection
    // sensitivity.
    ("claudeCode", "Claude Code",       "claude",       &[".claude/sessions", ".claude/projects", ".claude/history.jsonl"]),
    ("codex",      "OpenAI Codex",      "codex",        &[".codex/sessions", ".codex/auth.json", ".codex/log"]),
    ("cursor",     "Cursor",            "cursor-agent", &[".cursor/auth.json", "Library/Application Support/cursor-agent"]),
    ("windsurf",   "Windsurf",          "windsurf",     &[".windsurf/auth.json", ".codeium"]),
    ("cline",      "Cline",             "cline",        &[".cline/auth.json", ".clinerules"]),
    ("opencode",   "opencode",          "opencode",     &[".local/share/opencode/auth.json", ".local/share/opencode"]),
    ("gemini",     "Gemini CLI",        "gemini",       &[".gemini/oauth_creds.json", ".gemini"]),
    ("copilot",    "GitHub Copilot",    "copilot",      &[".copilot/auth.json", ".copilot"]),
    ("aider",      "Aider",             "aider",        &[".aider.chat.history.md", ".aider"]),
    // Bonus: not a "vendor" per se, but useful in the wizard.
    ("llm",        "Simon Willison's `llm`", "llm",     &[".config/io.datasette.llm/keys.json", "Library/Application Support/io.datasette.llm"]),
    ("ollama",     "Ollama",            "ollama",       &[".ollama"]),
];

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn is_on_path(bin: &str) -> bool {
    if !bin.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return false;
    }
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {} >/dev/null 2>&1", bin))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn first_existing_config(home: &Path, paths: &[&str]) -> Option<String> {
    for rel in paths {
        let candidate = home.join(rel);
        if candidate.exists() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

pub async fn detect_vendors() -> Json<Value> {
    let home = home_dir();
    let probes: Vec<VendorProbe> = VENDORS
        .iter()
        .map(|(id, display, bin, paths)| {
            let installed = is_on_path(bin);
            let config_path = home
                .as_ref()
                .and_then(|h| first_existing_config(h, paths));
            VendorProbe {
                id,
                display,
                bin,
                installed,
                configured: config_path.is_some(),
                config_path,
            }
        })
        .collect();

    Json(serde_json::json!({
        "vendors": probes,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_id_format_matches_schema() {
        // VendorId in libs/config uses camelCase for compound names.
        for (id, _, _, _) in VENDORS {
            assert!(!id.contains('_'), "vendor id {} must be camelCase, not snake_case", id);
            assert!(!id.contains('-'), "vendor id {} must be camelCase, not kebab-case", id);
        }
    }

    #[test]
    fn is_on_path_rejects_injection() {
        // Defensive: caller never passes user input, but if it ever did
        // the metacharacter filter must reject it.
        assert!(!is_on_path("claude;rm -rf /"));
        assert!(!is_on_path("$(whoami)"));
        assert!(!is_on_path(".."));
    }

    #[test]
    fn known_vendor_ids_are_stable() {
        // The wizard maps these directly to services.vendors.{id}.enabled
        // in libs/config. Adding/renaming requires a schema migration.
        let ids: Vec<&str> = VENDORS.iter().map(|(id, ..)| *id).collect();
        for required in [
            "claudeCode", "codex", "cursor", "windsurf", "cline",
            "opencode", "gemini", "copilot", "aider",
        ] {
            assert!(ids.contains(&required), "missing vendor id: {}", required);
        }
    }
}
