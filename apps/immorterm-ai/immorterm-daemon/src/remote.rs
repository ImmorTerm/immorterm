//! Remote ImmorTerm hosts — login, registry-fetch, on-demand SSH tunnels.
//!
//! The "magic" remote story: a user runs `immorterm-ai remote add hetzner
//! user@my-box`, after which the local Tauri's session picker shows the
//! remote's projects + sessions alongside its own. Clicking a remote
//! session opens an SSH tunnel forwarding the remote's WebSocket port to a
//! free local port; the existing webview WS code (`ws://127.0.0.1:<port>`)
//! works unmodified.
//!
//! Why SSH and not raw exposed ports:
//! - Auth piggybacks on the user's existing SSH keys — no bearer tokens to
//!   manage, no TLS to configure.
//! - The remote daemon stays bound to 127.0.0.1 inside its host — nothing
//!   on the public internet.
//! - Works through every firewall + NAT a founder is likely to hit.
//!
//! On-disk schema lives at `$IMMORTERM_HOME/remotes.json`:
//!
//! ```json
//! { "remotes": [
//!     { "name": "hetzner",
//!       "ssh_target": "user@my-box.hetzner.cloud",
//!       "ssh_port": 22,
//!       "immorterm_home": "~/.immorterm",
//!       "created_at": 1731350000 }
//! ] }
//! ```

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEntry {
    /// User-visible name shown in the picker dropdown, e.g. "hetzner".
    pub name: String,
    /// Argument passed to `ssh` — anything accepted by `ssh USER@HOST` or
    /// a `Host` alias defined in `~/.ssh/config`.
    pub ssh_target: String,
    /// SSH port (default 22). Stored so it's visible in `remote list`
    /// even though `~/.ssh/config` could also set it.
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
    /// `~/.immorterm` on the remote. Defaults to `~/.immorterm` (the
    /// remote daemon's default) but configurable in case the user runs
    /// the daemon under a different home dir on the remote.
    #[serde(default = "default_immorterm_home")]
    pub immorterm_home: String,
    /// Unix timestamp the entry was added. Informational only.
    #[serde(default)]
    pub created_at: i64,
    /// When true, SSH commands use `StrictHostKeyChecking=yes` —
    /// refuses connections to hosts not already in `~/.ssh/known_hosts`.
    /// Default false (= `accept-new` TOFU model). User opts in via
    /// `remote edit NAME --strict-known-hosts on` for paranoid setups.
    #[serde(default)]
    pub strict_known_hosts: bool,
}

fn default_ssh_port() -> u16 { 22 }
fn default_immorterm_home() -> String { "~/.immorterm".to_string() }

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RemotesFile {
    #[serde(default)]
    pub remotes: Vec<RemoteEntry>,
}

/// Path to the remotes registry file.
fn remotes_path() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    home.join(".immorterm").join("remotes.json")
}

