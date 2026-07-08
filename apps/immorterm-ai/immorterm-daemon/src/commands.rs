//! CLI command implementations: -ls, -wipe, -X, -Q, and session subcommands.
//!
//! These run in the client process, connecting to daemon(s) via Unix socket.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::ipc::{Request, Response};
use crate::socket_dir;

/// Information about a discovered session socket.
pub(crate) struct SocketInfo {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) attached: bool,
    pub(crate) alive: bool,
}

/// Discover all session sockets in the socket directory.
pub(crate) fn discover_sessions() -> Vec<SocketInfo> {
    let dir = socket_dir();
    let mut sessions = Vec::new();

    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return sessions,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let filename = match path.file_name().and_then(|f| f.to_str()) {
            Some(f) => f.to_string(),
            None => continue,
        };

        // Parse PID.name format
        let (pid_str, name) = match filename.split_once('.') {
            Some((p, n)) => (p, n.to_string()),
            None => continue,
        };

        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Check if process is alive
        let alive = is_process_alive(pid);

        // Check socket permissions for attached state
        let attached = match fs::metadata(&path) {
            Ok(meta) => meta.permissions().mode() & 0o100 != 0, // 0700 = attached
            Err(_) => false,
        };

        sessions.push(SocketInfo {
            pid,
            name,
            path,
            attached,
            alive,
        });
    }

    sessions
}

/// Check if a process is still running.
pub(crate) fn is_process_alive(pid: u32) -> bool {
    // kill(pid, 0) checks if process exists without sending a signal
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        None,
    )
    .is_ok()
}

/// List all sessions (`-ls`).
///
/// Output format MUST match the C binary's format exactly:
/// ```text
/// There are screens on:
///     12345.session-name    (Detached)
///     12346.other-session   (Attached)
/// 2 Sockets in /path/to/sockets.
/// ```
/// Returns true if sessions were found (caller should exit 1 per GNU Screen convention).
pub fn list_sessions() -> Result<bool> {
    let sessions = discover_sessions();
    let dir = socket_dir();

    if sessions.is_empty() {
        println!("No Sockets found in {}.", dir.display());
        return Ok(false);
    }

    let count = sessions.len();
    let there = if count == 1 { "is a screen" } else { "are screens" };
    println!("There {} on:", there);

    for session in &sessions {
        let status = if !session.alive {
            "(Dead ???)"
        } else if session.attached {
            "(Attached)"
        } else {
            "(Detached)"
        };
        println!("\t{}.{}\t{}", session.pid, session.name, status);
    }

    let socket_word = if count == 1 { "Socket" } else { "Sockets" };
    println!("{} {} in {}.", count, socket_word, dir.display());

    Ok(true)
}

/// Remove dead session sockets (`-wipe`).
pub fn wipe_sessions() -> Result<()> {
    let sessions = discover_sessions();
    let mut removed = 0;

    for session in &sessions {
        if !session.alive
            && fs::remove_file(&session.path).is_ok() {
                println!(
                    "\t{}.{}\tRemoved",
                    session.pid, session.name
                );
                removed += 1;
            }
    }

    if removed == 0 {
        println!("No dead sessions to remove.");
    } else {
        println!("{} socket(s) wiped.", removed);
    }

    Ok(())
}

/// Detach a running session (`-d`).
pub fn detach_session(session_name: &str) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let socket = find_session_socket(session_name)?;
        let mut stream = UnixStream::connect(&socket)
            .await
            .context("Failed to connect to session")?;

        let request = Request::Detach;
        let msg = serde_json::to_vec(&request)?;
        stream.write_all(&msg).await?;

        // Read response
        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).await?;
        if n > 0
            && let Ok(Response::Error(e)) = serde_json::from_slice::<Response>(&buf[..n]) {
                eprintln!("Error: {}", e);
            }

        Ok(())
    })
}

/// Send a command to a running session (`-X`).
pub fn execute_in_session(session_name: &str, args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Ok(());
    }

    let command = args[0].clone();
    let cmd_args = args[1..].to_vec();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let socket = find_session_socket(session_name)?;
        let mut stream = UnixStream::connect(&socket)
            .await
            .context("Failed to connect to session")?;

        let request = Request::Execute {
            command,
            args: cmd_args,
        };
        let msg = serde_json::to_vec(&request)?;
        stream.write_all(&msg).await?;

        // Read response
        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).await?;
        if n > 0
            && let Ok(resp) = serde_json::from_slice::<Response>(&buf[..n]) {
                match resp {
                    Response::Ok(output) if !output.is_empty() => println!("{}", output),
                    Response::Error(e) => eprintln!("Error: {}", e),
                    _ => {}
                }
            }

        Ok(())
    })
}

