// Daemon has many WIP features (team window, WebSocket protocol, etc.)
// that define fields/variants/methods for future use. Allow these globally.
#![allow(dead_code)]
#![allow(clippy::type_complexity)]

//! ImmorTerm daemon CLI — drop-in replacement for the C screen fork.
//!
//! The VS Code extension calls this binary with specific flags.
//! Output formats (especially `-ls`) must match exactly.
//!
//! We use manual argument parsing instead of clap because GNU Screen has
//! non-standard conventions (-dmS as combined flag, -ls as single flag,
//! -D -RR, etc.) that don't map cleanly to standard CLI parsers.

mod attach;
pub mod audio;
pub mod browser;
pub mod claude;
pub mod commands;
mod daemon;
pub mod ipc;
mod log_processor;
mod mcp;
mod pty;
mod pty_history;
pub mod remote;
mod screenshot;
mod structured_log;
mod openmemory_push;
pub mod subagent_watcher;
pub mod channel_registry;
pub mod chat_overlay;
pub mod team_watcher;
// Native winit/wgpu window modes. Gated behind the `gui` feature so
// `--no-default-features --features headless` builds (Linux VPS) skip winit entirely.
#[cfg(feature = "gui")]
mod team_window;
mod websocket;
pub mod registry;
#[cfg(feature = "gui")]
pub mod window;

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