pub fn load_remotes() -> Result<RemotesFile> {
    let path = remotes_path();
    if !path.exists() {
        return Ok(RemotesFile::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("parse {}", path.display()))
}

fn save_remotes(f: &RemotesFile) -> Result<()> {
    let path = remotes_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    // Atomic write: write to .tmp then rename, so a crash mid-write
    // doesn't truncate the file. Same pattern the daemon uses for
    // registry.json.
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;
    let body = serde_json::to_string_pretty(f)?;
    file.write_all(body.as_bytes())?;
    file.sync_all().ok();
    fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Add a remote. Refuses to overwrite an existing entry with the same name —
/// use `remove` first if you mean to replace it.
pub fn add(name: &str, ssh_target: &str, ssh_port: u16, immorterm_home: &str) -> Result<RemoteEntry> {
    if name.is_empty() || ssh_target.is_empty() {
        return Err(anyhow!("name and ssh_target are required"));
    }
    if name.chars().any(|c| !c.is_alphanumeric() && c != '-' && c != '_') {
        return Err(anyhow!(
            "name '{name}' may only contain letters, digits, '-', '_'"
        ));
    }
    let mut f = load_remotes()?;
    if f.remotes.iter().any(|r| r.name == name) {
        return Err(anyhow!(
            "remote '{name}' already exists — `immorterm-ai remote remove {name}` first"
        ));
    }
    let entry = RemoteEntry {
        name: name.to_string(),
        ssh_target: ssh_target.to_string(),
        ssh_port,
        immorterm_home: immorterm_home.to_string(),
        created_at: now_unix(),
        strict_known_hosts: false,
    };
    f.remotes.push(entry.clone());
    save_remotes(&f)?;
    Ok(entry)
}

/// Update fields on an existing remote entry. Each `Option` is "leave
/// unchanged" when `None`. Returns the new entry. Renames not allowed —
/// `name` is the lookup key + ties to tabs.json `remote` field, so
/// a rename would invalidate persisted state. Re-`add` under a new name
/// if you need that.
pub fn edit(
    name: &str,
    ssh_target: Option<String>,
    ssh_port: Option<u16>,
    immorterm_home: Option<String>,
    strict_known_hosts: Option<bool>,
) -> Result<RemoteEntry> {
    let mut f = load_remotes()?;
    let entry = f
        .remotes
        .iter_mut()
        .find(|r| r.name == name)
        .ok_or_else(|| anyhow!("no remote named '{name}'"))?;
    if let Some(v) = ssh_target {
        entry.ssh_target = v;
    }
    if let Some(v) = ssh_port {
        entry.ssh_port = v;
    }
    if let Some(v) = immorterm_home {
        entry.immorterm_home = v;
    }
    if let Some(v) = strict_known_hosts {
        entry.strict_known_hosts = v;
    }
    let updated = entry.clone();
    save_remotes(&f)?;
    Ok(updated)
}

pub fn remove(name: &str) -> Result<()> {
    let mut f = load_remotes()?;
    let before = f.remotes.len();
    f.remotes.retain(|r| r.name != name);
    if f.remotes.len() == before {
        return Err(anyhow!("no remote named '{name}'"));
    }
    save_remotes(&f)?;
    Ok(())
}

pub fn get(name: &str) -> Result<Option<RemoteEntry>> {
    let f = load_remotes()?;
    Ok(f.remotes.into_iter().find(|r| r.name == name))
}

/// Run a non-interactive SSH probe — returns Ok(true) if the remote is
/// reachable + auth succeeds, Ok(false) on a clean auth failure, Err on
/// IO/process failure.
///
/// Uses `BatchMode=yes` so SSH never prompts (keys-only). Doesn't enable
/// `StrictHostKeyChecking=no` — host keys must already be in
/// `~/.ssh/known_hosts`, which mirrors the user's existing manual SSH
/// flow. Surfacing host-key prompts in a Tauri webview would be a
/// security gotcha.
/// Write a `.mcp.json` entry that runs `immorterm-ai mcp serve` on the
/// remote over SSH-stdio. Lets a local agent (claude on Mac) call the
/// REMOTE daemon's MCP toolset via tool names like
/// `mcp__immorterm-<name>__read_screen` etc., alongside the local
/// `mcp__immorterm__*` tools. No new transport in the daemon — Claude
/// Code already supports `command + args` MCP server transports.
///
/// Writes to `~/.claude.json` (global Claude config) by default. Can be
/// pointed at a project-scoped `.mcp.json` via the second arg.
pub fn configure_mcp(entry: &RemoteEntry, target_path: Option<&str>) -> Result<PathBuf> {
    let target = match target_path {
        Some(p) => PathBuf::from(shellexpand(p)),
        None => {
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .map_err(|_| anyhow!("HOME not set"))?;
            home.join(".claude.json")
        }
    };

    let mut root: serde_json::Value = if target.exists() {
        let raw = fs::read_to_string(&target)
            .with_context(|| format!("read {}", target.display()))?;
        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // Ensure `mcpServers` exists at root. Both Claude Code (~/.claude.json)
    // and project .mcp.json use this key.
    let servers = root
        .as_object_mut()
        .ok_or_else(|| anyhow!(".claude.json is not a JSON object"))?
        .entry("mcpServers".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| anyhow!("mcpServers must be an object"))?;

    let server_name = format!("immorterm-{}", entry.name);
    let mut args = vec![
        "-p".to_string(),
        entry.ssh_port.to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=30".to_string(),
        entry.ssh_target.clone(),
        "immorterm-ai".to_string(),
        "mcp".to_string(),
        "serve".to_string(),
    ];
    // ssh-stdio: the SSH command itself is the MCP transport. The remote
    // process's stdout = our stdin; remote stdin = our stdout.
    let _ = &mut args;
    servers_obj.insert(
        server_name.clone(),
        serde_json::json!({
            "command": "ssh",
            "args": args,
        }),
    );

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).ok();
    }
    let tmp = target.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&root)?;
    fs::write(&tmp, body)
        .with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;

    Ok(target)
}