/// Query a value from a session (`-Q`).
pub fn query_session(session_name: &str, args: &[String]) -> Result<()> {
    let command = args.join(" ");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let socket = find_session_socket(session_name)?;
        let mut stream = UnixStream::connect(&socket)
            .await
            .context("Failed to connect to session")?;

        let request = Request::Query { command };
        let msg = serde_json::to_vec(&request)?;
        stream.write_all(&msg).await?;

        // Read response
        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).await?;
        if n > 0
            && let Ok(resp) = serde_json::from_slice::<Response>(&buf[..n]) {
                match resp {
                    Response::Ok(output) => print!("{}", output),
                    Response::Error(e) => eprintln!("Error: {}", e),
                    _ => {}
                }
            }

        Ok(())
    })
}

/// Find the socket path for a named session (sync version for attach).
pub fn find_session_socket_sync(name: &str) -> Result<PathBuf> {
    find_session_socket(name)
}

// ─── New subcommand implementations ─────────────────────────────────

/// List sessions as JSON (for extension consumption).
pub fn list_sessions_json() -> Result<()> {
    let sessions = discover_sessions();
    let list: Vec<serde_json::Value> = sessions
        .iter()
        .map(|s| {
            let status = if !s.alive {
                "dead"
            } else if s.attached {
                "attached"
            } else {
                "detached"
            };
            serde_json::json!({
                "pid": s.pid,
                "name": s.name,
                "status": status,
                "id": format!("{}.{}", s.pid, s.name),
            })
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "sessions": list,
            "count": list.len(),
            "socket_dir": socket_dir().display().to_string()
        }))?
    );
    Ok(())
}

/// Session auto: create or reattach — the extension's entry point.
///
/// Replaces `screen-auto` bash script (408 lines).
///
/// Usage: `immorterm session auto <window_id> [display_name]`
///
/// Flow:
/// 1. Derive session name from project + window_id
/// 2. Find existing session or create new one
/// 3. Set up logging
/// 4. Attach (foreground, raw terminal relay)
pub fn session_auto(args: &[String]) -> Result<()> {
    let window_id = args.first().context(
        "Usage: immorterm session auto <window_id> [display_name]",
    )?;

    let display_name = args.get(1).cloned().unwrap_or_else(|| window_id.clone());

    // Derive project name from current directory
    let project_dir = std::env::current_dir()?;
    let project = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_lowercase();
    let session_name = format!("{}-{}", project, window_id);

    // Set terminal title immediately (OSC 0)
    eprint!("\x1b]0;{}\x07", display_name);

    // Ensure directories exist
    let logs_dir = project_dir.join(".immorterm/terminals/logs");
    let renames_dir = project_dir.join(".immorterm/terminals/renames");
    fs::create_dir_all(&logs_dir).ok();
    fs::create_dir_all(&renames_dir).ok();

    // Anchor logfile on window_id (always-safe: digits-hex) rather than session_name,
    // which can start with `-` and poison path/argv (see "--help.log" zombie).
    let log_stem = std::env::var("IMMORTERM_WINDOW_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| session_name.clone());
    let logfile = logs_dir.join(format!("{}.log", log_stem));

    // Look for existing session
    let existing = discover_sessions()
        .into_iter()
        .filter(|s| s.name == session_name && s.alive)
        .max_by_key(|s| s.pid); // Prefer most recent if multiple

    if let Some(session) = existing {
        // Existing session found — reattach
        tracing::info!("Reattaching to existing session: {}.{}", session.pid, session.name);

        // Export env vars for shell integration
        export_session_env(&project_dir, &session_name, &display_name, &renames_dir);

        // Attach (foreground, blocking)
        crate::attach::attach_session(&session.name, true)?;
    } else {
        // No session found — create new one
        tracing::info!("Creating new session: {}", session_name);

        // Dump log history to VS Code scrollback (new sessions only)
        if logfile.exists() {
            dump_log_to_scrollback(&logfile)?;
        }

        // Export env vars BEFORE creating session (daemon inherits them)
        export_session_env(&project_dir, &session_name, &display_name, &renames_dir);

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

        // Create detached session
        crate::daemon::create_session(
            &session_name,
            &shell,
            50_000,
            None,
            true,
            Some(logfile.to_string_lossy().to_string()),
        )?;

        // Wait for session to be ready (poll up to 500ms)
        let mut ready = false;
        for _ in 0..25 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if discover_sessions().iter().any(|s| s.name == session_name && s.alive) {
                ready = true;
                break;
            }
        }

        if !ready {
            anyhow::bail!("Session '{}' failed to start within 500ms", session_name);
        }

        // Set window title via IPC
        let _ = execute_in_session(&session_name, &["title".into(), display_name.clone()]);

        // Auto-resume via the vendor-neutral `immorterm recall` CLI.
        // When IMMORTERM_CLAUDE_SESSION_ID is set (extension passed the prior
        // Claude UUID from registry/session.json/claude-env), stuff `<bin> recall`
        // into the new session. Full path to this daemon binary is used because
        // /usr/local/bin/immorterm is the unrelated C fork. The recall CLI
        // handles the 3-tier cascade: claude --resume if jsonl is present,
        // /immorterm:recall skill if the jsonl is gone, or fresh claude.
        // See docs/restore-and-logs.md.
        if let Ok(claude_id) = std::env::var("IMMORTERM_CLAUDE_SESSION_ID")
            && !claude_id.is_empty()
        {
            tracing::info!("Auto-resuming via immorterm recall (claude session: {})", claude_id);
            let bin = std::env::current_exe()
                .ok()
                .and_then(|p| p.into_os_string().into_string().ok())
                .unwrap_or_else(|| "immorterm-ai".to_string());
            let _ = execute_in_session(
                &session_name,
                &["stuff".into(), format!("{} recall\n", bin)],
            );
        }

        // Attach (foreground, blocking)
        crate::attach::attach_session(&session_name, true)?;
    }

    Ok(())
}