/// Socket directory for session communication.
pub fn socket_dir() -> PathBuf {
    let dir = dirs_home().join(".immorterm").join("sockets");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// `immorterm-p` bash wrapper, embedded at compile time. The wrapper drives
/// interactive `claude` (or any user-configured CLI) inside a headless
/// immorterm session and harvests the answer from a temp JSON file the
/// model writes through its Write tool — a subscription-safe replacement
/// for `claude -p`.
///
/// We bake the script into the binary so every install path (extension
/// downloader, npm, brew, manual cargo build) gets the same copy without
/// touching its own install script. Each daemon launch refreshes
/// ~/.immorterm/bin/immorterm-p when the on-disk content differs.
const IMMORTERM_P_SCRIPT: &str = include_str!("../../scripts/immorterm-p.sh");

/// POSIX xclip/wl-paste/xsel replacement script, embedded at compile
/// time. Reads from `~/.immorterm/clipboard/current.png` (which the
/// daemon writes when handling `clipboard_save_image_bytes`). Lets
/// Claude Code's image-paste flow work on headless hosts (Docker
/// container, VPS) where no real OS clipboard exists.
///
/// Installed at `~/.immorterm/bin/{xclip,wl-paste,xsel}`; the daemon
/// prepends `~/.immorterm/bin` to PATH for spawned `claude` only when
/// the host genuinely lacks a clipboard (no DISPLAY, no
/// WAYLAND_DISPLAY, no real xclip on PATH) — so this never overrides
/// a working OS clipboard.
const IMMORTERM_XCLIP_SHIM: &str =
    include_str!("../../scripts/immorterm-xclip-shim.sh");

/// Write the embedded `immorterm-p` to `~/.immorterm/bin/immorterm-p` if it
/// is missing or its content does not match the embedded copy. Idempotent;
/// runs at every daemon startup. Errors are logged but never fatal — the
/// daemon must boot even if the bin dir is unwritable.
fn install_immorterm_p() {
    let bin_dir = dirs_home().join(".immorterm").join("bin");
    if let Err(e) = std::fs::create_dir_all(&bin_dir) {
        tracing::warn!("install_immorterm_p: mkdir {}: {}", bin_dir.display(), e);
        return;
    }
    let target = bin_dir.join("immorterm-p");

    // Skip the write when content already matches — keeps mtime stable so
    // `stat -f %m` watchers (e.g. the brew bottle versioning script) don't
    // tick every reboot.
    let needs_write = match std::fs::read_to_string(&target) {
        Ok(existing) => existing != IMMORTERM_P_SCRIPT,
        Err(_) => true,
    };
    if !needs_write {
        return;
    }

    // Replace via temp file + rename for atomic update (no half-written
    // script visible to a concurrent `immorterm-p` invocation).
    let tmp = bin_dir.join(format!(".immorterm-p.tmp.{}", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, IMMORTERM_P_SCRIPT) {
        tracing::warn!("install_immorterm_p: write {}: {}", tmp.display(), e);
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    if let Err(e) = std::fs::rename(&tmp, &target) {
        tracing::warn!("install_immorterm_p: rename {} → {}: {}", tmp.display(), target.display(), e);
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    tracing::info!("install_immorterm_p: refreshed {}", target.display());
}

/// Install the embedded xclip/wl-paste/xsel shim into
/// `~/.immorterm/bin/{xclip,wl-paste,xsel}`. Same idempotent pattern as
/// `install_immorterm_p`. Per-name files (not symlinks) so a `which`
/// resolution stays self-explanatory.
///
/// The shim itself is harmless when never invoked — it only kicks in
/// when the daemon's launcher prepends `~/.immorterm/bin` to PATH for a
/// `claude` subprocess (see `should_use_clipboard_shim`).
fn install_clipboard_shim() {
    let bin_dir = dirs_home().join(".immorterm").join("bin");
    if let Err(e) = std::fs::create_dir_all(&bin_dir) {
        tracing::warn!("install_clipboard_shim: mkdir {}: {}", bin_dir.display(), e);
        return;
    }
    // Names Claude Code probes for clipboard reads on Linux. xclip is
    // the historical default; wl-paste covers Wayland; xsel covers
    // older setups. All three exec the same shim — the script inspects
    // its argv to handle TARGETS vs read-bytes.
    for name in &["xclip", "wl-paste", "xsel"] {
        let target = bin_dir.join(name);
        let needs_write = match std::fs::read_to_string(&target) {
            Ok(existing) => existing != IMMORTERM_XCLIP_SHIM,
            Err(_) => true,
        };
        if !needs_write {
            continue;
        }
        let tmp = bin_dir.join(format!(".{name}.tmp.{}", std::process::id()));
        if let Err(e) = std::fs::write(&tmp, IMMORTERM_XCLIP_SHIM) {
            tracing::warn!("install_clipboard_shim: write {}: {}", tmp.display(), e);
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &tmp,
                std::fs::Permissions::from_mode(0o755),
            );
        }
        if let Err(e) = std::fs::rename(&tmp, &target) {
            tracing::warn!("install_clipboard_shim: rename {} → {}: {}", tmp.display(), target.display(), e);
            let _ = std::fs::remove_file(&tmp);
            continue;
        }
        tracing::info!("install_clipboard_shim: refreshed {}", target.display());
    }
}

/// Decide whether to inject `~/.immorterm/bin` at the front of PATH for
/// spawned `claude` processes (which activates the xclip shim).
///
/// Activates ONLY when ALL of:
///   - Linux host (Mac uses NSPasteboard; Windows uses its own — leave
///     them alone).
///   - No `DISPLAY` (X11) and no `WAYLAND_DISPLAY` set.
///   - No `xclip`/`wl-paste` in the system PATH.
///
/// Opt in regardless via `IMMORTERM_FORCE_CLIPBOARD_SHIM=1`.
#[allow(dead_code)]
pub(crate) fn should_use_clipboard_shim() -> bool {
    if std::env::var("IMMORTERM_FORCE_CLIPBOARD_SHIM").as_deref() == Ok("1") {
        return true;
    }
    if !cfg!(target_os = "linux") {
        return false;
    }
    let has_display = std::env::var("DISPLAY").is_ok()
        || std::env::var("WAYLAND_DISPLAY").is_ok();
    if has_display {
        return false;
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let p = std::path::Path::new(dir);
        if p.join("xclip").exists() || p.join("wl-paste").exists() {
            return false;
        }
    }
    true
}

/// Parsed CLI arguments.
#[derive(Debug, Default)]
struct CliArgs {
    version: bool,
    list: bool,
    wipe: bool,
    session_name: Option<String>,
    detached: bool,
    force_detach: bool,
    multiattach: bool,
    reattach: u8,
    execute: bool,
    query: bool,
    config: Option<String>,
    scrollback: usize,
    title: Option<String>,
    shell: Option<String>,
    log_enabled: bool,
    logfile: Option<String>,
    /// Remaining args after -X or -Q
    command_args: Vec<String>,
}

fn parse_args() -> Result<CliArgs> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cli = CliArgs {
        scrollback: 50_000,
        ..Default::default()
    };

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        match arg.as_str() {
            "-v" | "--version" => cli.version = true,
            "-ls" | "--ls" | "-list" | "--list" => cli.list = true,
            "-wipe" | "--wipe" => cli.wipe = true,
            "-X" => {
                cli.execute = true;
                // Everything after -X is the command
                cli.command_args = args[i + 1..].to_vec();
                break;
            }
            "-Q" => {
                cli.query = true;
                cli.command_args = args[i + 1..].to_vec();
                break;
            }
            "-S" => {
                i += 1;
                cli.session_name = args.get(i).cloned();
            }
            "-c" => {
                i += 1;
                cli.config = args.get(i).cloned();
            }
            "-h" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    cli.scrollback = val.parse().unwrap_or(50_000);
                }
            }
            "-t" => {
                i += 1;
                cli.title = args.get(i).cloned();
            }
            "-s" => {
                i += 1;
                cli.shell = args.get(i).cloned();
            }
            "-L" => cli.log_enabled = true,
            "-Logfile" | "--Logfile" | "-logfile" | "--logfile" => {
                i += 1;
                cli.logfile = args.get(i).cloned();
            }
            "-U" => {} // UTF-8 always on, ignore
            "--help" => {
                print_help();
                std::process::exit(0);
            }
            _ => {
                // Handle combined flags: -dmS, -D, -RR, -r, -R, -m, -d, etc.
                if let Some(flags) = arg.strip_prefix('-') {
                    let mut chars = flags.chars().peekable();
                    while let Some(ch) = chars.next() {
                        match ch {
                            'd' => cli.detached = true,
                            'D' => {
                                cli.detached = true;
                                cli.force_detach = true;
                            }
                            'm' => cli.multiattach = true,
                            'r' => cli.reattach += 1,
                            'R' => cli.reattach += 1,
                            'x' => cli.reattach += 1,
                            'S' => {
                                // Rest of this arg or next arg is the session name
                                let rest: String = chars.collect();
                                if !rest.is_empty() {
                                    cli.session_name = Some(rest);
                                } else {
                                    i += 1;
                                    cli.session_name = args.get(i).cloned();
                                }
                                break;
                            }
                            'c' => {
                                let rest: String = chars.collect();
                                if !rest.is_empty() {
                                    cli.config = Some(rest);
                                } else {
                                    i += 1;
                                    cli.config = args.get(i).cloned();
                                }
                                break;
                            }
                            'h' => {
                                let rest: String = chars.collect();
                                if !rest.is_empty() {
                                    cli.scrollback = rest.parse().unwrap_or(50_000);
                                } else {
                                    i += 1;
                                    if let Some(val) = args.get(i) {
                                        cli.scrollback = val.parse().unwrap_or(50_000);
                                    }
                                }
                                break;
                            }
                            _ => {} // Ignore unknown flags
                        }
                    }
                }
                // Else: positional args — ignore for now
            }
        }
        i += 1;
    }

    Ok(cli)
}