/// Remove the mcpServers.immorterm-<name> entry. Returns true if an
/// entry was removed, false if there was nothing to remove.
pub fn unconfigure_mcp(name: &str, target_path: Option<&str>) -> Result<bool> {
    let target = match target_path {
        Some(p) => PathBuf::from(shellexpand(p)),
        None => {
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .map_err(|_| anyhow!("HOME not set"))?;
            home.join(".claude.json")
        }
    };
    if !target.exists() {
        return Ok(false);
    }
    let raw = fs::read_to_string(&target)?;
    let mut root: serde_json::Value = serde_json::from_str(&raw)?;
    let Some(servers) = root
        .as_object_mut()
        .and_then(|o| o.get_mut("mcpServers"))
        .and_then(|s| s.as_object_mut())
    else {
        return Ok(false);
    };
    let key = format!("immorterm-{name}");
    if servers.remove(&key).is_none() {
        return Ok(false);
    }
    let tmp = target.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&root)?;
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &target)?;
    Ok(true)
}

fn shellexpand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    p.to_string()
}

/// Interactive bootstrap: ensure a local SSH keypair exists, install
/// the pubkey on the remote via `ssh-copy-id` (which prompts for
/// password once), then verify keys-only auth works. Caller decides
/// whether to subsequently call `add()` to register the new entry.
///
/// Stdout/stderr stream live to the terminal so the user can interact
/// with ssh's password prompt + host-key-trust dialog. Returns true on
/// success; false if the post-install BatchMode probe fails.
pub fn setup_bootstrap(ssh_target: &str, ssh_port: u16) -> Result<bool> {
    use std::process::Stdio;
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| anyhow!("HOME not set"))?;

    // Step 1: ensure a default ed25519 key exists. ssh-copy-id needs a
    // public key to install; if the user has nothing in ~/.ssh, generate
    // one. Skip silently if any key is already present.
    let ssh_dir = home.join(".ssh");
    let any_pubkey = fs::read_dir(&ssh_dir)
        .ok()
        .map(|rd| rd.flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with(".pub")
        }))
        .unwrap_or(false);
    if !any_pubkey {
        println!("[setup] no SSH key found in ~/.ssh — generating ed25519…");
        let status = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-f", &ssh_dir.join("id_ed25519").to_string_lossy()])
            .status()?;
        if !status.success() {
            return Err(anyhow!("ssh-keygen failed (exit {})", status.code().unwrap_or(-1)));
        }
    }

    // Step 2: ssh-copy-id installs the local pubkey into the remote's
    // ~/.ssh/authorized_keys. Inherits stdin/stdout/stderr so the user
    // sees + answers the password prompt.
    println!("[setup] installing pubkey on {ssh_target} (you'll be prompted for the SSH password ONCE)…");
    let status = Command::new("ssh-copy-id")
        .args([
            "-p", &ssh_port.to_string(),
            ssh_target,
        ])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        return Err(anyhow!(
            "ssh-copy-id failed (exit {}). Common causes: wrong password, sshd disallows password auth, host key mismatch.",
            status.code().unwrap_or(-1)
        ));
    }

    // Step 3: verify keys-only auth works now.
    let probe = Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=accept-new",
            "-p", &ssh_port.to_string(),
            ssh_target,
            "true",
        ])
        .output()?;
    Ok(probe.status.success())
}

pub fn test(entry: &RemoteEntry) -> Result<bool> {
    let strict = if entry.strict_known_hosts {
        "StrictHostKeyChecking=yes"
    } else {
        // accept-new: add unknown host keys on first contact (matches the
        // user's mental model of `immorterm-ai remote add` as a setup
        // step), but reject mismatches afterward — preserves MITM
        // protection once the remote is registered.
        "StrictHostKeyChecking=accept-new"
    };
    let out = Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", strict,
            "-p", &entry.ssh_port.to_string(),
            &entry.ssh_target,
            "true",
        ])
        .output()
        .context("spawn ssh — is the ssh CLI installed?")?;
    Ok(out.status.success())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