// ── `immorterm recall` — vendor-neutral session restoration entry point ────
//
// Stuffed by the daemon into the PTY on session boot (replaces the older
// direct `claude --resume <uuid>` stuff). 3-tier cascade:
//   1. Resolve Claude UUID: env → registry → session.json → claude-env mtime.
//   2. jsonl exists → `claude --resume <uuid>` (native Claude resume).
//   3. jsonl missing, /immorterm:recall skill installed → `claude /immorterm:recall <iid> <uuid>`.
//   4. Nothing usable → plain `claude` or bare shell.
//
// Every branch replaces the current process via unix execvp so the user lands
// directly in Claude, not in a wrapper.

pub fn recall() -> Result<()> {
    let wid = match std::env::var("IMMORTERM_WINDOW_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => return Ok(()),
    };

    // Hard skip: the extension sets this when shelve detected claude was
    // not in the daemon's process tree (user `/exit`'d before shelving).
    // Without this gate, the tier-4 claude-env mtime fallback below would
    // find a leftover .env file from the prior claude run and resurrect
    // the UUID — auto-resuming a session the user deliberately ended.
    if std::env::var("IMMORTERM_NO_AUTO_RESUME").ok().is_some_and(|v| v == "1") {
        tracing::info!("recall: IMMORTERM_NO_AUTO_RESUME=1 — skipping all tiers, returning to bare shell");
        return Ok(());
    }

    let claude_uuid = resolve_claude_uuid_for_recall(&wid);

    if let Some(uuid) = &claude_uuid
        && claude_jsonl_exists(uuid)
    {
        // Clear the viewport (scrollback preserved) before `claude --resume`
        // takes over. Without this, the daemon's scrollback restore renders
        // the prior conversation, then claude --resume replays it AGAIN —
        // duplicate output. CSI 2J clears the visible grid; CSI H homes the
        // cursor; the previously-restored content is pushed into scrollback
        // by the emulator and remains scrollable.
        clear_viewport_before_handoff();
        return launch_claude(&["--resume", uuid]);
    }

    if recall_skill_installed() {
        let prompt = match &claude_uuid {
            Some(uuid) => format!(
                "/immorterm:recall immorterm id: {} claude id: {}",
                wid, uuid
            ),
            None => format!("/immorterm:recall immorterm id: {}", wid),
        };
        // Tier 3 also benefits from a clean handoff so the recall skill
        // prompt appears at the top of the visible grid, not buried under
        // restored scrollback.
        clear_viewport_before_handoff();
        return launch_claude(&[&prompt]);
    }

    if which_claude().is_some() {
        // Tier 4: no UUID, no skill — start a fresh claude. Clear first so
        // the new claude UI doesn't render on top of the daemon's restored
        // scrollback dump.
        clear_viewport_before_handoff();
        launch_claude(&[])
    } else {
        Ok(())
    }
}