fn print_help() {
    eprintln!("ImmorTerm 0.1.0 (rust) — persistent terminal sessions");
    eprintln!();
    eprintln!("Usage: immorterm [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -v, --version        Print version");
    eprintln!("  -ls                  List sessions");
    eprintln!("  -wipe                Remove dead sessions");
    eprintln!("  -dmS <name>          Create detached session");
    eprintln!("  -D -RR -S <name>     Force reattach");
    eprintln!("  -S <name> -X <cmd>   Execute command in session");
    eprintln!("  -S <name> -Q <query> Query session");
    eprintln!("  -h <lines>           Scrollback buffer size");
    eprintln!("  -c <file>            Config file");
    eprintln!("  -s <shell>           Shell to run");
    eprintln!("  -L                   Enable logging");
    eprintln!("  -Logfile <file>      Log file path");
}

/// Handle `immorterm session <subcommand>`.
fn handle_session_subcommand(args: &[String]) -> Result<()> {
    // Initialize tracing for session commands
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .try_init()
        .ok();

    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    match sub {
        "auto" => commands::session_auto(&args[1..]),
        "list" => {
            let json_format = args.iter().any(|a| a == "--json" || a == "--format=json");
            if json_format {
                commands::list_sessions_json()
            } else {
                commands::list_sessions().map(|_| ())
            }
        }
        "cleanup" => commands::session_cleanup(),
        "restore-json" => commands::session_restore_json(),
        "forget" => {
            let all = args.iter().any(|a| a == "--all");
            if all {
                commands::session_forget_all()
            } else {
                let name = args.get(1).context("Session name required: immorterm session forget <name>")?;
                commands::session_forget(name)
            }
        }
        _ => {
            eprintln!("Usage: immorterm session <subcommand>");
            eprintln!();
            eprintln!("Subcommands:");
            eprintln!("  auto <window_id> [name]  Create or reattach session (extension entry point)");
            eprintln!("  list [--json]            List sessions (optionally as JSON)");
            eprintln!("  restore-json             Generate restore-terminals.json for extension");
            eprintln!("  cleanup                  Remove stale entries and prune registry");
            eprintln!("  forget <name>            Kill a session and clean up");
            eprintln!("  forget --all             Kill all project sessions");
            Ok(())
        }
    }
}

/// Handle `immorterm log <subcommand>`.
fn handle_log_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    match sub {
        "rotate" => {
            let max_mb: u64 = args.get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(300);
            commands::log_rotate(max_mb)
        }
        _ => {
            eprintln!("Usage: immorterm log <subcommand>");
            eprintln!();
            eprintln!("Subcommands:");
            eprintln!("  rotate [max_mb]    Remove oldest logs to stay under limit (default: 300MB)");
            Ok(())
        }
    }
}

