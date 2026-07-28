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
    /// Whether this vendor gates hook execution behind an interactive trust
    /// prompt that the user has not yet accepted for this project.
    ///
    /// Only Codex does this today: it fingerprints each hook in
    /// `.codex/hooks.json` and runs NONE of them until the user picks
    /// "Trust all and continue", recording approval in `~/.codex/config.toml`
    /// under `[hooks.state."<abs path>:<event>:0:0"]`. Without surfacing it,
    /// the wizard's promise that ticking a vendor feeds memory is silently
    /// false. `None` = the vendor has no such gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks_trusted: Option<bool>,
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

/// Whether Codex has any trusted hook entries for `project_dir`.
///
/// Reads `[hooks.state."<abs>/.codex/hooks.json:<event>:0:0"]` keys out of
/// `~/.codex/config.toml`. Matching on the path prefix is enough — we only
/// need to know whether the user has been through the trust prompt, not which
/// specific hooks were approved. Deliberately does NOT parse or verify
/// `trusted_hash`: that is Codex's security control, and reproducing it would
/// both defeat the control and break on any version bump.
fn codex_hooks_trusted(home: &Path, project_dir: Option<&str>) -> Option<bool> {
    let project_dir = project_dir?;
    let config = std::fs::read_to_string(home.join(".codex").join("config.toml")).ok()?;
    let needle = format!("{}/.codex/hooks.json:", project_dir.trim_end_matches('/'));
    Some(
        config
            .lines()
            .filter(|l| l.trim_start().starts_with("[hooks.state."))
            .any(|l| l.contains(&needle)),
    )
}

#[derive(Debug, serde::Deserialize)]
pub struct DetectQuery {
    /// Absolute project path. Optional — omit for a host-wide probe.
    pub project_dir: Option<String>,
}

pub async fn detect_vendors(
    axum::extract::Query(q): axum::extract::Query<DetectQuery>,
) -> Json<Value> {
    detect_vendors_for(q.project_dir.as_deref()).await
}

/// Same probe, scoped to a project so per-project state (Codex's hook trust)
/// can be reported. `project_dir` must be an absolute path.
pub async fn detect_vendors_for(project_dir: Option<&str>) -> Json<Value> {
    let home = home_dir();
    let probes: Vec<VendorProbe> = VENDORS
        .iter()
        .map(|(id, display, bin, paths)| {
            let installed = is_on_path(bin);
            let config_path = home
                .as_ref()
                .and_then(|h| first_existing_config(h, paths));
            let hooks_trusted = if *id == "codex" {
                home.as_ref()
                    .and_then(|h| codex_hooks_trusted(h, project_dir))
            } else {
                None
            };
            VendorProbe {
                id,
                display,
                bin,
                installed,
                configured: config_path.is_some(),
                config_path,
                hooks_trusted,
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

    /// Codex runs NO hooks until the user accepts its trust prompt, and
    /// records approval in ~/.codex/config.toml. The wizard has to be able to
    /// tell "hooks installed" from "hooks installed and actually running".
    #[test]
    fn codex_hook_trust_is_read_from_config_toml() {
        let tmp = std::env::temp_dir().join(format!("imvend-{}", std::process::id()));
        let codex = tmp.join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        let project = "/Users/u/Development/proj";

        // No config at all → nothing trusted.
        assert_eq!(codex_hooks_trusted(&tmp, Some(project)), None);

        // Config present but this project has never been trusted.
        std::fs::write(
            codex.join("config.toml"),
            "[projects.\"/Users/u/other\"]\ntrust_level = \"trusted\"\n",
        )
        .unwrap();
        assert_eq!(codex_hooks_trusted(&tmp, Some(project)), Some(false));

        // Real shape, transcribed from a live 0.145 config after accepting.
        std::fs::write(
            codex.join("config.toml"),
            format!(
                "[hooks.state]\n\n[hooks.state.\"{project}/.codex/hooks.json:session_start:0:0\"]\n\
                 trusted_hash = \"sha256:912cfdc0\"\n"
            ),
        )
        .unwrap();
        assert_eq!(codex_hooks_trusted(&tmp, Some(project)), Some(true));

        // A different project's trust must not count as this one's.
        assert_eq!(codex_hooks_trusted(&tmp, Some("/Users/u/elsewhere")), Some(false));
        // No project scope → nothing to report.
        assert_eq!(codex_hooks_trusted(&tmp, None), None);

        let _ = std::fs::remove_dir_all(&tmp);
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