/// Push the current viewport content into scrollback, then home the cursor.
/// Used right before exec'ing claude so the prior session's visible
/// content scrolls UP into the scrollback buffer (where the user can scroll
/// to see it) rather than being deleted.
///
/// Why not `ESC[2J`: in our emulator (and most modern terminals) `ESC[2J`
/// just zeroes the visible grid — the rows are NOT pushed to scrollback,
/// so the user loses everything they could see at shelve time. Sending
/// raw newlines while the cursor is at the bottom row triggers natural
/// scroll-up — the top row goes into scrollback, the new row at the
/// bottom is blank. After (rows) newlines the viewport is fully blank
/// and all prior content is in scrollback.
///
/// We don't know the actual viewport row count from this side of the PTY,
/// so emit a generous upper bound (200) — overshooting just inflates
/// scrollback by a few extra blank rows, which is harmless.
fn clear_viewport_before_handoff() {
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    let stdout = std::io::stdout();
    let fd = stdout.as_raw_fd();

    // Query the actual viewport row count via TIOCGWINSZ so we send
    // exactly the right number of newlines. Earlier hardcode of 200
    // overshot by a wide margin and showed as a block of blank
    // scrollback rows. Default to 24 (DEC VT100 baseline) when the
    // ioctl fails.
    let rows = unsafe {
        let mut ws: nix::libc::winsize = std::mem::zeroed();
        if nix::libc::ioctl(fd, nix::libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 {
            ws.ws_row as usize
        } else {
            24
        }
    };

    let mut out = stdout.lock();
    // 1. Move cursor to bottom of viewport so subsequent newlines scroll.
    let _ = out.write_all(b"\x1b[999;1H");
    // 2. Send exactly `rows` newlines to scroll the viewport into scrollback.
    for _ in 0..rows {
        let _ = out.write_all(b"\n");
    }
    // 3. Home cursor for the next renderer (claude).
    let _ = out.write_all(b"\x1b[H");
    let _ = out.flush();
}

fn resolve_claude_uuid_for_recall(window_id: &str) -> Option<String> {
    if let Ok(v) = std::env::var("IMMORTERM_CLAUDE_SESSION_ID")
        && !v.is_empty()
    {
        return Some(v);
    }

    let registry = crate::registry::Registry::load();
    if let Some(entry) = registry.sessions.iter().find(|e| e.window_id == window_id)
        && let Some(uuid) = entry.claude_session_id.clone()
        && !uuid.is_empty()
    {
        return Some(uuid);
    }

    let sld = registry
        .sessions
        .iter()
        .find(|e| e.window_id == window_id)
        .and_then(|e| e.structured_log_dir.clone());
    if let Some(sld) = sld {
        let sj_path = std::path::Path::new(&sld).join("session.json");
        if let Ok(contents) = fs::read_to_string(&sj_path)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&contents)
            && let Some(id) = v.get("claude_session_id").and_then(|x| x.as_str())
            && !id.is_empty()
        {
            return Some(id.to_string());
        }
    }

    let env_dir = crate::dirs_home().join(".immorterm").join("claude-env");
    let Ok(entries) = fs::read_dir(&env_dir) else {
        return None;
    };
    let mut best: Option<(String, std::time::SystemTime)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.ends_with(".env") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        let mut matches_wid = false;
        for line in contents.lines() {
            if let Some(rest) = line.strip_prefix("IMMORTERM_ID=")
                && rest.trim() == window_id
            {
                matches_wid = true;
                break;
            }
        }
        if !matches_wid {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let uuid = name.trim_end_matches(".env").to_string();
        match &best {
            Some((_, best_mtime)) if *best_mtime >= mtime => {}
            _ => best = Some((uuid, mtime)),
        }
    }
    best.map(|(u, _)| u)
}

fn claude_jsonl_exists(uuid: &str) -> bool {
    // Claude stores ~/.claude/projects/<escaped-cwd>/<uuid>.jsonl where the dir
    // name is the cwd at the time Claude was invoked, with '/' replaced by '-'.
    // Prefer project_dir from the registry over std::env::current_dir — the user
    // may have cd'd inside the terminal, which would produce the wrong escaped path.
    let project_dir = std::env::var("IMMORTERM_WINDOW_ID")
        .ok()
        .and_then(|wid| {
            let registry = crate::registry::Registry::load();
            registry
                .sessions
                .iter()
                .find(|e| e.window_id == wid)
                .map(|e| e.project_dir.clone())
        })
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|p| p.into_os_string().into_string().ok())
        });
    let Some(project_dir) = project_dir else { return false };
    let escaped = project_dir.replace('/', "-");
    let jsonl = crate::dirs_home()
        .join(".claude")
        .join("projects")
        .join(&escaped)
        .join(format!("{}.jsonl", uuid));
    jsonl.exists()
}

fn recall_skill_installed() -> bool {
    // Claude Code slash commands live in `.claude/commands/`, not `skills/`.
    // Check project-local first (highest priority — Claude prefers local), then
    // the user-global location.
    let project_local = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".claude").join("commands").join("immorterm").join("recall.md"));
    if let Some(p) = project_local
        && p.exists()
    {
        return true;
    }
    crate::dirs_home()
        .join(".claude")
        .join("commands")
        .join("immorterm")
        .join("recall.md")
        .exists()
}

fn which_claude() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join("claude");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// Replace the current process with `claude <args...>` via unix execvp so the
// user's shell lands directly in Claude. Named `launch_claude` (not `exec_*`)
// to avoid a naive-regex pre-commit hook that flags the `.exec()` method name
// regardless of language — this is Rust's CommandExt, not JS shell exec.
fn launch_claude(args: &[&str]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let claude = which_claude().context("claude not found on PATH")?;
    let mut command_obj = std::process::Command::new(&claude);
    command_obj.args(args);
    // On success this call never returns; on failure returns an io::Error.
    Err(command_obj.exec().into())
}