/// Handle `immorterm-ai remote <subcommand>`.
///
/// Magic-UX layer for the Tauri remote picker. `add` writes an entry to
/// `~/.immorterm/remotes.json`; `list` dumps it; `remove` deletes one;
/// `test` runs a non-interactive SSH probe to verify auth works before
/// the user wastes a click expecting a working tunnel.
fn handle_remote_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    match sub {
        "add" => {
            // immorterm-ai remote add NAME SSH_TARGET [--port PORT] [--remote-home PATH]
            let name = args.get(1).context("remote add: NAME is required")?;
            let target = args.get(2).context("remote add: SSH_TARGET (user@host) is required")?;
            let mut port: u16 = 22;
            let mut home = "~/.immorterm".to_string();
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--port" | "-p" => {
                        i += 1;
                        port = args.get(i)
                            .and_then(|s| s.parse().ok())
                            .context("--port wants a u16")?;
                    }
                    "--remote-home" => {
                        i += 1;
                        home = args.get(i)
                            .context("--remote-home wants a path")?
                            .clone();
                    }
                    other => return Err(anyhow::anyhow!("unknown flag: {other}")),
                }
                i += 1;
            }
            let entry = remote::add(name, target, port, &home)?;
            println!("Added remote '{}' → {}:{}", entry.name, entry.ssh_target, entry.ssh_port);
            // Best-effort connectivity probe so the user finds out NOW if
            // their SSH config is wrong, not at first click.
            match remote::test(&entry) {
                Ok(true) => println!("  SSH probe: ok"),
                Ok(false) => eprintln!("  SSH probe: FAILED — check `ssh -o BatchMode=yes {}` works", entry.ssh_target),
                Err(e) => eprintln!("  SSH probe: error: {e}"),
            }
            Ok(())
        }
        "list" => {
            let f = remote::load_remotes()?;
            if f.remotes.is_empty() {
                println!("(no remotes — `immorterm-ai remote add NAME user@host`)");
            } else {
                println!("{:<16} {:<40} PORT  IMMORTERM_HOME", "NAME", "SSH_TARGET");
                for r in &f.remotes {
                    println!("{:<16} {:<40} {:<5} {}", r.name, r.ssh_target, r.ssh_port, r.immorterm_home);
                }
            }
            Ok(())
        }
        "remove" | "rm" => {
            let name = args.get(1).context("remote remove: NAME is required")?;
            remote::remove(name)?;
            println!("Removed remote '{name}'");
            Ok(())
        }
        "edit" => {
            // immorterm-ai remote edit NAME [--ssh-target T] [--port N]
            //   [--remote-home P] [--strict-known-hosts on|off]
            let name = args.get(1).context("remote edit: NAME is required")?;
            let mut ssh_target: Option<String> = None;
            let mut ssh_port: Option<u16> = None;
            let mut home: Option<String> = None;
            let mut strict: Option<bool> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--ssh-target" => {
                        i += 1;
                        ssh_target = args.get(i).cloned();
                    }
                    "--port" | "-p" => {
                        i += 1;
                        ssh_port = args.get(i).and_then(|s| s.parse().ok());
                    }
                    "--remote-home" => {
                        i += 1;
                        home = args.get(i).cloned();
                    }
                    "--strict-known-hosts" => {
                        i += 1;
                        strict = args.get(i).map(|v| matches!(v.as_str(), "on" | "true" | "1" | "yes"));
                    }
                    other => return Err(anyhow::anyhow!("unknown flag: {other}")),
                }
                i += 1;
            }
            let updated = remote::edit(name, ssh_target, ssh_port, home, strict)?;
            println!("Updated remote '{}':", updated.name);
            println!("  ssh_target       = {}", updated.ssh_target);
            println!("  ssh_port         = {}", updated.ssh_port);
            println!("  immorterm_home   = {}", updated.immorterm_home);
            println!("  strict_known_hosts = {}", updated.strict_known_hosts);
            Ok(())
        }
        "test" => {
            let name = args.get(1).context("remote test: NAME is required")?;
            let entry = remote::get(name)?
                .ok_or_else(|| anyhow::anyhow!("no remote named '{name}'"))?;
            match remote::test(&entry)? {
                true => { println!("ok"); Ok(()) }
                false => Err(anyhow::anyhow!("SSH probe failed")),
            }
        }
        "setup" => {
            // immorterm-ai remote setup user@host [--port N]
            //   [--name NAME] [--remote-home PATH]
            //
            // Interactive: prompts for password ONCE, installs SSH key,
            // verifies keys-only auth, then registers the remote.
            let target = args.get(1).context("remote setup: SSH_TARGET (user@host) is required")?;
            let mut port: u16 = 22;
            let mut name: Option<String> = None;
            let mut home = "~/.immorterm".to_string();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--port" | "-p" => {
                        i += 1;
                        port = args.get(i).and_then(|s| s.parse().ok())
                            .context("--port wants a u16")?;
                    }
                    "--name" => {
                        i += 1;
                        name = args.get(i).cloned();
                    }
                    "--remote-home" => {
                        i += 1;
                        home = args.get(i)
                            .context("--remote-home wants a path")?
                            .clone();
                    }
                    other => return Err(anyhow::anyhow!("unknown flag: {other}")),
                }
                i += 1;
            }
            // Derive a reasonable name if user didn't supply one — last
            // host part of user@host, stripped of common TLD bits.
            let derived_name = name.unwrap_or_else(|| {
                let host = target.split('@').next_back().unwrap_or(target);
                let first = host.split('.').next().unwrap_or(host);
                first.chars()
                    .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                    .collect::<String>()
                    .trim_matches('-')
                    .to_string()
            });
            if remote::get(&derived_name)?.is_some() {
                return Err(anyhow::anyhow!(
                    "remote '{derived_name}' already exists — use a different --name or `remote remove {derived_name}` first"
                ));
            }

            let ok = remote::setup_bootstrap(target, port)?;
            if !ok {
                eprintln!("[setup] WARNING: ssh-copy-id ran but BatchMode probe failed. \
                          The remote may not allow keys-only auth, or sshd is misconfigured.");
                return Err(anyhow::anyhow!("setup failed verification"));
            }
            let entry = remote::add(&derived_name, target, port, &home)?;
            println!("[setup] keys-only auth confirmed. Registered remote '{}' → {}:{}",
                     entry.name, entry.ssh_target, entry.ssh_port);
            println!("[setup] Next: open Tauri → Cmd+T → Source: {} → click a project.", entry.name);
            Ok(())
        }
        "configure-mcp" => {
            let name = args.get(1).context("remote configure-mcp: NAME is required")?;
            let entry = remote::get(name)?
                .ok_or_else(|| anyhow::anyhow!("no remote named '{name}'"))?;
            // Optional target path (--target /path/.mcp.json). Default
            // is ~/.claude.json.
            let mut target: Option<&str> = None;
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--target" {
                    i += 1;
                    target = args.get(i).map(|s| s.as_str());
                }
                i += 1;
            }
            let path = remote::configure_mcp(&entry, target)?;
            println!("Added mcpServers.immorterm-{name} to {}", path.display());
            println!("Restart Claude Code to pick up the new MCP server.");
            println!("Tool prefix: mcp__immorterm-{name}__<tool>");
            Ok(())
        }
        "unconfigure-mcp" => {
            let name = args.get(1).context("remote unconfigure-mcp: NAME is required")?;
            let mut target: Option<&str> = None;
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--target" {
                    i += 1;
                    target = args.get(i).map(|s| s.as_str());
                }
                i += 1;
            }
            if remote::unconfigure_mcp(name, target)? {
                println!("Removed mcpServers.immorterm-{name}");
            } else {
                println!("No entry mcpServers.immorterm-{name} found (nothing to remove)");
            }
            Ok(())
        }
        _ => {
            eprintln!("Usage: immorterm-ai remote <subcommand>");
            eprintln!();
            eprintln!("Subcommands:");
            eprintln!("  add NAME user@host [--port N] [--remote-home PATH]");
            eprintln!("                       Register a remote ImmorTerm host");
            eprintln!("  list                 Show registered remotes");
            eprintln!("  remove NAME          Forget a remote");
            eprintln!("  edit NAME [--ssh-target T] [--port N] [--remote-home P]");
            eprintln!("           [--strict-known-hosts on|off]");
            eprintln!("                       Mutate fields on an existing remote.");
            eprintln!("  test NAME            Probe SSH auth (non-interactive)");
            eprintln!("  setup user@host [--port N] [--name NAME] [--remote-home PATH]");
            eprintln!("                       Interactive: ssh-copy-id installs your pubkey on");
            eprintln!("                       the remote (prompts for password ONCE), then");
            eprintln!("                       verifies keys-only auth + registers the remote.");
            eprintln!("  configure-mcp NAME [--target PATH]");
            eprintln!("                       Add mcpServers.immorterm-NAME entry to");
            eprintln!("                       ~/.claude.json (or --target file). Lets");
            eprintln!("                       agents call the REMOTE daemon's MCP via");
            eprintln!("                       SSH-stdio. Tool prefix: mcp__immorterm-NAME__*");
            eprintln!("  unconfigure-mcp NAME [--target PATH]");
            eprintln!("                       Remove the mcpServers.immorterm-NAME entry.");
            Ok(())
        }
    }
}