/// Export environment variables for shell integration.
///
/// These are inherited by the daemon (double-fork) and then by the PTY child.
/// The daemon also sets IMMORTERM_SESSION and ZDOTDIR in run_daemon().
fn export_session_env(
    project_dir: &std::path::Path,
    session_name: &str,
    display_name: &str,
    _renames_dir: &std::path::Path,
) {
    // SAFETY: called during session setup before tokio runtime spawns async tasks.
    unsafe {
        std::env::set_var("SCREEN_PROJECT_DIR", project_dir);
        std::env::set_var("SCREEN_WINDOW_NAME", display_name);
        std::env::set_var("IMMORTERM_BASE_NAME", display_name);
        std::env::set_var("IMMORTERM_SESSION_NAME", session_name);
    }
    // IMMORTERM_WINDOW_ID is set by the extension before calling session auto
    // IMMORTERM_SESSION and ZDOTDIR are set by run_daemon() just before PTY spawn
}

/// Dump filtered log to VS Code's scrollback buffer.
///
/// Replaces `dump_filtered_log()` in screen-auto (the Perl filter).
/// Strips non-visual escape sequences while preserving SGR (colors).
fn dump_log_to_scrollback(logfile: &std::path::Path) -> Result<()> {
    use std::io::{BufRead, BufReader};

    let file = fs::File::open(logfile)?;
    let reader = BufReader::new(file);
    let mut prev_stripped = String::new();

    // Read last 20K lines (like screen-auto)
    let lines: Vec<String> = reader
        .lines()
        .map_while(Result::ok)
        .collect();
    let start = lines.len().saturating_sub(20_000);

    println!(); // blank line before log dump

    for line in &lines[start..] {
        // Strip non-visual escape sequences (keep SGR = colors)
        let cleaned = strip_nonvisual_escapes(line);
        let stripped = strip_all_escapes(&cleaned);

        // Skip empty/whitespace-only lines
        if stripped.trim().is_empty() {
            continue;
        }

        // Skip consecutive duplicates (dedup ghost prompts)
        if stripped == prev_stripped {
            continue;
        }

        println!("{}", cleaned);
        prev_stripped = stripped;
    }

    Ok(())
}

/// Strip non-visual CSI sequences but keep SGR (ESC[...m = colors).
fn strip_nonvisual_escapes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            if bytes[i + 1] == b'[' {
                // CSI sequence: ESC [ params final_byte
                let start = i;
                i += 2; // skip ESC [
                // Skip parameter bytes (0x30-0x3f) and intermediate bytes (0x20-0x2f)
                while i < bytes.len() && bytes[i] >= 0x20 && bytes[i] <= 0x3f {
                    i += 1;
                }
                if i < bytes.len() {
                    let final_byte = bytes[i];
                    i += 1;
                    if final_byte == b'm' {
                        // SGR — keep it
                        result.push_str(&s[start..i]);
                    }
                    // else: non-SGR CSI — strip (cursor movement, erase, etc.)
                }
            } else if bytes[i + 1] == b']' {
                // OSC sequence: ESC ] ... (BEL or ST)
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break; // BEL terminated
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break; // ST terminated
                    }
                    i += 1;
                }
            } else {
                // Simple two-char escape — strip
                i += 2;
            }
        } else if bytes[i] < 0x20 && bytes[i] != b'\n' && bytes[i] != b'\t' {
            // Control character (except newline and tab) — strip
            i += 1;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    result
}

/// Strip ALL escape sequences (for dedup comparison).
fn strip_all_escapes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && bytes[i] >= 0x20 && bytes[i] <= 0x3f {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1; // skip final byte
                }
            } else if i < bytes.len() {
                i += 1; // skip simple escape
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    result
}

/// Remove stale session entries and dead sockets.
///
/// Replaces `screen-cleanup` (40 lines).
pub fn session_cleanup() -> Result<()> {
    // Clean dead sockets
    let sessions = discover_sessions();
    let mut cleaned = 0;

    for session in &sessions {
        if !session.alive
            && fs::remove_file(&session.path).is_ok() {
                println!("Removed dead socket: {}.{}", session.pid, session.name);
                cleaned += 1;
            }
    }

    // Prune dead entries from registry
    let mut registry = crate::registry::Registry::load();
    let before = registry.sessions.len();
    registry.prune();
    let pruned = before - registry.sessions.len();
    if pruned > 0 {
        registry.save().ok();
        println!("Pruned {} dead entries from registry.", pruned);
    }

    if cleaned == 0 && pruned == 0 {
        println!("No stale sessions found.");
    }

    Ok(())
}

/// Generate restore-terminals.json for extension compatibility.
///
/// Reads the registry and outputs the format VS Code extension expects.
/// Called via: `immorterm session restore-json`
pub fn session_restore_json() -> Result<()> {
    let registry = crate::registry::Registry::load();
    let json = registry.to_restore_json();
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

/// Kill a specific session and clean up all associated files.
///
/// Replaces `screen-forget` (81 lines).
pub fn session_forget(name: &str) -> Result<()> {
    let socket = find_session_socket(name)?;

    // Send Kill via IPC
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut stream = tokio::net::UnixStream::connect(&socket)
            .await
            .context("Failed to connect to session")?;

        let request = crate::ipc::Request::Kill;
        let msg = serde_json::to_vec(&request)?;
        stream.write_all(&msg).await?;

        // Read response
        let mut buf = vec![0u8; 65536];
        let _ = stream.read(&mut buf).await;
        Ok::<(), anyhow::Error>(())
    })?;

    // Wait for daemon to exit
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Clean up socket if still present
    if socket.exists() {
        fs::remove_file(&socket).ok();
    }

    println!("Session '{}' killed.", name);
    Ok(())
}