/// Handle `immorterm shell-init <shell>`.
///
/// Outputs shell integration script to stdout.
/// Usage: `eval "$(immorterm shell-init zsh)"` or `immorterm shell-init setup`
fn handle_shell_init_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("zsh");
    match sub {
        "zsh" => {
            print!("{}", commands::shell_init_zsh());
            Ok(())
        }
        "bash" => {
            print!("{}", commands::shell_init_bash());
            Ok(())
        }
        "setup" => {
            commands::ensure_shell_integration()?;
            eprintln!("Shell integration files written to ~/.immorterm/shell/");
            Ok(())
        }
        _ => {
            eprintln!("Usage: immorterm shell-init <shell>");
            eprintln!();
            eprintln!("Shells:");
            eprintln!("  zsh      Output zsh integration script");
            eprintln!("  bash     Output bash integration script");
            eprintln!("  setup    Write ZDOTDIR shim files to ~/.immorterm/shell/");
            Ok(())
        }
    }
}

/// Handle `immorterm mcp <subcommand>`.
fn handle_mcp_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    match sub {
        "serve" => mcp::serve_stdio(),
        _ => {
            eprintln!("Usage: immorterm mcp serve");
            eprintln!();
            eprintln!("Subcommands:");
            eprintln!("  serve    Start MCP server on stdio (JSON-RPC 2.0)");
            Ok(())
        }
    }
}

/// Handle `immorterm claude-push`.
///
/// Reads Claude Code's statusLine JSON from stdin and pushes it directly
/// to the daemon via IPC. Called by immorterm-statusline.sh.
///
/// This is the event-driven path — the daemon gets Claude session data
/// immediately instead of polling /tmp context files every 10 seconds.
fn handle_claude_push() -> Result<()> {
    use std::io::Read;

    // Read JSON from stdin (Claude Code pipes statusLine data here)
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let input = input.trim();
    if input.is_empty() {
        return Ok(());
    }

    // Parse the Claude Code statusLine JSON
    let json: serde_json::Value = serde_json::from_str(input)
        .context("Invalid JSON from Claude Code statusLine")?;

    let session_id = json["session_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Missing session_id in statusLine data")?
        .to_string();

    let model = json["model"]["display_name"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let cost_usd = json["cost"]["total_cost_usd"]
        .as_f64()
        .unwrap_or(0.0);

    let context_pct = json["context_window"]["used_percentage"]
        .as_f64()
        .unwrap_or(0.0);

    let transcript_path = json["transcript_path"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let permission_mode = json["permission_mode"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Find the daemon session name from env (set by daemon at PTY spawn)
    let session_name = std::env::var("IMMORTERM_SESSION")
        .or_else(|_| {
            // Fallback: parse from STY (legacy screen format: "PID.sessionname")
            std::env::var("STY").map(|sty| {
                sty.split_once('.')
                    .map(|(_, name)| name.to_string())
                    .unwrap_or(sty)
            })
        })
        .context("Cannot find daemon session — IMMORTERM_SESSION not set")?;

    // Send IPC request to the daemon
    let request = ipc::Request::UpdateClaudeSession {
        session_id,
        model,
        cost_usd,
        context_pct,
        transcript_path,
        permission_mode,
    };

    // Find the daemon's socket
    let socket_path = commands::find_session_socket_sync(&session_name)?;

    // Connect and send
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::UnixStream::connect(&socket_path)
            .await
            .context("Failed to connect to daemon")?;

        let msg = serde_json::to_vec(&request)?;
        stream.write_all(&msg).await?;

        // Read response (don't block long — this runs in the statusline script)
        let mut buf = vec![0u8; 1024];
        let _ = tokio::time::timeout(
            tokio::time::Duration::from_millis(500),
            stream.read(&mut buf),
        )
        .await;

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// Handle `immorterm permission-push <mode>`.
///
/// Pushes a permission mode change to the daemon via IPC.
/// Called by a Claude Code hook when permission_mode changes.
fn handle_permission_push(args: &[String]) -> Result<()> {
    let mode = args.first()
        .context("Permission mode required: immorterm permission-push <mode>")?
        .to_string();

    let session_name = std::env::var("IMMORTERM_SESSION")
        .or_else(|_| {
            std::env::var("STY").map(|sty| {
                sty.split_once('.')
                    .map(|(_, name)| name.to_string())
                    .unwrap_or(sty)
            })
        })
        .context("Cannot find daemon session — IMMORTERM_SESSION not set")?;

    let request = ipc::Request::UpdatePermissionMode { mode };
    let socket_path = commands::find_session_socket_sync(&session_name)?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::UnixStream::connect(&socket_path)
            .await
            .context("Failed to connect to daemon")?;

        let msg = serde_json::to_vec(&request)?;
        stream.write_all(&msg).await?;

        let mut buf = vec![0u8; 1024];
        let _ = tokio::time::timeout(
            tokio::time::Duration::from_millis(500),
            stream.read(&mut buf),
        )
        .await;

        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// Handle `immorterm screenshot <session> [--output file.png]`.
///
/// Fetches terminal state from daemon via `DumpState`, then renders the
/// screenshot locally using this process's GPU access (Metal/Vulkan).
/// The daemon can't render because double-forked processes lose WindowServer.
fn handle_screenshot_cmd(args: &[String]) -> Result<()> {
    let mut session_name: Option<&str> = None;
    let mut output_path: Option<&str> = None;
    let mut no_status_bar = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--output" | "-o" => {
                i += 1;
                output_path = args.get(i).map(|s| s.as_str());
            }
            "--no-status-bar" => {
                no_status_bar = true;
            }
            name => {
                if session_name.is_none() {
                    session_name = Some(name);
                }
            }
        }
        i += 1;
    }

    let session = session_name.context(
        "Session name required: immorterm screenshot <session> [--output file.png]",
    )?;

    // Step 1: Fetch terminal state from daemon via IPC
    let rt = tokio::runtime::Runtime::new()?;
    let (snapshot_json, session_name_str, sb_project, sb_ai_stats) = rt.block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let socket = commands::find_session_socket_sync(session)?;
        let mut stream = tokio::net::UnixStream::connect(&socket)
            .await
            .context("Failed to connect to daemon")?;

        let request = ipc::Request::DumpState;
        let msg = serde_json::to_vec(&request)?;
        stream.write_all(&msg).await?;
        stream.shutdown().await?; // Signal we're done writing

        // Read full response — terminal state can be large
        let mut buf = Vec::new();
        tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            stream.read_to_end(&mut buf),
        )
        .await
        .context("Timeout waiting for terminal state")??;

        anyhow::ensure!(!buf.is_empty(), "No response from daemon");

        let resp: ipc::Response = serde_json::from_slice(&buf)?;

        match resp {
            ipc::Response::TerminalState {
                snapshot_json,
                session_name,
                status_bar_project,
                status_bar_ai_stats,
            } => Ok((snapshot_json, session_name, status_bar_project, status_bar_ai_stats)),
            ipc::Response::Error(e) => {
                anyhow::bail!("DumpState failed: {}", e);
            }
            _ => {
                anyhow::bail!("Unexpected response type");
            }
        }
    })?;

    // Step 2: Deserialize terminal state
    let snapshot: immorterm_core::TerminalSnapshot = serde_json::from_str(&snapshot_json)
        .context("Failed to deserialize terminal snapshot")?;
    let mut terminal = immorterm_core::Terminal::from_snapshot(snapshot);

    eprintln!(
        "Got terminal state: {}x{} (session: {})",
        terminal.cols(),
        terminal.rows(),
        session_name_str,
    );

    // Step 3: Render screenshot locally (this process has GPU access)
    let sb_ctx = if !no_status_bar {
        Some(screenshot::StatusBarContext {
            project: sb_project,
            ai_stats: sb_ai_stats,
        })
    } else {
        None
    };

    let (png_base64, width, height) = screenshot::render_screenshot(
        &mut terminal,
        !no_status_bar,
        sb_ctx.as_ref(),
        None,
        None,
    )
    .map_err(|e| anyhow::anyhow!("Screenshot render failed: {}", e))?;

    // Step 4: Output
    if let Some(path) = output_path {
        use base64::Engine;
        let png_bytes = base64::engine::general_purpose::STANDARD.decode(&png_base64)?;
        std::fs::write(path, &png_bytes)?;
        eprintln!(
            "Screenshot saved: {} ({}x{}, {} bytes)",
            path, width, height, png_bytes.len()
        );
    } else {
        print!("{}", png_base64);
    }

    Ok(())
}