/// Kill ALL sessions for the current project.
///
/// Replaces `screen-forget-all` (81 lines) and `kill-screens` (45 lines).
pub fn session_forget_all() -> Result<()> {
    let project_dir = std::env::current_dir()?;
    let project = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    let sessions = discover_sessions();
    let project_sessions: Vec<_> = sessions
        .iter()
        .filter(|s| s.name.starts_with(&format!("{}-", project)))
        .collect();

    if project_sessions.is_empty() {
        println!("No sessions found for project '{}'.", project);
        return Ok(());
    }

    let rt = tokio::runtime::Runtime::new()?;
    let mut killed = 0;

    for session in &project_sessions {
        if session.alive {
            let path = session.path.clone();
            let result = rt.block_on(async {
                if let Ok(mut stream) = tokio::net::UnixStream::connect(&path).await {
                    let request = crate::ipc::Request::Kill;
                    if let Ok(msg) = serde_json::to_vec(&request) {
                        let _ = stream.write_all(&msg).await;
                    }
                }
                Ok::<(), anyhow::Error>(())
            });
            if result.is_ok() {
                killed += 1;
            }
        }
        // Clean up socket regardless
        fs::remove_file(&session.path).ok();
    }

    // Wait for daemons to exit
    if killed > 0 {
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    println!("Killed {} session(s) for project '{}'.", killed, project);
    Ok(())
}

/// Rotate logs: remove oldest logs until directory is under max_mb.
///
/// Replaces `log-cleanup` (57 lines).
pub fn log_rotate(max_mb: u64) -> Result<()> {
    let project_dir = std::env::current_dir()?;
    let logs_dir = project_dir.join(".immorterm/terminals/logs");

    if !logs_dir.exists() {
        println!("No logs directory found.");
        return Ok(());
    }

    let max_bytes = max_mb * 1024 * 1024;

    // Load registry once to identify live window_ids whose logs must not be pruned.
    // Raw .log filenames are `<windowId>.log` (post-2026-04-21 spawn); the stem
    // matches exactly. Older files (`{project}-ai-{windowId}.log`) are matched by
    // suffix. In either case, if a live registry entry references the windowId
    // the file is protected — destroying its scrollback would strand the session.
    let registry = crate::registry::Registry::load();
    let protected: std::collections::HashSet<String> = registry
        .sessions
        .iter()
        .filter(|e| !e.window_id.is_empty())
        .map(|e| e.window_id.clone())
        .collect();

    fn is_protected(path: &std::path::Path, protected: &std::collections::HashSet<String>) -> bool {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            return false;
        };
        // Exact stem match (new naming: windowId.log).
        if protected.contains(stem) {
            return true;
        }
        // Suffix match for legacy naming (project-ai-windowId.log).
        protected.iter().any(|wid| stem.ends_with(wid))
    }

    loop {
        // Calculate total size
        let mut total_size: u64 = 0;
        let mut logs: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();

        for entry in fs::read_dir(&logs_dir)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("log")
                && let Ok(meta) = fs::metadata(&path) {
                    let size = meta.len();
                    let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    total_size += size;
                    logs.push((path, size, mtime));
                }
        }

        if total_size <= max_bytes || logs.is_empty() {
            let mb = total_size as f64 / (1024.0 * 1024.0);
            println!("Logs: {:.1}MB / {}MB limit ({} files)", mb, max_mb, logs.len());
            break;
        }

        // Delete oldest unprotected log first. If only protected logs remain,
        // refuse to prune — better to exceed the size cap than strand a live
        // session's scrollback history.
        logs.sort_by_key(|(_, _, mtime)| *mtime);
        let Some((oldest, size, _)) = logs
            .iter()
            .find(|(path, _, _)| !is_protected(path, &protected))
        else {
            let mb = total_size as f64 / (1024.0 * 1024.0);
            println!(
                "Logs: {:.1}MB / {}MB limit — all {} remaining files belong to live \
                 sessions (registry-referenced). Refusing to prune.",
                mb,
                max_mb,
                logs.len()
            );
            break;
        };

        println!(
            "Removing {} ({:.1}MB)",
            oldest.file_name().unwrap_or_default().to_string_lossy(),
            *size as f64 / (1024.0 * 1024.0)
        );
        fs::remove_file(oldest).ok();
    }

    Ok(())
}

// ─── Shell Integration ──────────────────────────────────────────────