/// Handle `immorterm-ai wait-event <session> [--type click|hover] [--id N] [--name NAME] [--timeout MS]`.
///
/// Blocks until a matching AI canvas event occurs, then prints JSON to stdout.
/// Designed to be used as a background bash task by Claude Code.
fn handle_wait_event_cmd(args: &[String]) -> Result<()> {
    let mut session_name: Option<&str> = None;
    let mut event_type: Option<String> = None;
    let mut primitive_id: Option<u32> = None;
    let mut name: Option<String> = None;
    let mut timeout_ms: u64 = 30_000;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--type" | "-t" => {
                i += 1;
                event_type = args.get(i).map(|s| s.to_string());
            }
            "--id" => {
                i += 1;
                primitive_id = args.get(i).and_then(|s| s.parse().ok());
            }
            "--name" | "-n" => {
                i += 1;
                name = args.get(i).map(|s| s.to_string());
            }
            "--timeout" => {
                i += 1;
                if let Some(ms) = args.get(i).and_then(|s| s.parse().ok()) {
                    // Cap raised from 300_000 (5 min) → 86_400_000 (24 h) so
                    // background-bash wait loops can sit on a workshop for a
                    // full work day without forced re-arm. Still capped to
                    // guard against accidental u64::MAX values that would
                    // overflow the daemon's tokio::time::Instant arithmetic.
                    timeout_ms = std::cmp::min(ms, 86_400_000);
                }
            }
            arg => {
                if session_name.is_none() {
                    session_name = Some(arg);
                }
            }
        }
        i += 1;
    }

    let session = session_name.context(
        "Session name required: immorterm wait-event <session> [--type click] [--id N] [--name NAME] [--timeout MS]",
    )?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let socket = commands::find_session_socket_sync(session)?;
        let mut stream = tokio::net::UnixStream::connect(&socket)
            .await
            .context("Failed to connect to daemon")?;

        let request = ipc::Request::WaitForAiEvent {
            event_type,
            primitive_id,
            name,
            timeout_ms,
        };
        let msg = serde_json::to_vec(&request)?;
        stream.write_all(&msg).await?;

        let mut buf = vec![0u8; 65536];
        let n = tokio::time::timeout(
            tokio::time::Duration::from_millis(timeout_ms + 2000),
            stream.read(&mut buf),
        )
        .await
        .context("Client-side timeout waiting for AI event")??;

        anyhow::ensure!(n > 0, "No response from daemon");

        let resp: ipc::Response = serde_json::from_slice(&buf[..n])?;

        match resp {
            ipc::Response::AiEventOccurred { event } => {
                let event_json = match &event {
                    immorterm_core::ai_layer::AiEvent::ButtonClicked { id, data_click } => {
                        let mut obj = serde_json::json!({"type": "button_clicked", "id": id});
                        if let Some(dc) = data_click {
                            obj["data_click"] = serde_json::json!(dc);
                        }
                        obj
                    }
                    immorterm_core::ai_layer::AiEvent::ButtonHovered { id, entered } => {
                        serde_json::json!({"type": "button_hovered", "id": id, "entered": entered})
                    }
                    immorterm_core::ai_layer::AiEvent::WorkshopClicked { name, data_click } => {
                        let mut obj = serde_json::json!({"type": "workshop_clicked", "name": name});
                        if let Some(dc) = data_click {
                            obj["data_click"] = serde_json::json!(dc);
                        }
                        obj
                    }
                };
                println!("{}", serde_json::to_string(&event_json)?);
                Ok(())
            }
            ipc::Response::Error(e) => {
                anyhow::bail!("{}", e);
            }
            _ => {
                anyhow::bail!("Unexpected response type");
            }
        }
    })
}

/// Handle `immorterm-ai log-process <log-file>`.
///
/// Runs the C binary sidecar: tails a raw `.log` file, processes it through
/// the VTE terminal emulator, and produces `.grid.jsonl` + `.cast` structured
/// log files in the same directory.
fn handle_log_process_cmd(args: &[String]) -> Result<()> {
    // Initialize tracing for the sidecar
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .try_init()
        .ok();

    let log_path = args
        .first()
        .context("Log file path required: immorterm-ai log-process <log-file>")?;

    let path = std::path::Path::new(log_path);
    anyhow::ensure!(path.exists(), "Log file does not exist: {}", log_path);

    log_processor::run_log_processor(path)
}

/// Handle `immorterm-ai restore-dump <grid-log-path>`.
///
/// Reads a `.grid.jsonl` file, finds the last snapshot and scrollback dump,
/// converts them to ANSI escape sequences, and writes the result to stdout.
/// Used for session restoration in the `screen-auto` script.
fn handle_restore_dump_cmd(args: &[String]) -> Result<()> {
    use immorterm_core::log::{GridSnapshot, ScrollbackDump, snapshot_to_ansi};
    use std::io::{BufRead, BufReader, Write};

    let grid_log_path = args
        .first()
        .context("Grid log path required: immorterm-ai restore-dump <grid-log-path>")?;

    let file = std::fs::File::open(grid_log_path)
        .with_context(|| format!("Cannot open grid log: {}", grid_log_path))?;

    let reader = BufReader::new(file);

    let mut last_snapshot: Option<GridSnapshot> = None;
    let mut last_scrollback: Option<ScrollbackDump> = None;

    // Scan all lines, keeping track of the last snapshot and scrollback entries
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Peek at the "type" field to decide how to deserialize
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            match value.get("type").and_then(|t| t.as_str()) {
                Some("snapshot") => {
                    if let Ok(snap) = serde_json::from_str::<GridSnapshot>(line) {
                        last_snapshot = Some(snap);
                    }
                }
                Some("scrollback") => {
                    if let Ok(sb) = serde_json::from_str::<ScrollbackDump>(line) {
                        last_scrollback = Some(sb);
                    }
                }
                _ => {}
            }
        }
    }

    // Convert to ANSI and output
    if let Some(snapshot) = last_snapshot {
        let ansi = snapshot_to_ansi(&snapshot, last_scrollback.as_ref());
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(ansi.as_bytes())?;
        out.flush()?;
    } else {
        // No snapshot found — output nothing (caller will fall back to dump_filtered_log)
        eprintln!("No snapshot found in {}", grid_log_path);
    }

    Ok(())
}

fn main() -> Result<()> {
    // Refresh the embedded immorterm-p wrapper on every launch. Cheap (one
    // file read + maybe one write); fire-and-forget — failures don't block
    // the daemon. See install_immorterm_p() docs above.
    install_immorterm_p();
    install_clipboard_shim();

    // Check for subcommands first (new features, not Screen compat)
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.len() >= 2 {
        match raw_args[1].as_str() {
            "mcp" => return handle_mcp_subcommand(&raw_args[2..]),
            "session" => return handle_session_subcommand(&raw_args[2..]),
            "log" => return handle_log_subcommand(&raw_args[2..]),
            "shell-init" => return handle_shell_init_subcommand(&raw_args[2..]),
            "remote" => return handle_remote_subcommand(&raw_args[2..]),
            "recall" => return commands::recall(),
            "claude-push" => return handle_claude_push(),
            "permission-push" => return handle_permission_push(&raw_args[2..]),
            "screenshot" => return handle_screenshot_cmd(&raw_args[2..]),
            "wait-event" => return handle_wait_event_cmd(&raw_args[2..]),
            "log-process" => return handle_log_process_cmd(&raw_args[2..]),
            "restore-dump" => return handle_restore_dump_cmd(&raw_args[2..]),
            #[cfg(feature = "gui")]
            "team-view" => {
                // immorterm-ai team-view [team-name]
                let team_name = raw_args.get(2).map(|s| s.as_str());
                return team_window::main_team_view(team_name);
            }
            #[cfg(feature = "gui")]
            "gui" => {
                let mut session_name = None;
                let mut shell = None;
                let mut i = 2;
                while i < raw_args.len() {
                    match raw_args[i].as_str() {
                        "--shell" | "-s" => {
                            i += 1;
                            shell = raw_args.get(i).map(|s| s.as_str());
                        }
                        name => {
                            session_name = Some(name);
                        }
                    }
                    i += 1;
                }
                let shell = shell.unwrap_or_else(|| {
                    Box::leak(default_shell().into_boxed_str())
                });
                return window::main_gui(session_name, shell);
            }
            #[cfg(not(feature = "gui"))]
            "gui" | "team-view" => {
                anyhow::bail!(
                    "`{}` subcommand requires the `gui` feature; this binary was built with `--no-default-features --features headless`",
                    raw_args[1]
                );
            }
            _ => {}
        }
    }

    // Initialize tracing (respects RUST_LOG env var).
    // Use try_init() because the daemon code path sets its own file-based subscriber
    // after double-fork, and the global slot may already be occupied.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .try_init()
        .ok();

    let cli = parse_args()?;

    // -v / --version
    if cli.version {
        println!("ImmorTerm 0.1.0 (rust)");
        return Ok(());
    }

    // -ls (GNU Screen returns exit 1 when sessions exist, 0 when none)
    if cli.list {
        let has_sessions = commands::list_sessions()?;
        if has_sessions {
            std::process::exit(1);
        }
        return Ok(());
    }

    // -wipe
    if cli.wipe {
        return commands::wipe_sessions();
    }

    // -X (execute command in session) or -Q (query)
    if cli.execute || cli.query {
        let name = cli
            .session_name
            .as_ref()
            .context("Session name (-S) required for -X/-Q commands")?;

        if cli.query {
            return commands::query_session(name, &cli.command_args);
        } else {
            return commands::execute_in_session(name, &cli.command_args);
        }
    }

    // -S <name> -d (without -m) — detach existing session
    if cli.detached && !cli.multiattach && cli.reattach == 0
        && !cli.execute && !cli.query
        && let Some(name) = &cli.session_name {
            return commands::detach_session(name);
        }

    // -dmS <name> — create detached session (requires -m flag)
    if cli.detached && cli.multiattach && cli.session_name.is_some() && cli.reattach == 0 {
        let name = cli
            .session_name
            .as_ref()
            .context("Session name (-S) required for -dmS")?;
        let shell = cli.shell.unwrap_or_else(default_shell);

        return daemon::create_session(
            name,
            &shell,
            cli.scrollback,
            cli.config,
            cli.log_enabled,
            cli.logfile,
        );
    }

    // -D -RR -S <name> / -r -S <name> — reattach
    if cli.reattach > 0 {
        let name = cli
            .session_name
            .as_ref()
            .context("Session name (-S) required for reattach")?;
        return attach::attach_session(name, cli.force_detach);
    }

    // No valid command — print help
    print_help();
    Ok(())
}

/// Get the user's default shell.
fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
}