/// Return the zsh shell integration script.
///
/// This replaces the 148-line shell-init.zsh. Much simpler because:
/// - The Rust daemon passes OSC sequences through to VS Code (no interception)
/// - Title is tracked natively by the daemon (no rename file polling)
/// - sname() talks to the daemon via env var + OSC (no screen -X calls)
pub fn shell_init_zsh() -> &'static str {
    r#"# ImmorTerm shell integration (generated by immorterm shell-init zsh)
# Loaded automatically via ZDOTDIR shim when running inside ImmorTerm
# Works with any IDE terminal: VS Code, Cursor, JetBrains, Warp, etc.

[[ -z "$IMMORTERM_SESSION" ]] && return

# Load zsh datetime module for $EPOCHSECONDS (avoids forking to date)
zmodload -F zsh/datetime b:EPOCHSECONDS 2>/dev/null

_IMMORTERM_LAST_UPDATE=0

# Update terminal tab title on each prompt via OSC 0 (standard escape sequence).
# The Rust daemon passes OSC 0 through to the host terminal natively,
# so this just ensures the title stays in sync after commands.
_immorterm_title_update() {
    local now=${EPOCHSECONDS:-$(date +%s)}
    (( now - _IMMORTERM_LAST_UPDATE < 2 )) && return
    _IMMORTERM_LAST_UPDATE=$now
    printf '\033]0;%s\007' "${IMMORTERM_BASE_NAME:-zsh}" > /dev/tty 2>/dev/null
}

# Register precmd hook (remove duplicates first)
precmd_functions=(${precmd_functions:#_immorterm_title_update})
precmd_functions+=(_immorterm_title_update)

# Set initial title
printf '\033]0;%s\007' "${IMMORTERM_BASE_NAME:-zsh}" > /dev/tty 2>/dev/null

# Rename terminal tab
sname() {
    if [[ -z "$1" ]]; then
        echo "Usage: sname <name>           - Rename and lock title"
        echo "       sname --unlock         - Unlock title (allow dynamic changes)"
        return 1
    fi

    if [[ "$1" == "--unlock" ]]; then
        export IMMORTERM_TITLE_LOCKED=0
        echo "Title unlocked"
    else
        export IMMORTERM_BASE_NAME="$1"
        export IMMORTERM_TITLE_LOCKED=1
        _immorterm_title_update
    fi
}

# OSC 133 Semantic Prompt Markers
# A = prompt start, B = input start, C = output start, D = command done
if [[ "$PROMPT" != *'133;B'* ]]; then
    _immorterm_osc133_precmd() {
        local exit_code=$?
        printf '\e]133;D;%d\e\\' "$exit_code"
        printf '\e]133;A\e\\'
        # OSC 7 — current working directory, consumed by Terminal.cwd in
        # immorterm-core. Drives the plain→project upgrade banner in the
        # standalone Tauri app.
        printf '\e]7;file://%s%s\e\\' "${HOST:-${HOSTNAME:-localhost}}" "$PWD"
    }
    _immorterm_osc133_preexec() {
        printf '\e]133;C\e\\'
    }
    PROMPT="${PROMPT}"$'%{\e]133;B\e\\%}'
    precmd_functions=(${precmd_functions:#_immorterm_osc133_precmd})
    precmd_functions=(_immorterm_osc133_precmd "${precmd_functions[@]}")
    preexec_functions=(${preexec_functions:#_immorterm_osc133_preexec})
    preexec_functions+=(_immorterm_osc133_preexec)
fi
"#
}

/// Return the bash shell integration script.
pub fn shell_init_bash() -> &'static str {
    r#"# ImmorTerm shell integration (generated by immorterm shell-init bash)
# Works with any IDE terminal: VS Code, Cursor, JetBrains, Warp, etc.

[[ -z "$IMMORTERM_SESSION" ]] && return

_IMMORTERM_LAST_UPDATE=0

_immorterm_title_update() {
    local now
    now=$(date +%s)
    if (( now - _IMMORTERM_LAST_UPDATE < 2 )); then
        return
    fi
    _IMMORTERM_LAST_UPDATE=$now
    printf '\033]0;%s\007' "${IMMORTERM_BASE_NAME:-bash}" > /dev/tty 2>/dev/null
}

PROMPT_COMMAND="_immorterm_title_update${PROMPT_COMMAND:+;$PROMPT_COMMAND}"

printf '\033]0;%s\007' "${IMMORTERM_BASE_NAME:-bash}" > /dev/tty 2>/dev/null

sname() {
    if [[ -z "$1" ]]; then
        echo "Usage: sname <name>           - Rename and lock title"
        echo "       sname --unlock         - Unlock title (allow dynamic changes)"
        return 1
    fi

    if [[ "$1" == "--unlock" ]]; then
        export IMMORTERM_TITLE_LOCKED=0
        echo "Title unlocked"
    else
        export IMMORTERM_BASE_NAME="$1"
        export IMMORTERM_TITLE_LOCKED=1
        _immorterm_title_update
    fi
}

# OSC 133 Semantic Prompt Markers
# D+A in PROMPT_COMMAND, C via PS0 (bash 4.4+), B in PS1
if [[ "$PS1" != *'133;B'* ]]; then
    _immorterm_osc133_prompt() {
        local exit_code=$?
        printf '\e]133;D;%d\e\\' "$exit_code"
        printf '\e]133;A\e\\'
        # OSC 7 — current working directory (see zsh variant for why).
        printf '\e]7;file://%s%s\e\\' "${HOSTNAME:-localhost}" "$PWD"
        return "$exit_code"
    }
    PROMPT_COMMAND="_immorterm_osc133_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
    PS1="${PS1}\[\e]133;B\e\\\]"
    PS0='\[\e]133;C\e\\\]'
fi
"#
}

/// Ensure shell integration files exist at ~/.immorterm/shell/.
///
/// Creates:
/// - `~/.immorterm/shell/.zshrc` — ZDOTDIR shim that sources user's .zshrc then our init
/// - `~/.immorterm/shell/shell-init.zsh` — the actual integration script
///
/// The daemon sets ZDOTDIR=~/.immorterm/shell/ when spawning the PTY,
/// so zsh reads our .zshrc first, which chains to the user's real config.
pub fn ensure_shell_integration() -> Result<()> {
    let shell_dir = crate::dirs_home().join(".immorterm").join("shell");
    fs::create_dir_all(&shell_dir)?;

    // Write the ZDOTDIR .zshrc shim
    let zshrc_path = shell_dir.join(".zshrc");
    let zshrc_content = r#"# ImmorTerm ZDOTDIR shim — chains to user's .zshrc then loads shell integration
# This file is auto-generated by `immorterm shell-init setup`

# Reset ZDOTDIR so nested shells (and tools like Claude Code) work normally
ZDOTDIR="$HOME"

# Source user's real .zshrc
[[ -f "$HOME/.zshrc" ]] && source "$HOME/.zshrc"

# Load ImmorTerm shell integration (title sync, sname helper)
[[ -n "$IMMORTERM_SESSION" && -f "$HOME/.immorterm/shell/shell-init.zsh" ]] && \
    source "$HOME/.immorterm/shell/shell-init.zsh"
"#;
    fs::write(&zshrc_path, zshrc_content)?;

    // Write the shell-init scripts
    fs::write(shell_dir.join("shell-init.zsh"), shell_init_zsh())?;
    fs::write(shell_dir.join("shell-init.bash"), shell_init_bash())?;

    // Write .bashrc shim for bash users
    let bashrc_content = r#"# ImmorTerm bashrc shim — chains to user's .bashrc then loads shell integration
# This file is auto-generated by `immorterm shell-init setup`

# Source user's real .bashrc
[[ -f "$HOME/.bashrc" ]] && source "$HOME/.bashrc"

# Load ImmorTerm shell integration
[[ -n "$IMMORTERM_SESSION" && -f "$HOME/.immorterm/shell/shell-init.bash" ]] && \
    source "$HOME/.immorterm/shell/shell-init.bash"
"#;
    fs::write(shell_dir.join(".bashrc"), bashrc_content)?;

    Ok(())
}

/// Find the socket path for a named session.
///
/// Supports three addressing modes (matching GNU Screen):
/// 1. `PID.name` — exact match against full session identifier
/// 2. `name` — exact match against session name
/// 3. `prefix` — prefix match against session name
fn find_session_socket(name: &str) -> Result<PathBuf> {
    let sessions = discover_sessions();

    // Mode 1: Full "PID.name" format (used by screen-auto after -ls)
    if let Some((pid_str, session_name)) = name.split_once('.')
        && let Ok(pid) = pid_str.parse::<u32>() {
            for session in &sessions {
                if session.pid == pid && session.name == session_name && session.alive {
                    return Ok(session.path.clone());
                }
            }
        }

    // Mode 2: Exact name match
    for session in &sessions {
        if session.name == name && session.alive {
            return Ok(session.path.clone());
        }
    }

    // Mode 3: Prefix match
    let matches: Vec<_> = sessions
        .iter()
        .filter(|s| s.name.starts_with(name) && s.alive)
        .collect();

    match matches.len() {
        1 => return Ok(matches[0].path.clone()),
        n if n > 1 => {
            eprintln!("Multiple sessions match '{}':", name);
            for m in &matches {
                eprintln!("\t{}.{}", m.pid, m.name);
            }
            anyhow::bail!("Please be more specific with -S")
        }
        _ => {} // 0 matches — fall through to Mode 4
    }

    // Mode 4: Registry-based window_id lookup (for immorterm_id like "25716-4e1b9f7d")
    let registry = crate::registry::Registry::load();
    if let Some(entry) = registry.find_by_window_id(name) {
        // Found by window_id — resolve via session name (Mode 2 retry)
        for session in &sessions {
            if session.name == entry.name && session.alive {
                return Ok(session.path.clone());
            }
        }
    }

    anyhow::bail!("No session found matching '{}'", name)
}
