//! Daemon process — double-fork, PTY spawn, Unix socket server.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use nix::sys::signal;
use nix::unistd::{self, ForkResult, Pid};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::ipc::{self, Request, Response};
use crate::pty::PtySession;
use crate::socket_dir;
use crate::team_watcher;

/// Shared PTY writer handle. The terminal-side writer is wrapped in
/// `Arc<Mutex<...>>` so it can be cloned and used from spawned tasks (e.g.
/// the ReconnectAi exit/recall sequence) without holding the daemon's
/// `&mut SessionState` borrow across `.await` points.
type PtyWriter = std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>;

/// Write `bytes` to the PTY through a cloned writer, swallowing errors and
/// scoping the mutex guard tightly so it's never held across `.await` (the
/// std `MutexGuard` isn't `Send`, which would otherwise break tokio tasks).
fn pty_try_write(writer: &PtyWriter, bytes: &[u8]) {
    if let Ok(mut w) = writer.lock() {
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }
}

/// One attempt at exiting a TUI AI (Claude Code, Codex CLI, etc.) using only
/// Ctrl+C. Deliberately avoids `/exit\r`: if the TUI has any queued input
/// (a draft message the user hadn't sent, an unsubmitted slash command),
/// the `\r` would submit that draft first and turn `/exit` into a follow-up
/// prompt — and the recall string we stuff afterwards lands inside the
/// still-running TUI. Ctrl+C never touches the input buffer, and a double
/// Ctrl+C within ~1.5 s reliably exits Claude / Codex / Aider.
///
/// Sends 3 Ctrl+Cs spaced 400 ms apart, then polls `kill(pid, 0)` every
/// 200 ms for up to 3 s. Returns `true` if the process exited within the
/// window.
async fn attempt_claude_exit(writer: &PtyWriter, pid: nix::unistd::Pid) -> bool {
    for _ in 0..3 {
        pty_try_write(writer, b"\x03");
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if nix::sys::signal::kill(pid, None).is_err() {
            return true;
        }
    }
    false
}

/// Status bar configuration and live data.
pub struct StatusBar {
    /// Project name (from SCREEN_PROJECT_DIR or session name)
    pub project: String,
    /// AI process stats (CPU, mem) — pushed via `aistats` command
    pub ai_process_stats: String,
    /// AI API stats (tokens, cost) — pushed via `aistats` command
    pub ai_api_stats: String,
    /// Which stats to show (toggled by F5 / aistatstoggle)
    pub stats_mode: u8, // 0=process, 1=api, 2=both
    /// Last I/O activity timestamp
    pub last_activity: std::time::Instant,
    /// Theme name (for GPU renderer — "purple-gradient" is the default)
    pub theme: String,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            project: String::new(),
            ai_process_stats: String::new(),
            ai_api_stats: String::new(),
            stats_mode: 0,
            last_activity: std::time::Instant::now(),
            theme: "purple-gradient".into(),
        }
    }
}

/// State of a running daemon session.
pub struct SessionState {
    pub name: String,
    pub pty: PtySession,
    pub title: String,
    pub attached: bool,
    pub scrollback_max: usize,
    pub env: std::collections::HashMap<String, String>,
    pub log_file: Option<String>,
    pub logging: bool,
    pub log_writer: Option<fs::File>,
    /// Terminal emulation state
    pub terminal: immorterm_core::Terminal,
    /// Status bar state
    pub status_bar: StatusBar,
    /// Claude Code process tracker
    pub claude: crate::claude::ClaudeTracker,
    /// Window ID (from IMMORTERM_WINDOW_ID) for registry updates
    pub window_id: String,
    /// WebSocket streaming port (0 = not started)
    pub ws_port: u16,
    /// Structured logger for .grid.jsonl, .cast, .ai.jsonl
    pub structured_log: Option<crate::structured_log::StructuredLogger>,
    /// Directory for structured logs and death events
    pub structured_log_dir: PathBuf,
    /// Last user prompt from AI conversation (for status bar tooltip)
    pub last_user_prompt: Option<String>,
    /// Deferred restore ANSI — processed on first client resize so content
    /// reflows at the actual viewport dimensions, not the snapshot's old size.
    /// **Grid-only** ANSI: scrollback is restored via direct row injection
    /// (see `pending_restore_scrollback`). Mixing ANSI replay with scrollback
    /// caused a compounding bug — the prior scrollback was emulated as ANSI,
    /// scrolling rows into the new emulator's scrollback, doubling content
    /// every shelve+reattach cycle.
    pub pending_restore_ansi: Option<String>,
    /// Parsed scrollback rows from the prior session. Pushed directly into
    /// `terminal.scrollback` during deferred restore — never goes through
    /// `terminal.process()` so it doesn't compound on the next snapshot.
    pub pending_restore_scrollback: Option<immorterm_core::log::ScrollbackDump>,
    /// Claude session ID to auto-resume after restore completes.
    /// Set from IMMORTERM_CLAUDE_SESSION_ID env var; consumed after pending_restore_ansi.
    pub pending_claude_resume: Option<String>,
    /// When true, the next `subscribe_raw` response includes full scrollback
    /// instead of viewport-only. Set after `process_deferred_restore` so the
    /// WASM client receives the restored scrollback history.
    pub pending_full_snapshot: bool,
    /// Local mirror of registry `needs_attention` — avoids reading registry on
    /// every PTY frame. Set via `notify attention` IPC, cleared on PTY activity.
    pub needs_attention: bool,
    /// Agent is currently working — between a user prompt and the model's Stop.
    /// Set via `notify working` IPC, cleared via `notify idle` IPC.
    /// Ephemeral (not persisted): only the live daemon knows if work is in flight.
    pub is_working: bool,
    /// Count of WebSocket clients currently in `raw_mode` — i.e. actually
    /// rendering AI overlays. Updated by the WS loop on subscribe_raw /
    /// subscribe_control transitions and on disconnect. Used as a guard so
    /// `wait_for_event(background=false)` fails fast instead of timing out
    /// when no client can possibly deliver a click.
    pub raw_subscriber_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Bounded ring buffer of raw bytes fed to `terminal.process()`. Used to
    /// rebuild the terminal grid at a new column width (on resize, or on
    /// explicit `rerender_backlog` from the client) — same idea as
    /// `claude --resume` but generic. Older-than-buffer content stays visible
    /// via the existing scroll-to-top scrollback loader.
    pub pty_history: crate::pty_history::PtyHistory,
    /// Current git branch for this session, derived from `terminal.cwd`'s
    /// `.git/HEAD`. Refreshed on each claude_interval tick. Emitted in
    /// control events so both host flavors (VS Code extension and Tauri
    /// standalone) display the same per-session branch label without
    /// extension-side state. `None` when cwd isn't inside a git repo.
    pub branch: Option<String>,
    /// Active Workshops keyed by name. Persistent webview panes living next
    /// to the terminal — the AI's surface for "build me a real app I can
    /// iterate on" tasks. See `Workshop` doc for lifecycle. New entries are
    /// also persisted to disk under `~/.immorterm/workshops/<session>/<name>.html`.
    pub workshops: std::collections::HashMap<String, Workshop>,
    /// One-time memory-onboarding hint (set by `registry::ensure_memory_hooks`
    /// when the project has no hooks and no `immorterm` CLI was found).
    /// Consumed at event-loop start: fed through the emulator as a dim line
    /// above the first shell prompt.
    pub pending_memory_banner: Option<String>,
    /// Human→browser input (clicks/keys/scroll/pause) forwarded from the webview
    /// browser panel, queued here for the MCP screencast pump to drain via
    /// `PollBrowserInput`. The browser itself lives in the MCP process, so the
    /// daemon can only stash these — same poll bridge as `PollAiEvents`.
    /// ponytail: unbounded Vec, drained every pump tick (~15fps); cap if a
    /// wedged pump ever lets it grow.
    pub browser_input_queue: Vec<crate::ipc::BrowserInputEvent>,
}

impl SessionState {
    /// Feed bytes through the terminal emulator AND capture them in the
    /// PTY history ring buffer. Use this at every site that would otherwise
    /// call `self.terminal.process(...)` — it keeps history coverage in sync
    /// with what's actually visible.
    pub fn ingest_pty(&mut self, data: &[u8]) {
        self.terminal.process(data);
        self.pty_history.write(data);
    }

    // replay_pty_history removed 2026-06: the byte-replay-rebuild approach
    // produced scrollback corruption (`Self─{` interleaving) in sessions whose
    // PTY ring evicted older bytes mid-history, and the cols-aware variant
    // still failed on long sessions. RerenderBacklog now drives a Phase 1
    // reflow cycle on the emulator itself (no byte replay), which is the same
    // code path a user drag-resize exercises. The pty_history ring is still
    // populated (writes + record_resize) so future callers can replay if a
    // safer scheme appears.
}

/// A persistent, full-size webview pane authored by the AI.
///
/// Differs from `AiPrimitive` overlays in three ways:
/// - **Persistent**: survives across response turns until `CloseWorkshop`.
/// - **Full-size**: renders in a dedicated panel, not over the terminal grid.
/// - **On disk**: HTML is written to the session's workshops dir so the
///   user can pop out into a real browser tab.
///
/// The Shadow-DOM execution model is unchanged from `draw_html` — same
/// `<script>` semantics with `root`, `wrapper`, `card`, `prim` in scope, same
/// `data-click` → `wait_for_event` plumbing.
#[derive(Debug, Clone)]
pub struct Workshop {
    pub name: String,
    pub html: String,
    pub css: String,
    /// Wall-clock time of the most recent open/update. Used to display
    /// "modified 3 min ago" in the sidebar.
    pub modified: std::time::SystemTime,
    /// Optional prompt template auto-written to the Claude PTY on each
    /// workshop button click. Placeholders: `{data_click}`, `{name}`.
    /// When set, the daemon injects `<formatted-template>\n` into the
    /// session's PTY — Claude treats it as a typed prompt and reacts
    /// immediately, with no background subprocess needed.
    pub on_click_prompt: Option<String>,
    /// Optional rich-context template for the hook-injection path.
    /// On click, the daemon writes a marker file + types a tiny trigger;
    /// Claude's UserPromptSubmit hook reads the marker and surfaces this
    /// (after placeholder substitution) as `additionalContext`. Cleaner
    /// terminal than `on_click_prompt`; hook script must be installed.
    pub on_click_inject_context: Option<String>,
}

impl SessionState {
    pub fn socket_path(name: &str) -> PathBuf {
        let pid = std::process::id();
        socket_dir().join(format!("{}.{}", pid, name))
    }

    /// Process deferred restore: inject scrollback directly, replay grid via
    /// ANSI, then auto-resume Claude if a session ID was saved.
    ///
    /// Scrollback is pushed straight into `terminal.scrollback` via
    /// `runs_to_row` — it never goes through `terminal.process()`. Routing
    /// scrollback through the emulator (as we did before) caused a
    /// compounding bug: the prior scrollback content scrolled off the new
    /// emulator's grid INTO its scrollback again, doubling on every
    /// shelve+reattach cycle. Each subsequent dump grew (23 → 51 → 78 → 114
    /// rows in one observed session, with `ls -la` repeating 4 times).
    pub fn process_deferred_restore(&mut self, cols: u16, rows: u16) {
        if let Some(ansi) = self.pending_restore_ansi.take() {
            info!("Processing deferred restore ({} bytes) at {}x{}", ansi.len(), cols, rows);

            // 1. Inject scrollback rows DIRECTLY (no emulator scrolling).
            if let Some(dump) = self.pending_restore_scrollback.take() {
                let injected = dump.lines.len();
                let target_cols = self.terminal.cols();
                for line in dump.lines {
                    let row = immorterm_core::log::runs_to_row(
                        &line.runs,
                        target_cols,
                        line.wrapped,
                    );
                    self.terminal.scrollback.push(row);
                }
                info!("Injected {} scrollback rows directly (no emulator pass)", injected);
            }

            // 2. Clear screen + replay grid (viewport only).
            //    The new shell already output its prompt, and the restore
            //    ANSI includes the old prompt — without clearing, both
            //    appear on the same line.
            // Capture in pty_history so replay-on-resize reproduces the
            // restored grid at the new column width. Direct-injected
            // scrollback (step 1) is NOT in history — accepted v1 limit.
            self.ingest_pty(b"\x1b[H\x1b[2J");
            self.ingest_pty(ansi.as_bytes());

            // Signal that the next subscribe_raw should send full scrollback
            // so the WASM client receives the restored session history.
            self.pending_full_snapshot = true;
            info!("Flagged pending_full_snapshot for next subscribe_raw ({} scrollback rows)",
                  self.terminal.scrollback.len());

            // Auto-resume via the vendor-neutral `immorterm recall` CLI.
            // ALWAYS stuff recall after a deferred restore — even when no
            // Claude UUID was passed via env var. recall handles the full
            // 4-tier cascade:
            //   1. UUID + jsonl exists → claude --resume <uuid>
            //   2. UUID present but no jsonl, /immorterm:recall skill installed
            //      → claude /immorterm:recall <iid> <uuid>
            //   3. No UUID, skill installed → claude /immorterm:recall <iid>
            //   4. No UUID, no skill → plain claude (or bare shell)
            // The previous gate (`pending_claude_resume.is_some()`) skipped
            // recall for sessions whose claude_session_id was never tracked
            // by the daemon (chronic registry race) — meaning users got a
            // bare zsh prompt on reattach instead of Claude rebuilding
            // context from memory packs.
            //
            // Honor IMMORTERM_NO_AUTO_RESUME — set when the user explicitly
            // exited claude before shelving. We skip BOTH the scroll-out and
            // the recall stuffing: the visible echo of `immorterm-ai recall`
            // at the prompt is itself the artifact the user complained about,
            // even though recall returns Ok(()) immediately on the same flag.
            // Without scroll-out, the restored grid sits naturally and the
            // new shell prompt prints wherever the ANSI replay left the cursor.
            let no_auto_resume = std::env::var("IMMORTERM_NO_AUTO_RESUME")
                .ok()
                .is_some_and(|v| v == "1");

            if no_auto_resume {
                info!("IMMORTERM_NO_AUTO_RESUME=1 — skipping recall stuffing and viewport scroll-out");
                self.pending_claude_resume.take();
            } else {
                let claude_id = self.pending_claude_resume.take();

                // Scroll the visible viewport into scrollback ONLY when we're
                // actually going to clear and resume claude — i.e. we have a
                // UUID hint (tier 1/2). For tier 3/4 (no UUID, plain recall
                // returning to bare shell) we skip the scroll-out: there's
                // nothing about to overwrite the viewport, so blanking it
                // would just produce cosmetic empty rows.
                //
                // Newlines (not `ESC[2J`) preserve content: `ESC[2J` zeroes
                // the grid without pushing rows to scrollback in our emulator.
                if claude_id.is_some() {
                    let rows = self.terminal.rows();
                    self.ingest_pty(b"\x1b[999;1H");
                    for _ in 0..rows {
                        self.ingest_pty(b"\n");
                    }
                    self.ingest_pty(b"\x1b[H");
                }

                let bin = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.into_os_string().into_string().ok())
                    .unwrap_or_else(|| "immorterm-ai".to_string());
                if let Some(ref id) = claude_id {
                    info!("Auto-resuming via immorterm recall (claude session: {})", id);
                } else {
                    info!("Stuffing immorterm recall with no UUID (tier 3/4 cascade)");
                }
                let cmd = format!("{} recall\n", bin);
                if let Err(e) = self.pty.write_all(cmd.as_bytes()) {
                    warn!("Failed to send immorterm recall: {}", e);
                }
            }
        }
    }
}

/// Process start time for uptime tracking in death events.
static DAEMON_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Write a death event JSON to the structured log directory.
/// Called on any exit path (signal, PTY exit, kill request, panic).
fn write_death_event(dir: &Path, reason: &str, session_name: &str) {
    let now = std::time::SystemTime::now();
    let timestamp = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            // Format as ISO 8601 (simplified — no chrono dependency)
            let secs = d.as_secs();
            format!("{}Z", secs)
        })
        .unwrap_or_else(|_| "unknown".into());

    let uptime_secs = DAEMON_START
        .get()
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);

    let death = serde_json::json!({
        "timestamp": timestamp,
        "reason": reason,
        "pid": std::process::id(),
        "session": session_name,
        "uptime_secs": uptime_secs,
    });

    if let Ok(json) = serde_json::to_string(&death) {
        let path = dir.join("death.json");
        if let Err(e) = fs::write(&path, json) {
            eprintln!("Failed to write death event to {:?}: {}", path, e);
        }
    }
}

/// Create a new detached session via double-fork.
pub fn create_session(
    name: &str,
    shell: &str,
    scrollback: usize,
    _config: Option<String>,
    log_enabled: bool,
    logfile: Option<String>,
) -> Result<()> {
    // First fork
    match unsafe { unistd::fork() }.context("First fork failed")? {
        ForkResult::Parent { child: _ } => {
            // Parent exits immediately — the VS Code extension expects this
            Ok(())
        }
        ForkResult::Child => {
            // Create new session (become session leader)
            unistd::setsid().context("setsid failed")?;

            // Second fork — orphan the daemon (no controlling terminal)
            match unsafe { unistd::fork() }.context("Second fork failed")? {
                ForkResult::Parent { child: _ } => {
                    // First child exits
                    std::process::exit(0);
                }
                ForkResult::Child => {
                    // This is the daemon process
                    run_daemon(name, shell, scrollback, log_enabled, logfile)
                }
            }
        }
    }
}

/// The actual daemon process — sets up PTY, socket, and event loop.
fn run_daemon(
    name: &str,
    shell: &str,
    scrollback: usize,
    log_enabled: bool,
    logfile: Option<String>,
) -> Result<()> {
    // Ignore SIGHUP so the daemon doesn't die when the terminal detaches
    unsafe {
        signal::signal(signal::Signal::SIGHUP, signal::SigHandler::SigIgn)
            .context("Failed to ignore SIGHUP")?;
    }

    // Ignore SIGPIPE so writes to broken pipes return errors instead of killing the daemon
    unsafe {
        signal::signal(signal::Signal::SIGPIPE, signal::SigHandler::SigIgn)
            .context("Failed to ignore SIGPIPE")?;
    }

    // Ensure shell integration files exist (~/.immorterm/shell/)
    if let Err(e) = crate::commands::ensure_shell_integration() {
        error!("Failed to set up shell integration: {}", e);
    }

    // Set environment variables for the PTY child process.
    // These are inherited by PtySession::spawn() which copies all env vars.
    // SAFETY: called before spawning any threads — single-threaded at this point.
    unsafe {
        std::env::set_var("IMMORTERM_SESSION", name);
        std::env::set_var("TERM", "xterm-256color");
        std::env::set_var("TERM_PROGRAM", "ImmorTerm");
        std::env::set_var("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        std::env::set_var("COLORTERM", "truecolor");
    }

    // Set STY for backward compat with scripts that check for screen sessions.
    // Format: "PID.sessionname" — same as GNU screen.
    // Also ensures IMMORTERM_WINDOW_ID flows through to the shell
    // (it's already in our env from the extension, but be explicit).
    let daemon_pid = std::process::id();
    // SAFETY: still single-threaded, before tokio runtime starts.
    unsafe {
        std::env::set_var("STY", format!("{}.{}", daemon_pid, name));
    }

    // Set up shell init sourcing based on shell type.
    // zsh: ZDOTDIR makes zsh read our .zshrc shim
    // bash: ENV makes bash source our .bashrc shim (for non-interactive, -l for login)
    let shell_dir = crate::dirs_home().join(".immorterm").join("shell");
    // SAFETY: still single-threaded, before tokio runtime starts.
    unsafe {
        if shell.contains("zsh") {
            std::env::set_var("ZDOTDIR", &shell_dir);
        } else if shell.contains("bash") {
            std::env::set_var("BASH_ENV", shell_dir.join("shell-init.bash"));
            std::env::set_var("ENV", shell_dir.join("shell-init.bash"));
        }
    }

    // Default terminal size
    let cols = 80;
    let rows = 24;

    // Spawn PTY with shell
    let pty = PtySession::spawn(shell, cols, rows)
        .context("Failed to spawn PTY")?;

    // Set up logging
    let log_writer = if log_enabled {
        logfile.as_ref().and_then(|path| {
            fs::File::create(path)
                .map_err(|e| error!("Failed to create log file {}: {}", path, e))
                .ok()
        })
    } else {
        None
    };

    // Create terminal emulator
    let mut terminal = immorterm_core::Terminal::new(cols as usize, rows as usize);
    terminal.set_scrollback(scrollback);
    terminal.enable_marker_parsing();

    // Derive project name and full path from env
    let project_dir_full = std::env::var("SCREEN_PROJECT_DIR").unwrap_or_default();
    let project = project_dir_full
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .to_string();
    // Register in shared session registry (before logfile is moved).
    //
    // Ephemeral wrappers (immorterm-p, future short-lived helper sessions)
    // set IMMORTERM_SKIP_REGISTRY=1 so they don't pollute the registry —
    // they live for seconds, are not user-facing, and shouldn't appear in
    // the sidebar nor be restored across VS Code reloads. The daemon still
    // accepts -X commands and writes its socket; only the registry entry
    // (+ later periodic updates) is suppressed.
    let skip_registry = std::env::var("IMMORTERM_SKIP_REGISTRY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // Skip-registry daemons must NOT carry a window_id. Wrapper daemons
    // inherit the HOST session's IMMORTERM_WINDOW_ID through the digest
    // pipeline's environment; keeping it lets the 10s self-heal loop (and
    // the claude_session/claude_stats updates, all keyed by window_id)
    // hijack the host session's registry row — stamping the wrapper's
    // pid/ws_port into it, so a VS Code reload reattaches the host tab to
    // the wrapper daemon (2026-06-07 "Dodo" incident). Every registry
    // write path is gated on a non-empty window_id, so clearing it here
    // closes them all at once.
    let window_id = if skip_registry {
        String::new()
    } else {
        std::env::var("IMMORTERM_WINDOW_ID")
            .or_else(|_| std::env::var("SCREEN_WINDOW_ID"))
            .unwrap_or_default()
    };
    if !skip_registry {
        crate::registry::register_session(
            name,
            shell,
            logfile.as_deref(),
        );
    }

    // Memory-wiring bootstrap (last onboarding gap): a project opened via the
    // Tauri app gets project.json + terminals here, but nothing installs the
    // memory hooks — that TS installer only runs from the CLI / VS Code
    // extension. If hooks are missing, auto-install via the `immorterm` CLI
    // (probed through a login shell), or carry a one-time hint line to render
    // in this session. Best-effort; never fails the spawn.
    let pending_memory_banner = if skip_registry {
        None
    } else {
        let owner = crate::registry::resolve_owner_project(&project_dir_full);
        crate::registry::ensure_memory_hooks(&owner.owner_dir)
    };

    // Structured logging: derive log directory from project dir or fallback to ~/.immorterm/logs/
    let base_log_dir = if !project_dir_full.is_empty() {
        PathBuf::from(&project_dir_full)
            .join(".immorterm")
            .join("terminals")
            .join("logs")
    } else {
        crate::dirs_home()
            .join(".immorterm")
            .join("logs")
    };

    // Per-session directory: {base_log_dir}/{date}_{window_id}/
    // When window_id is unavailable (daemon sessions), use the session name instead
    // to ensure each session gets its own isolated directory.
    let structured_log_dir = {
        let dir_suffix = if !window_id.is_empty() {
            window_id.clone()
        } else {
            name.to_string()
        };
        // Reuse existing session directory if one exists for this window_id,
        // so that respawned daemons continue writing to the same log files.
        // New naming: bare windowId (no date prefix). Matches both legacy
        // date-prefixed dirs and new bare dirs. See task #24.
        crate::registry::find_existing_session_dir(&base_log_dir, &dir_suffix)
            .unwrap_or_else(|| base_log_dir.join(&dir_suffix))
    };

    // OpenMemory push channel — created outside the runtime (only spawn needs it).
    // For wrapper sessions (skip_registry=1) we still build the channel so the
    // spawn-side signature stays uniform, but drop the sender — no events get
    // queued because we install neither file logger nor event sink below. The
    // push task observes the closed receiver and exits.
    let (push_tx, push_rx) =
        tokio::sync::mpsc::channel::<crate::openmemory_push::TerminalLogEvent>(
            crate::openmemory_push::CHANNEL_CAPACITY,
        );

    // Ephemeral wrapper sessions (immorterm-p): no terminal log file, no events
    // pushed to memory service. The transcript is captured by the wrapper-
    // spawned claude itself; persisting daemon-side logs here pollutes the
    // project's .immorterm/terminals/logs/ tree and the memory service's
    // terminal_logs SQLite table.
    let structured_log = if skip_registry {
        drop(push_tx);
        None
    } else {
        let event_sink: Option<Box<dyn structured_logs::LogEventSink>> = Some(Box::new(
            crate::structured_log::OpenMemoryEventSink::new(name, push_tx),
        ));
        Some(crate::structured_log::StructuredLogger::new(
            name,
            &structured_log_dir,
            cols as usize,
            rows as usize,
            event_sink,
        ))
    };

    // Seed had_ai_session from the boot-time UUID env var so restored
    // sessions know they had AI before, even before the periodic scan
    // re-detects a (newly-launched) Claude process tree.
    let restored_claude_uuid = std::env::var("IMMORTERM_CLAUDE_SESSION_ID").ok()
        .filter(|s| !s.is_empty());
    let mut claude_tracker = crate::claude::ClaudeTracker::new(&window_id);
    if restored_claude_uuid.is_some() {
        claude_tracker.had_ai_session = true;
    }

    let state = SessionState {
        name: name.to_string(),
        pty,
        title: String::new(),
        attached: false,
        scrollback_max: scrollback,
        env: std::collections::HashMap::new(),
        log_file: logfile,
        logging: log_enabled,
        log_writer,
        terminal,
        status_bar: StatusBar {
            project,
            ..Default::default()
        },
        claude: claude_tracker,
        window_id,
        ws_port: 0,
        structured_log,
        structured_log_dir: structured_log_dir.clone(),
        last_user_prompt: None,
        pending_restore_ansi: None,
        pending_restore_scrollback: None,
        pending_claude_resume: restored_claude_uuid,
        pending_full_snapshot: false,
        needs_attention: false,
        is_working: false,
        raw_subscriber_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        pty_history: {
            let mut h = crate::pty_history::PtyHistory::new(crate::pty_history::DEFAULT_CAP_BYTES);
            // Anchor the initial dims so replay knows the cols active for any
            // bytes written before the first WS Resize arrives.
            h.record_resize(cols, rows);
            h
        },
        branch: None,
        workshops: std::collections::HashMap::new(),
        pending_memory_banner,
        browser_input_queue: Vec::new(),
    };

    // Record daemon start time for uptime tracking in death events
    DAEMON_START.get_or_init(std::time::Instant::now);

    // File-based tracing — after double-fork, stderr goes to /dev/null so the default
    // subscriber is useless. Write to {structured_log_dir}/daemon.log instead.
    {
        let file_appender = tracing_appender::rolling::never(&structured_log_dir, "daemon.log");
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
        // Leak the guard so it lives for the entire daemon process.
        // This is intentional — the daemon runs until exit, and the guard must
        // outlive all tracing calls to ensure logs are flushed.
        let _guard = Box::leak(Box::new(_guard));
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_target(false)
            .with_ansi(false)
            .with_writer(non_blocking)
            .try_init()
            .ok();
    }

    // Panic hook — write death event before aborting
    {
        let panic_log_dir = structured_log_dir.clone();
        let panic_session = name.to_string();
        std::panic::set_hook(Box::new(move |info| {
            let msg = info.to_string();
            eprintln!("PANIC: {}", msg);
            write_death_event(&panic_log_dir, &format!("panic:{}", msg), &panic_session);
        }));
    }

    // Create socket
    let socket_path = SessionState::socket_path(name);
    if socket_path.exists() {
        fs::remove_file(&socket_path).ok();
    }

    // Run the tokio runtime
    let rt = tokio::runtime::Runtime::new()
        .context("Failed to create tokio runtime")?;

    // Derive user_id for OpenMemory from project directory name (matches extension convention)
    let push_user_id = if !project_dir_full.is_empty() {
        project_dir_full
            .rsplit('/')
            .next()
            .unwrap_or("default")
            .to_string()
    } else {
        "default".to_string()
    };

    rt.block_on(async move {
        // Spawn background task that pushes AI events to OpenMemory REST API
        crate::openmemory_push::spawn_push_task(push_rx, push_user_id);

        run_event_loop(state, socket_path).await
    })
}

/// Main event loop: handles PTY output, client connections, and commands.
async fn run_event_loop(mut state: SessionState, socket_path: PathBuf) -> Result<()> {
    let listener = UnixListener::bind(&socket_path)
        .context("Failed to bind Unix socket")?;

    // Set socket permissions: 0600 = detached, 0700 = attached
    set_socket_permissions(&socket_path, false);

    info!("Daemon started for session '{}' at {:?}", state.name, socket_path);

    // Restore previous terminal state from grid snapshot (respawn after reboot/crash).
    // The structured_log_dir reuses the existing session dir via find_existing_session_dir(),
    // so grid.jsonl from the previous daemon lifetime is available.
    // NOTE: We defer processing until the first client resize so the ANSI reflows
    // at the actual viewport dimensions. Processing at the snapshot's old dimensions
    // (e.g. 67x24) causes content to stay narrow even after resize.
    if let Some(restore) = structured_logs::restore::restore_session(
        &state.structured_log_dir,
        &state.name,
    ) {
        let sb_lines = restore.scrollback_dump.as_ref().map(|d| d.lines.len()).unwrap_or(0);
        info!("Deferring restore of {} grid-bytes + {} scrollback rows ({}x{}) until first client resize",
            restore.grid_ansi.len(), sb_lines, restore.cols, restore.rows);
        // Grid-only ANSI: scrollback is restored separately via direct row
        // injection (avoids the compounding bug where ANSI-replayed scrollback
        // re-enters the emulator's scroll pipeline and doubles every cycle).
        state.pending_restore_ansi = Some(restore.grid_ansi);
        state.pending_restore_scrollback = restore.scrollback_dump;
    }

    // One-time memory onboarding hint — fed through the emulator (and the
    // PTY history ring, via ingest_pty) before any PTY bytes are processed,
    // so it renders as a dim line above the first shell prompt and survives
    // in scrollback like normal output.
    if let Some(hint) = state.pending_memory_banner.take() {
        state.ingest_pty(format!("\x1b[2m{hint}\x1b[0m\r\n").as_bytes());
    }

    // Broadcast channel for PTY output — raw bytes for the event loop (marker parsing + HtmlBlocks)
    let (pty_tx, _) = broadcast::channel::<Vec<u8>>(256);
    // Filtered broadcast — <<html>> markers stripped, for WebSocket/GUI/attach clients
    let (pty_filtered_tx, _) = broadcast::channel::<Vec<u8>>(256);

    // Get the PTY reader
    let mut pty_reader = state.pty.take_reader()
        .context("Failed to get PTY reader")?;

    let pty_tx_clone = pty_tx.clone();
    let pty_filtered_tx_clone = pty_filtered_tx.clone();
    let pty_child = state.pty.child_pid();

    // Task: Read PTY output and broadcast on two channels:
    //   pty_tx          — raw bytes (event loop: terminal emulation, marker parsing, HtmlBlocks)
    //   pty_filtered_tx — markers stripped (WebSocket, GUI window, Unix attach clients)
    let mut pty_read_handle = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        let mut broadcast_parser = immorterm_core::marker::MarkerParser::new();
        broadcast_parser.enable();
        loop {
            match pty_reader.read(&mut buf).await {
                Ok(0) => {
                    info!("PTY closed (shell exited)");
                    break;
                }
                Ok(n) => {
                    // Raw bytes → event loop (terminal emulator with marker parsing)
                    let _ = pty_tx_clone.send(buf[..n].to_vec());
                    // Filtered bytes → external clients (markers stripped)
                    let mut filtered = Vec::with_capacity(n);
                    for &byte in &buf[..n] {
                        for event in broadcast_parser.feed(byte) {
                            if let immorterm_core::marker::MarkerEvent::PassThrough(b) = event {
                                filtered.push(b);
                            }
                        }
                    }
                    if !filtered.is_empty() {
                        let _ = pty_filtered_tx_clone.send(filtered);
                    }
                }
                Err(e) => {
                    error!("PTY read error: {}", e);
                    break;
                }
            }
        }
    });

    // Task: Accept client connections
    let session_name = state.name.clone();
    let socket_path_clone = socket_path.clone();

    // Graceful SIGTERM/SIGINT handling
    use futures_util::StreamExt;
    let mut signals = signal_hook_tokio::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGCHLD,
    ])?;

    // Persistent subscriber for terminal emulation — MUST be created before the
    // loop. Creating a new subscriber per iteration loses messages between iterations.
    let mut pty_rx = pty_tx.subscribe();

    // ─── WebSocket streaming setup ───────────────────────────────────
    // Viewport diff broadcast for WebSocket clients (pre-serialized JSON)
    let (viewport_tx, _) = broadcast::channel::<Arc<String>>(64);

    // AI layer state broadcast for GUI window (serialized Vec<AiPrimitive>)
    let (ai_layer_tx, _) = broadcast::channel::<Arc<Vec<u8>>>(64);

    // Control events for extension subscribers (~1 event per 10s)
    let (control_tx, _) = broadcast::channel::<Arc<String>>(64);

    // Command channel from WebSocket clients → event loop
    let (ws_cmd_tx, mut ws_cmd_rx) = tokio::sync::mpsc::channel::<crate::websocket::WsCommand>(256);

    // AI event broadcast — clicks/hovers sent directly from WS handlers, bypassing the
    // main select loop. This allows WaitForAiEvent IPC handlers to receive events even
    // while the main loop is blocked on their behalf.
    let (ai_event_tx, _) = broadcast::channel::<immorterm_core::ai_layer::AiEvent>(64);

    // AI eval broadcast — `eval_in_primitive` fire-and-forget JS snippets that
    // each WS client runs inside the matching primitive's Shadow DOM. Carries
    // pre-serialized JSON: `{"primitive_id":N,"js":"..."}`. Bypasses the
    // ai_layer dirty-flag broadcast so the snippet fires immediately.
    let (ai_eval_tx, _) = broadcast::channel::<Arc<String>>(64);

    // Workshop event broadcast — open/update/eval/close events for the
    // persistent webview panes. Pre-serialized envelopes:
    //   `{"event":"open","name":"...","html":"...","css":"..."}`
    //   `{"event":"update","name":"...","html":"...","css":"..."}`
    //   `{"event":"eval","name":"...","js":"..."}`
    //   `{"event":"close","name":"..."}`
    // Channel is independent of the ai_layer dirty broadcast so workshop
    // updates fire immediately, not on the 60fps frame tick.
    let (workshop_tx, _) = broadcast::channel::<Arc<String>>(64);

    // Start WebSocket server (dynamic port, localhost only)
    let ws_port = crate::websocket::start_websocket_server(
        state.name.clone(),
        viewport_tx.clone(),
        ws_cmd_tx,
        control_tx.clone(),
        ai_layer_tx.clone(),
        ai_event_tx.clone(),
        ai_eval_tx.clone(),
        workshop_tx.clone(),
        state.raw_subscriber_count.clone(),
    )
    .await
    .unwrap_or(0);

    state.ws_port = ws_port;

    // Write port file for client discovery
    if ws_port > 0 {
        let port_file = socket_dir().join(format!("{}.{}.ws", std::process::id(), state.name));
        std::fs::write(&port_file, ws_port.to_string()).ok();
        info!("WebSocket: ws://127.0.0.1:{}", ws_port);

        // Update registry with ws_port + session_type now that WS is running
        let mut registry = crate::registry::Registry::load();
        if let Some(entry) = registry.sessions.iter_mut().find(|e| e.pid == std::process::id()) {
            entry.ws_port = Some(ws_port);
            entry.session_type = Some("ai".to_string());
            if let Err(e) = registry.save() {
                error!("Failed to update registry with ws_port: {}", e);
            }
        }
    }

    // 60fps frame timer for viewport diff broadcasting
    let mut frame_interval = tokio::time::interval(Duration::from_millis(16));
    frame_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut frame_seq: u64 = 0;
    let start_time = std::time::Instant::now();
    // CSI ?2026 (Synchronized Output) watchdog. When a TUI like Claude Code
    // sets mode 2026, we defer broadcasting viewport diffs until the matching
    // reset arrives. If it never comes (crash / badly-behaved program), this
    // timestamp drives a 150ms watchdog so the terminal can't freeze.
    let mut sync_update_since: Option<std::time::Instant> = None;
    const SYNC_UPDATE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);

    // Track the last-seen terminal title (from OSC 0/2) to detect changes.
    // When a program (like Claude Code) sends \033]0;title\007, the VTE performer
    // sets state.terminal.title. We compare each frame to detect the change,
    // sync it to state.title, and broadcast to WebSocket clients.
    let mut last_terminal_title = state.terminal.title.clone();
    // ─── End WebSocket setup ─────────────────────────────────────────

    // ─── Team auto-launch setup ─────────────────────────────────────
    // Watch ~/.claude/teams/ for new teams. When a new team is created
    // (not pre-existing at startup), auto-launch the GPU team-view window.
    let team_watcher_result = team_watcher::start_team_watcher().await;
    let mut team_rx = match &team_watcher_result {
        Ok((_shared, tx)) => Some(tx.subscribe()),
        Err(e) => {
            warn!("Team watcher failed to start (teams auto-launch disabled): {}", e);
            None
        }
    };
    // Pre-populate with initially discovered teams — don't auto-launch for these
    let mut auto_launched_teams = std::collections::HashSet::new();
    if let Ok((shared, _)) = &team_watcher_result {
        let initial = shared.read().await;
        for name in initial.keys() {
            auto_launched_teams.insert(name.clone());
        }
        if !auto_launched_teams.is_empty() {
            debug!("Team watcher: {} pre-existing teams (won't auto-launch)", auto_launched_teams.len());
        }
    }
    // ─── End team auto-launch setup ──────────────────────────────────

    // Claude tracking: scan process tree every 10 seconds
    let mut claude_interval = tokio::time::interval(std::time::Duration::from_secs(10));
    // Skip the immediate first tick — give the shell time to start
    claude_interval.tick().await;

    // Structured logging: periodic snapshot every 30 seconds
    let mut snapshot_interval = tokio::time::interval(std::time::Duration::from_secs(30));
    snapshot_interval.tick().await;

    // Channel state: sender to the registered channel server WS client + inbox watcher
    let mut channel_tx: Option<tokio::sync::mpsc::Sender<String>> = None;
    let mut channel_inbox_rx: Option<tokio::sync::mpsc::Receiver<crate::channel_registry::ChannelMessage>> = None;
    let mut chat_overlay: Option<crate::chat_overlay::ChatOverlay> = None;
    let mut channel_partner_id: Option<String> = None;

    // Subscribe the main loop to the AI event broadcast so we can react to
    // events that need session-state access (PTY write on workshop clicks).
    // Other subscribers (WaitForAiEvent IPC handlers, etc.) keep their own
    // independent subscriptions — broadcast lets all of them receive the same
    // event.
    let mut main_ai_event_rx = ai_event_tx.subscribe();

    // Worktree tracking: snapshot of state.terminal.cwd from the last interval
    // tick. When it changes, we re-resolve owner-project against our own
    // owner_project_dir and update the registry's worktree field accordingly.
    // Empty initial value means "we haven't observed any cwd yet" — first
    // non-empty cwd triggers a check.
    let mut last_known_cwd: String = String::new();

    loop {
        tokio::select! {
            // Accept new client connection
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        handle_client_connection(
                            stream,
                            &mut state,
                            &pty_tx,
                            &pty_filtered_tx,
                            &ai_layer_tx,
                            &socket_path_clone,
                            &control_tx,
                            &ai_event_tx,
                            &ai_eval_tx,
                            &workshop_tx,
                            &channel_partner_id,
                            &mut chat_overlay,
                        ).await;
                    }
                    Err(e) => {
                        error!("Accept error: {}", e);
                    }
                }
            }
            // PTY output: process through terminal emulator + structured logging
            data = pty_rx.recv() => {
                if let Ok(data) = data {
                    // Feed through terminal emulator + ring buffer
                    state.ingest_pty(&data);
                    // Track last activity
                    state.status_bar.last_activity = std::time::Instant::now();

                    // Structured logging: feed parsed output + drain prompt events
                    if let Some(ref mut slog) = state.structured_log {
                        slog.on_pty_output(&data, &state.terminal);
                        let events = state.terminal.drain_prompt_events();
                        if !events.is_empty() {
                            slog.on_prompt_events(&events, &state.terminal);
                        }
                        // Check for new user prompt (for status bar tooltip)
                        if let Some(prompt) = slog.last_user_prompt()
                            && state.last_user_prompt.as_deref() != Some(prompt) {
                                state.last_user_prompt = Some(prompt.to_string());
                                let event = crate::websocket::build_control_event_with_prompt(
                                    "user_prompt",
                                    &state.claude,
                                    state.last_user_prompt.as_deref(),
                                );
                                let _ = control_tx.send(Arc::new(event));
                            }
                    }

                    // Drain AI stats from OSC 1337;ImmorTerm
                    if let Some(ai_event) = state.terminal.pending_ai_stats_event.take() {
                        let old_session = state.claude.session_id.clone();
                        state.claude.session_id = Some(ai_event.session_id.clone());
                        state.claude.had_ai_session = true;
                        state.claude.api_stats = crate::claude::ClaudeApiStats {
                            model: ai_event.model,
                            cost_usd: ai_event.cost_usd,
                            context_pct: ai_event.context_pct,
                            transcript_path: ai_event.transcript_path,
                        };
                        if let Some(mode) = ai_event.permission_mode {
                            state.claude.permission_mode = Some(mode);
                        }
                        // Update Rust-formatted strings (used by native window via GetStatusBar)
                        state.status_bar.ai_api_stats = state.claude.format_api_stats();

                        // Update registry
                        if !state.window_id.is_empty() {
                            let mut registry = crate::registry::Registry::load();
                            if old_session.as_deref() != Some(&ai_event.session_id) {
                                info!("Claude session via OSC: {}", &ai_event.session_id[..8.min(ai_event.session_id.len())]);
                                registry.update_claude_session(&state.window_id, &ai_event.session_id);
                            }
                            registry.update_claude_stats(&state.window_id, &state.claude);
                            let _ = registry.save();
                        }

                        // Fire WebSocket control event
                        let ctrl = crate::websocket::build_control_event("claude_update", &state.claude);
                        let _ = control_tx.send(Arc::new(ctrl));
                    }

                    // Drain terminal replies (DA1, DA2, XTVERSION, Kitty keyboard, DSR)
                    // and write them back to the PTY so applications get responses.
                    let replies = state.terminal.drain_replies();
                    if !replies.is_empty() {
                        for reply in replies {
                            if let Err(e) = state.pty.write_all(&reply) {
                                warn!("Failed to write terminal reply to PTY: {}", e);
                            }
                        }
                    }

                    // Drain generic ImmorTerm OSC events
                    for evt in state.terminal.pending_immorterm_events.drain(..) {
                        if evt.event_type == "share_consumed" {
                            let source = evt.params.get("src").cloned().unwrap_or_default();
                            // URL-decode: '+' → ' '
                            let source = source.replace('+', " ");
                            let ctrl = serde_json::json!({
                                "type": "control_event",
                                "event": "share_consumed",
                                "source_name": source,
                            }).to_string();
                            let _ = control_tx.send(Arc::new(ctrl));
                        } else if evt.event_type == "alignment" {
                            // OSC 1337;ImmorTerm;evt=alignment;align=<v>;dir=<v> ST
                            let alignment = evt.params.get("align").cloned();
                            let direction = evt.params.get("dir").cloned();
                            let msg = serde_json::json!({
                                "type": "alignment_update",
                                "alignment": alignment,
                                "direction": direction,
                            });
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = control_tx.send(Arc::new(json));
                            }
                        } else if evt.event_type == "attention" {
                            // Claude Code needs user attention (permission prompt or idle).
                            // Forward to all WS clients so the sidebar shows a 🔔 badge.
                            let ctrl = serde_json::json!({
                                "type": "control_event",
                                "event": "attention",
                            }).to_string();
                            let _ = control_tx.send(Arc::new(ctrl));
                        }
                    }

                    // Drain inline <<html>> blocks and create scroll-anchored AI primitives
                    for block in state.terminal.drain_html_blocks() {
                        let anchor_mode = match block.attrs.get("anchor").map(|s| s.as_str()) {
                            Some("fixed") => immorterm_core::ai_layer::AnchorMode::Fixed,
                            _ => immorterm_core::ai_layer::AnchorMode::Scroll {
                                scrollback_at_creation: block.scrollback_at_creation,
                            },
                        };
                        let visible_row = block.anchor_row.saturating_sub(block.scrollback_at_creation);
                        state.terminal.ai_layer.add_html(
                            immorterm_core::ai_layer::AiHtml {
                                html: block.content,
                                css: String::new(),
                                x: 0.0,
                                y: 0.0,
                                width: 0.0,
                                height: 0.0,
                                anchor_row: Some(visible_row),
                                on_click_prompt: None,
                                on_click_inject_context: None,
                            },
                            anchor_mode,
                            block.attrs.get("name").cloned(),
                        );
                        // Space reservation: height=N injects N blank lines to push
                        // terminal content below the overlay, preventing text overlap.
                        if let Some(h) = block.attrs.get("height")
                            .and_then(|s| s.parse::<usize>().ok())
                        {
                            let newlines = "\n".repeat(h);
                            // Internal terminal — advances cursor, updates viewport
                            state.ingest_pty(newlines.as_bytes());
                            // Filtered broadcast — WebSocket/GUI clients see the space too
                            let _ = pty_filtered_tx.send(newlines.into_bytes());
                        }
                    }

                    // Legacy raw log (kept for backward compat until fully migrated)
                    if state.logging
                        && let Some(ref mut writer) = state.log_writer {
                            use std::io::Write;
                            let _ = writer.write_all(&data);
                            let _ = writer.flush();
                        }
                }
            }
            // 60fps frame tick — compute viewport diff and broadcast to WS clients
            _ = frame_interval.tick() => {
                let elapsed = start_time.elapsed().as_secs_f32();
                let anim_active = state.terminal.ai_layer.tick_animations(elapsed);

                // Only broadcast if something changed
                let terminal_dirty = state.terminal.dirty;
                let ai_dirty = state.terminal.ai_layer.dirty;
                let overlay_dirty = state.terminal.overlays.dirty;

                // CSI ?2026 (Synchronized Output): defer broadcast while active,
                // with a watchdog so a dropped reset can't freeze the terminal.
                let sync_active = state.terminal.modes.synchronized_update;
                let sync_defer = if sync_active {
                    match sync_update_since {
                        Some(t) => t.elapsed() < SYNC_UPDATE_TIMEOUT,
                        None => {
                            sync_update_since = Some(std::time::Instant::now());
                            true
                        }
                    }
                } else {
                    sync_update_since = None;
                    false
                };

                if !sync_defer && (terminal_dirty || anim_active || ai_dirty || overlay_dirty) {
                    let ai_events = state.terminal.ai_layer.drain_events();
                    let diff = compute_viewport_diff(&state.terminal, frame_seq, ai_dirty, &ai_events);
                    frame_seq += 1;

                    if let Ok(json) = serde_json::to_string(&diff) {
                        let _ = viewport_tx.send(Arc::new(json));
                    }

                    // Broadcast AI layer state to GUI windows when AI content changed
                    if ai_dirty || anim_active {
                        let wrapper = serde_json::json!({
                            "sb_len": state.terminal.scrollback.len(),
                            "primitives": &state.terminal.ai_layer.primitives,
                        });
                        if let Ok(json) = serde_json::to_vec(&wrapper) {
                            let _ = ai_layer_tx.send(Arc::new(json));
                        }
                    }

                    clear_dirty_flags(&mut state.terminal);
                }

                // Detect OSC title changes (from programs like Claude Code sending \033]0;title\007).
                // This runs every frame (~16ms) but the string comparison is cheap.
                if state.terminal.title != last_terminal_title {
                    let new_title = state.terminal.title.clone();
                    let elapsed = start_time.elapsed();

                    // Grace period: suppress title broadcasts during the first 2s after spawn.
                    // Shells (zsh, oh-my-zsh) emit OSC 0 "zsh" during startup, which would
                    // overwrite the user-facing "immorterm-#" display name. We still track
                    // last_terminal_title so we don't re-fire when the grace period ends.
                    if elapsed < std::time::Duration::from_secs(2) {
                        info!("Terminal title changed via OSC (suppressed, {:.0?} < 2s grace): '{}' → '{}'",
                            elapsed, last_terminal_title, new_title);
                        last_terminal_title = new_title;
                    } else {
                        info!("Terminal title changed via OSC: '{}' → '{}'", last_terminal_title, new_title);

                        // Sync state.title (used by build_full_viewport, GetControlState)
                        state.title = new_title.clone();
                        last_terminal_title = new_title.clone();

                        // Broadcast title_changed to ALL WS clients:
                        // - viewport_tx for viewport-mode clients (text-based)
                        // - control_tx for raw-mode clients (GPU terminal webview)
                        let title_msg = serde_json::json!({
                            "type": "title_changed",
                            "title": new_title,
                        });
                        if let Ok(json) = serde_json::to_string(&title_msg) {
                            let json = Arc::new(json);
                            let _ = viewport_tx.send(Arc::clone(&json));
                            let _ = control_tx.send(json);
                        }

                        // Update registry display_name so it persists across restarts
                        let mut registry = crate::registry::Registry::load();
                        if let Some(entry) = registry.sessions.iter_mut().find(|e| e.pid == std::process::id()) {
                            // Only update if the title is NOT locked by the user
                            if !entry.title_locked {
                                entry.display_name = new_title.clone();
                            }
                            entry.title = new_title;
                        }
                        if let Err(e) = registry.save() {
                            error!("Failed to update registry with new title: {}", e);
                        }
                    }
                }

                // Drain bell flag (consumed but not forwarded — BEL alone is
                // not a reliable "needs attention" signal; the Notification hook
                // sends `notify attention` via IPC instead).
                let _ = state.terminal.take_bell();

                // Registry's `needs_attention` is no longer cleared on PTY activity —
                // that would wipe the flag within ms of Notification firing, breaking
                // VS Code reload persistence. Dismissal is now explicit:
                //   - `notify working` (UserPromptSubmit, PostToolUse:AskUserQuestion)
                //   - `WsCommand::DismissAttention` (frontend's visual auto-clear at
                //     gpu-terminal.html:5689 mirrors itself to registry)
            }
            // AI event broadcast — workshop AND draw_html button clicks fire
            // here. If the entity has `on_click_prompt` set, we auto-inject
            // the formatted template into the Claude PTY so the AI wakes as
            // if the user typed it (zero-subprocess wake-up). Other AiEvent
            // variants (hover) are no-ops — the wait-event CLI handles them.
            Ok(event) = main_ai_event_rx.recv() => {
                inject_on_click_prompt(&mut state, &event);
            }
            // WebSocket client commands (draw, input, resize)
            Some(cmd) = ws_cmd_rx.recv() => {
                // SubscribeRaw needs pty_tx access — handle it inline
                if let crate::websocket::WsCommand::GetControlState(reply) = cmd {
                    let claude_state = crate::websocket::build_control_state(&state.claude);
                    let _ = reply.send(crate::websocket::ControlStateReply {
                        session_name: state.name.clone(),
                        window_id: state.window_id.clone(),
                        display_name: state.title.clone(),
                        claude: claude_state,
                        last_user_prompt: state.last_user_prompt.clone(),
                    });
                } else if let crate::websocket::WsCommand::ScrollRequest { offset, count, reply } = cmd {
                    let rows = state.terminal.scrollback.range(offset, count);
                    let rows_json = serde_json::to_string(&rows).unwrap_or_default();
                    let total = state.terminal.scrollback.len();
                    let _ = reply.send(crate::websocket::ScrollbackReply {
                        rows_json,
                        offset,
                        total,
                    });
                } else if let crate::websocket::WsCommand::SubscribeRaw { reply, full_snapshot } = cmd {
                    let scrollback_total = state.terminal.scrollback.len();
                    // Send full snapshot (with scrollback) when:
                    // 1. After a deferred restore (pending_full_snapshot flag)
                    // 2. Client explicitly requests it (control→raw upgrade on session switch)
                    let send_full = state.pending_full_snapshot || full_snapshot;
                    let snapshot_json = if send_full {
                        state.pending_full_snapshot = false;
                        info!("Sending full snapshot with {} scrollback rows (reason: {})", scrollback_total,
                            if full_snapshot { "client-requested" } else { "post-restore" });
                        serde_json::to_string(&state.terminal.snapshot()).unwrap_or_default()
                    } else {
                        serde_json::to_string(&state.terminal.snapshot_viewport_only()).unwrap_or_default()
                    };
                    let claude = crate::websocket::build_control_state(&state.claude);
                    let alt_screen_active = state.terminal.modes.alternate_screen;
                    let _ = reply.send(crate::websocket::SubscribeRawReply {
                        snapshot_json,
                        session: state.name.clone(),
                        theme: state.status_bar.theme.clone(),
                        project: state.status_bar.project.clone(),
                        cols: state.terminal.cols(),
                        rows: state.terminal.rows(),
                        claude,
                        pty_rx: pty_filtered_tx.subscribe(),
                        scrollback_total,
                    });
                    // Kick the PTY child to redraw after the broadcast subscription is live.
                    // Reattach scenario: WebView reloaded, daemon's grid may be stale or empty
                    // (TUI mid-frame at snapshot time) and no SIGWINCH fires on plain reattach,
                    // so an alt-screen TUI (claude code, vim, etc.) won't redraw on its own and
                    // the client sees a black viewport. Send SIGWINCH only when alt-screen mode
                    // is active — plain shells don't need it. The redraw bytes flow through
                    // pty_filtered_tx into the just-subscribed client.
                    if alt_screen_active && let Err(e) = state.pty.signal(signal::Signal::SIGWINCH) {
                        warn!("SIGWINCH on subscribe_raw failed: {}", e);
                    }
                    // Replay current Workshops to the freshly-subscribed client.
                    // Without this, after a webview reload the daemon's
                    // workshops are still alive but the client has no record
                    // of them — clicks fail, sidebar list is empty, badges
                    // miss. Re-broadcasting open events under the existing
                    // workshop_tx channel makes the new subscriber catch up.
                    for workshop in state.workshops.values() {
                        let envelope = serde_json::json!({
                            "event": "open",
                            "name": workshop.name,
                            "html": workshop.html,
                            "css": workshop.css,
                        }).to_string();
                        let _ = workshop_tx.send(Arc::new(envelope));
                    }
                } else if let crate::websocket::WsCommand::RegisterChannel { immorterm_id, channel_tx: tx } = cmd {
                    info!("Channel server registered for session {}", immorterm_id);
                    channel_tx = Some(tx);
                    // Start inbox watcher if not already running
                    if channel_inbox_rx.is_none() {
                        channel_inbox_rx = Some(crate::channel_registry::start_inbox_watcher(&immorterm_id));
                    }
                } else if let crate::websocket::WsCommand::PairSessions { source_id, target_id, source_name, target_name } = cmd {
                    info!("Pairing sessions: {} ({}) <-> {} ({})", source_name, source_id, target_name, target_id);
                    // Notify the local channel server about the pairing
                    if let Some(ref tx) = channel_tx {
                        let paired_msg = serde_json::json!({
                            "type": "session_paired",
                            "partner_id": target_id,
                            "partner_name": target_name,
                        });
                        let _ = tx.try_send(paired_msg.to_string());
                    }
                    // Also notify the target session via inbox
                    let inbox_dir = {
                        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                        std::path::PathBuf::from(home).join(".immorterm").join("channel-inbox")
                    };
                    let pair_msg = crate::channel_registry::ChannelMessage {
                        from_immorterm_id: source_id.clone(),
                        from_name: source_name.clone(),
                        message: format!("__pair__:{}:{}", source_id, source_name),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                    };
                    let _ = crate::channel_registry::write_to_inbox(&inbox_dir, &target_id, &pair_msg);
                    // Create chat overlay on the terminal
                    let mut overlay = crate::chat_overlay::ChatOverlay::new(&target_name);
                    overlay.render(&mut state.terminal.ai_layer);
                    chat_overlay = Some(overlay);
                    channel_partner_id = Some(target_id.clone());
                } else if let crate::websocket::WsCommand::CloseWorkshop { name } = cmd {
                    // Webview-initiated close (X button on tab or context menu).
                    // Mirrors the MCP CloseWorkshop IPC: remove from state,
                    // delete the persisted HTML file, broadcast a close envelope
                    // so every webview drops its DOM copy. Idempotent.
                    state.workshops.remove(&name);
                    let _ = std::fs::remove_file(workshop_html_path(&state.name, &name));
                    let envelope = serde_json::json!({
                        "event": "close",
                        "name": name,
                    }).to_string();
                    let _ = workshop_tx.send(Arc::new(envelope));
                } else if let crate::websocket::WsCommand::UnpairSessions = cmd {
                    info!("Unpairing sessions");
                    if let Some(ref tx) = channel_tx {
                        let unpaired_msg = serde_json::json!({
                            "type": "session_unpaired",
                        });
                        let _ = tx.try_send(unpaired_msg.to_string());
                    }
                    // Remove chat overlay
                    if let Some(ref mut overlay) = chat_overlay {
                        overlay.remove(&mut state.terminal.ai_layer);
                    }
                    chat_overlay = None;
                } else {
                    handle_ws_command(cmd, &mut state);
                }
            }
            // Channel inbox: forward incoming messages to registered channel server
            inbox_msg = async {
                match channel_inbox_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(msg) = inbox_msg {
                    // Check for pairing control messages
                    if msg.message.starts_with("__pair__:") {
                        if let Some(rest) = msg.message.strip_prefix("__pair__:")
                            && let Some((partner_id, partner_name)) = rest.split_once(':')
                        {
                            if let Some(ref tx) = channel_tx {
                                let paired = serde_json::json!({
                                    "type": "session_paired",
                                    "partner_id": partner_id,
                                    "partner_name": partner_name,
                                });
                                let _ = tx.try_send(paired.to_string());
                            }
                            // Create chat overlay for the receiving side
                            let mut overlay = crate::chat_overlay::ChatOverlay::new(partner_name);
                            overlay.render(&mut state.terminal.ai_layer);
                            chat_overlay = Some(overlay);
                            channel_partner_id = Some(partner_id.to_string());
                        }
                    } else if msg.message == "__unpair__" {
                        // Partner disconnected — tear down the channel
                        info!("Partner session disconnected — unpairing");
                        if let Some(ref tx) = channel_tx {
                            let unpaired = serde_json::json!({
                                "type": "session_unpaired",
                            });
                            let _ = tx.try_send(unpaired.to_string());
                        }
                        if let Some(ref mut overlay) = chat_overlay {
                            overlay.remove(&mut state.terminal.ai_layer);
                        }
                        chat_overlay = None;
                        channel_partner_id = None;
                    } else {
                        // Forward regular channel message to the channel server
                        if let Some(ref tx) = channel_tx {
                            let fwd = serde_json::json!({
                                "type": "channel_message",
                                "from_immorterm_id": msg.from_immorterm_id,
                                "from_name": msg.from_name,
                                "message": msg.message,
                            });
                            let _ = tx.try_send(fwd.to_string());
                        }
                        // Update chat overlay with the incoming message
                        if let Some(ref mut overlay) = chat_overlay {
                            overlay.add_message(
                                &msg.from_name,
                                &msg.message,
                                false, // incoming = not local
                                &mut state.terminal.ai_layer,
                            );
                        }
                    }
                }
            }
            // Claude process scan (every 10 seconds)
            _ = claude_interval.tick() => {
                let shell_pid = state.pty.child_pid();
                let changed = state.claude.scan(shell_pid);

                // ── Claude session-id backfill (non-OSC path) ──
                // When Claude is running but state.claude.session_id is still None,
                // OSC 1337 was never emitted for this session. Fall back to the
                // SessionStart-hook-written env file in ~/.immorterm/claude-env/,
                // whose filename IS the Claude UUID and content carries
                // IMMORTERM_ID=<wid>. Updates registry + session.json so the
                // resolver's fast path has correct data on next restore.
                if state.claude.claude_pid.is_some()
                    && state.claude.session_id.is_none()
                    && !state.window_id.is_empty()
                    && let Some(uuid) = crate::registry::resolve_claude_uuid_via_env(&state.window_id)
                {
                    info!("Claude session via env backfill: {}", &uuid[..8.min(uuid.len())]);
                    state.claude.session_id = Some(uuid.clone());
                    state.claude.had_ai_session = true;
                    let mut registry = crate::registry::Registry::load();
                    registry.update_claude_session(&state.window_id, &uuid);
                    if let Err(e) = registry.save() {
                        warn!("Failed to persist backfilled claude_session_id: {}", e);
                    }
                    // Also refresh session.json inside the structured log dir so
                    // the restore resolver's Tier 2 path stays in sync.
                    if let Some(entry) = registry.sessions.iter().find(|e| e.window_id == state.window_id).cloned() {
                        crate::registry::write_session_json(&entry);
                    }
                }

                // Always update status bar when Claude is running
                state.status_bar.ai_process_stats = state.claude.format_process_stats();
                state.status_bar.ai_api_stats = state.claude.format_api_stats();

                // Notify structured logger of AI tool state changes
                if changed
                    && let Some(ref mut slog) = state.structured_log {
                        let tool = state.claude.detected_tool.map(|t| {
                            match t.name() {
                                "claude" => structured_logs::AiTool::Claude,
                                "aider" => structured_logs::AiTool::Aider,
                                "cursor" => structured_logs::AiTool::Cursor,
                                "copilot" => structured_logs::AiTool::Copilot,
                                "codex" => structured_logs::AiTool::Codex,
                                "windsurf" => structured_logs::AiTool::Windsurf,
                                "cline" => structured_logs::AiTool::Cline,
                                "opencode" => structured_logs::AiTool::Opencode,
                                "gemini" => structured_logs::AiTool::Gemini,
                                "continue" => structured_logs::AiTool::Continue,
                                "cody" => structured_logs::AiTool::Cody,
                                _ => structured_logs::AiTool::Unknown,
                            }
                        });
                        slog.on_ai_state_change(
                            tool,
                            state.claude.claude_pid,
                            None,
                            None,
                        );
                    }

                // ── Channel cleanup on Claude exit ──
                // When Claude exits, notify the paired session and tear down the overlay.
                if changed && state.claude.claude_pid.is_none() {
                    if let Some(ref tx) = channel_tx {
                        let unpaired_msg = serde_json::json!({
                            "type": "session_unpaired",
                        });
                        let _ = tx.try_send(unpaired_msg.to_string());
                    }
                    // Notify the partner daemon via inbox
                    if let Some(ref partner_id) = channel_partner_id {
                        let inbox_dir = {
                            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                            std::path::PathBuf::from(home).join(".immorterm").join("channel-inbox")
                        };
                        let unpair_msg = crate::channel_registry::ChannelMessage {
                            from_immorterm_id: state.window_id.clone(),
                            from_name: state.name.clone(),
                            message: "__unpair__".to_string(),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                        };
                        let _ = crate::channel_registry::write_to_inbox(&inbox_dir, partner_id, &unpair_msg);
                    }
                    if let Some(ref mut overlay) = chat_overlay {
                        overlay.remove(&mut state.terminal.ai_layer);
                    }
                    chat_overlay = None;
                    channel_tx = None;
                    channel_partner_id = None;
                }

                // ── Self-healing: re-register if our entry was lost ──
                // Runs every 10s regardless of Claude state changes.
                // Race condition (last-writer-wins) can silently drop our entry
                // when multiple daemons do concurrent load → modify → save cycles.
                if !state.window_id.is_empty() {
                    let mut registry = crate::registry::Registry::load();
                    let needs_heal = registry.find_by_pid(std::process::id()).is_none();

                    if needs_heal {
                        warn!(
                            "Self-healing: daemon PID {} missing from registry — re-registering (window_id={})",
                            std::process::id(),
                            state.window_id
                        );

                        // Try to recover from backup first — preserves display_name,
                        // theme, title_locked, and other extension-managed metadata
                        let entry = match crate::registry::recover_entry_from_backup(
                            std::process::id(),
                            &state.window_id,
                        ) {
                            Some(mut recovered) => {
                                // Update live fields that may have changed since backup
                                recovered.pid = std::process::id();
                                recovered.ws_port = if state.ws_port > 0 { Some(state.ws_port) } else { None };
                                recovered.claude_session_id = state.claude.session_id.clone();
                                if !state.title.is_empty() {
                                    recovered.title = state.title.clone();
                                }
                                info!("Self-healing: recovered entry from backup (display_name={})", recovered.display_name);
                                recovered
                            }
                            None => {
                                // No backup found — rebuild from live state (best effort)
                                warn!("Self-healing: no backup entry found, rebuilding from live state");
                                let project_dir = std::env::var("SCREEN_PROJECT_DIR").unwrap_or_default();
                                let owner = crate::registry::resolve_owner_project(&project_dir);
                                let owner_identity = crate::registry::read_or_create_project(&owner.owner_dir);
                                let owner_project_id = owner_identity.as_ref().map(|p| p.id.clone());
                                let owner_project_name = owner_identity.as_ref().map(|p| p.name.clone());
                                crate::registry::RegistryEntry {
                                    pid: std::process::id(),
                                    name: state.name.clone(),
                                    window_id: state.window_id.clone(),
                                    display_name: state.name.clone(),
                                    project_dir,
                                    claude_session_id: state.claude.session_id.clone(),
                                    title_locked: false,
                                    title: state.title.clone(),
                                    logfile: state.log_file.clone(),
                                    shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into()),
                                    created_at: DAEMON_START
                                        .get()
                                        .map(|t| {
                                            let boot = std::time::SystemTime::now() - t.elapsed();
                                            boot.duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs()
                                        })
                                        .unwrap_or(0),
                                    session_type: Some("ai".to_string()),
                                    ws_port: if state.ws_port > 0 { Some(state.ws_port) } else { None },
                                    theme: None,
                                    claude_transcript_path: None,
                                    claude_stats: None,
                                    tool: None,
                                    tool_history: Vec::new(),
                                    session_status: None,
                                    shelved_at: None,
                                    structured_log_dir: Some(
                                        state.structured_log_dir.to_string_lossy().into_owned(),
                                    ),
                                    needs_attention: false,
                                    is_working: false,
                                    owner_project_dir: if owner.owner_dir.is_empty() { None } else { Some(owner.owner_dir) },
                                    owner_project_id,
                                    owner_project_name,
                                    worktree: owner.worktree,
                                }
                            }
                        };
                        registry.register(entry);
                    }

                    // Update Claude stats on state change
                    if changed {
                        if let Some(ref sid) = state.claude.session_id {
                            registry.update_claude_session(&state.window_id, sid);
                        }
                        registry.update_claude_stats(&state.window_id, &state.claude);
                    }

                    // ── Worktree tracking: react to OSC-7 cwd changes ──
                    // When Claude (or the user) cd's between trunk and a worktree
                    // of the same project, update the entry's `worktree` field.
                    // Skipped when cwd lands outside our owner_project_dir's git
                    // tree — that's transient navigation (e.g. cd /tmp), not a
                    // project-membership change.
                    let mut worktree_changed = false;
                    let current_cwd = state.terminal.cwd.clone();
                    if current_cwd != last_known_cwd && !current_cwd.is_empty() {
                        last_known_cwd = current_cwd.clone();
                        // Find our own entry to learn our owner_project_dir.
                        let our_entry = registry.sessions.iter()
                            .find(|e| e.window_id == state.window_id)
                            .cloned();
                        if let Some(our) = our_entry
                            && let Some(ref our_owner) = our.owner_project_dir
                        {
                            let resolved = crate::registry::resolve_owner_project(&current_cwd);
                            // Only act when the new cwd resolves into OUR project's tree.
                            if resolved.owner_dir == *our_owner
                                && our.worktree != resolved.worktree
                                && let Some(idx) = registry.sessions.iter()
                                    .position(|e| e.window_id == state.window_id)
                            {
                                registry.sessions[idx].worktree = resolved.worktree.clone();
                                worktree_changed = true;
                                info!(
                                    "Worktree updated for {}: {:?}",
                                    state.window_id, resolved.worktree
                                );
                            }
                        }
                    }

                    // Only save if something changed
                    if (needs_heal || changed || worktree_changed) && let Err(e) = registry.save() {
                        error!("Failed to save registry: {}", e);
                    }
                }

                // ── Branch tracking: derive from state.terminal.cwd ──
                // Refresh the cached branch from the session's live cwd each
                // tick. If it changed, fire a control event so the webview
                // (in both VS Code extension and Tauri standalone hosts) can
                // update its per-session branch state and re-render the
                // status-bar projectName label if the active tab matches.
                let new_branch = crate::registry::read_branch_for_cwd(&state.terminal.cwd);
                let branch_changed = state.branch != new_branch;
                if branch_changed {
                    info!(
                        "Branch for session {} → {:?} (cwd: {})",
                        state.window_id, new_branch, state.terminal.cwd
                    );
                    state.branch = new_branch.clone();
                }

                // Fire control event for WebSocket subscribers when Claude
                // state OR the session's branch changed.
                if changed || branch_changed {
                    let evt_type = if changed {
                        if state.claude.claude_pid.is_some() { "claude_update" } else { "claude_exited" }
                    } else {
                        "branch_update"
                    };
                    let event = crate::websocket::build_control_event_with_branch(
                        evt_type,
                        &state.claude,
                        state.branch.as_deref(),
                    );
                    let _ = control_tx.send(Arc::new(event));
                }
            }
            // Structured logging: periodic snapshot (every 30s)
            _ = snapshot_interval.tick() => {
                if let Some(ref mut slog) = state.structured_log {
                    slog.on_periodic_tick(&state.terminal);
                    // Check for new user prompt (for status bar tooltip)
                    if let Some(prompt) = slog.last_user_prompt()
                        && state.last_user_prompt.as_deref() != Some(prompt) {
                            state.last_user_prompt = Some(prompt.to_string());
                            let event = crate::websocket::build_control_event_with_prompt(
                                "user_prompt",
                                &state.claude,
                                state.last_user_prompt.as_deref(),
                            );
                            let _ = control_tx.send(Arc::new(event));
                        }
                }
            }
            // Team auto-launch: detect new teams and spawn GPU team-view window
            team_change = async {
                match team_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Ok(change) = team_change
                    && !auto_launched_teams.contains(&change.team_name) {
                        auto_launched_teams.insert(change.team_name.clone());
                        info!("New team detected: '{}' — auto-launching team-view", change.team_name);

                        let exe = std::env::current_exe()
                            .unwrap_or_else(|_| PathBuf::from("immorterm-ai"));
                        match std::process::Command::new(&exe)
                            .arg("team-view")
                            .arg(&change.team_name)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                        {
                            Ok(child) => {
                                info!(
                                    "Auto-launched team-view for '{}' (PID {})",
                                    change.team_name,
                                    child.id()
                                );
                            }
                            Err(e) => {
                                error!(
                                    "Failed to auto-launch team-view for '{}': {}",
                                    change.team_name, e
                                );
                            }
                        }
                    }
            }
            // Graceful SIGTERM/SIGINT handling
            signal = signals.next() => {
                if let Some(sig) = signal {
                    // SIGCHLD: reap exited direct children so they don't pile up as
                    // <defunct>. signal_hook coalesces SIGCHLD (one wakeup can cover
                    // many exits), so drain in a loop. Catch-all for any child the
                    // daemon forks and never waits on (session-startup hook burst,
                    // auto-launched team-view, etc.).
                    // ponytail: one generic waitpid(-1) arm subsumes every spawn site.
                    // The only in-daemon std waiter is mcp::get_stable_project_id's
                    // `git config` .output(), already blocked on its own pid at exit
                    // (and it has fallbacks if it ever loses the race).
                    if sig == signal_hook::consts::SIGCHLD {
                        loop {
                            match nix::sys::wait::waitpid(
                                Pid::from_raw(-1),
                                Some(nix::sys::wait::WaitPidFlag::WNOHANG),
                            ) {
                                Ok(nix::sys::wait::WaitStatus::StillAlive) => break, // children remain, none ready
                                Ok(_) => continue,                                   // reaped one — keep draining
                                Err(nix::errno::Errno::ECHILD) => break,             // no children at all
                                Err(nix::errno::Errno::EINTR) => continue,           // interrupted — retry
                                Err(_) => break,                                     // unexpected — stop
                            }
                        }
                        continue; // SIGCHLD is not a shutdown — back to select!
                    }
                    let sig_name = match sig {
                        signal_hook::consts::SIGTERM => "SIGTERM",
                        signal_hook::consts::SIGINT => "SIGINT",
                        _ => "unknown",
                    };
                    info!("Received {} — shutting down gracefully", sig_name);

                    // Write death event
                    write_death_event(&state.structured_log_dir, &format!("signal:{}", sig_name), &session_name);

                    // Flush structured logs
                    if let Some(ref mut slog) = state.structured_log {
                        slog.on_shutdown(&state.terminal);
                    }

                    // Notify WS subscribers
                    let exit_event = crate::websocket::build_control_event("session_closing", &state.claude);
                    let _ = control_tx.send(Arc::new(exit_event));

                    // DON'T deregister from registry on signal — the dead entry is
                    // the restore state. The extension's restoreSessions() detects
                    // dead PIDs and respawns fresh daemons. Deregistering here is
                    // what caused sessions to vanish after VS Code reload.
                    // Only clean up socket + .ws files (stale IPC artifacts).
                    fs::remove_file(&socket_path).ok();
                    let ws_file = socket_dir().join(format!("{}.{}.ws", std::process::id(), session_name));
                    fs::remove_file(&ws_file).ok();
                    std::process::exit(0);
                }
            }
            // PTY process exit
            _ = &mut pty_read_handle => {
                info!("Session '{}' ended (shell exited)", session_name);

                // Write death event
                write_death_event(&state.structured_log_dir, "pty_exit", &session_name);

                // Structured logging: final flush
                if let Some(ref mut slog) = state.structured_log {
                    slog.on_shutdown(&state.terminal);
                }

                // Notify subscribers before exiting
                let exit_event = crate::websocket::build_control_event("session_closing", &state.claude);
                let _ = control_tx.send(Arc::new(exit_event));

                // Deregister from session registry
                crate::registry::deregister_session();
                // Clean up socket and WebSocket port file
                fs::remove_file(&socket_path).ok();
                let ws_file = socket_dir().join(format!("{}.{}.ws", std::process::id(), session_name));
                fs::remove_file(&ws_file).ok();
                // Reap child
                let _ = nix::sys::wait::waitpid(
                    Pid::from_raw(pty_child as i32),
                    Some(nix::sys::wait::WaitPidFlag::WNOHANG),
                );
                std::process::exit(0);
            }
        }
    }
}

/// Safely serialize and send a response. Never panics.
async fn send_response(stream: &mut tokio::net::UnixStream, resp: &Response) {
    match serde_json::to_string(resp) {
        Ok(json) => { let _ = stream.write_all(json.as_bytes()).await; }
        Err(e) => {
            error!("Failed to serialize response: {}", e);
            let fallback = format!(r#"{{"type":"Error","data":"Serialization error: {}"}}"#, e);
            let _ = stream.write_all(fallback.as_bytes()).await;
        }
    }
}

/// Convert a grid Row to a trimmed text string.
fn row_to_text(row: &immorterm_core::Row) -> String {
    let line: String = row.cells.iter()
        .filter(|c| !c.is_wide_continuation())
        .map(|c| c.grapheme)
        .collect();
    line.trim_end().to_string()
}

/// Convert the terminal's visible grid to text lines.
fn grid_to_text(terminal: &immorterm_core::Terminal) -> Vec<String> {
    let mut lines = Vec::with_capacity(terminal.rows());
    for i in 0..terminal.rows() {
        if let Some(row) = terminal.grid.row(i) {
            lines.push(row_to_text(row));
        }
    }
    lines
}

/// Handle a single client connection (command or attach).
#[allow(clippy::too_many_arguments)]
async fn handle_client_connection(
    mut stream: tokio::net::UnixStream,
    state: &mut SessionState,
    pty_tx: &broadcast::Sender<Vec<u8>>,
    pty_filtered_tx: &broadcast::Sender<Vec<u8>>,
    ai_layer_tx: &broadcast::Sender<Arc<Vec<u8>>>,
    socket_path: &PathBuf,
    control_tx: &broadcast::Sender<Arc<String>>,
    ai_event_tx: &broadcast::Sender<immorterm_core::ai_layer::AiEvent>,
    ai_eval_tx: &broadcast::Sender<Arc<String>>,
    workshop_tx: &broadcast::Sender<Arc<String>>,
    channel_partner_id: &Option<String>,
    chat_overlay: &mut Option<crate::chat_overlay::ChatOverlay>,
) {
    // Read the request
    let mut buf = vec![0u8; 65536];
    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request: Request = match serde_json::from_slice(&buf[..n]) {
        Ok(req) => req,
        Err(e) => {
            error!("Invalid request: {}", e);
            let resp = Response::Error(format!("Invalid request: {}", e));
            send_response(&mut stream, &resp).await;
            return;
        }
    };

    match request {
        Request::Ping => {
            let resp = Response::Ok("pong".into());
            send_response(&mut stream, &resp).await;
        }
        Request::GetInfo => {
            let resp = Response::SessionInfo {
                name: state.name.clone(),
                pid: std::process::id(),
                attached: state.attached,
                title: state.title.clone(),
                cols: state.terminal.cols(),
                rows: state.terminal.rows(),
            };
            send_response(&mut stream, &resp).await;
        }
        Request::Execute { command, args } => {
            // Intercept "notify" before dispatching — needs control_tx access.
            // State machine: attention and working are mutually exclusive.
            //   - attention → set needs_attention=true, clear is_working (Claude paused, waiting for user;
            //                 Claude Code's Notification:permission_prompt fires here but Stop does NOT,
            //                 so without clearing is_working the sidebar would pulse forever during permission waits)
            //   - working   → set is_working=true, clear needs_attention (Claude resumed work)
            //   - idle      → set is_working=false (turn ended cleanly)
            if command == "notify" && args.first().map(|s| s.as_str()) == Some("attention") {
                info!("Attention notification received via IPC");
                let ctrl = serde_json::json!({
                    "type": "control_event",
                    "event": "attention",
                }).to_string();
                let _ = control_tx.send(Arc::new(ctrl));
                state.needs_attention = true;
                state.is_working = false;
                let mut registry = crate::registry::Registry::load();
                if let Some(entry) = registry.sessions.iter_mut().find(|e| e.pid == std::process::id()) {
                    entry.needs_attention = true;
                    entry.is_working = false;
                    if let Err(e) = registry.save() {
                        warn!("Failed to save attention to registry: {}", e);
                    }
                }
                // Also broadcast idle so the breathing dot stops — attention implies not-working.
                let idle_ctrl = serde_json::json!({
                    "type": "control_event",
                    "event": "idle",
                }).to_string();
                let _ = control_tx.send(Arc::new(idle_ctrl));
                send_response(&mut stream, &Response::Ok(String::new())).await;
            } else if command == "notify"
                && matches!(args.first().map(|s| s.as_str()), Some("working") | Some("idle"))
            {
                let working = args.first().map(|s| s.as_str()) == Some("working");
                let event_name = if working { "working" } else { "idle" };
                info!("{} notification received via IPC", event_name);
                state.is_working = working;
                let ctrl = serde_json::json!({
                    "type": "control_event",
                    "event": event_name,
                }).to_string();
                let _ = control_tx.send(Arc::new(ctrl));

                // working clears stale attention — Claude is back at work
                let clear_attention = working && state.needs_attention;
                if clear_attention {
                    state.needs_attention = false;
                }

                let mut registry = crate::registry::Registry::load();
                if let Some(entry) = registry.sessions.iter_mut().find(|e| e.pid == std::process::id()) {
                    entry.is_working = working;
                    if clear_attention {
                        entry.needs_attention = false;
                    }
                    if let Err(e) = registry.save() {
                        warn!("Failed to save is_working to registry: {}", e);
                    }
                }
                send_response(&mut stream, &Response::Ok(String::new())).await;
            } else {
                let result = execute_command(state, &command, &args);
                let resp = match result {
                    Ok(output) => Response::Ok(output),
                    Err(e) => Response::Error(e.to_string()),
                };
                send_response(&mut stream, &resp).await;
            }
        }
        Request::Query { command } => {
            let result = query_command(state, &command);
            let resp = Response::Ok(result);
            send_response(&mut stream, &resp).await;
        }
        Request::Attach { cols, rows } => {
            // Resize terminal
            if cols > 0 && rows > 0 {
                state.pty.resize(cols, rows);
                state.terminal.resize(cols as usize, rows as usize);
                state.process_deferred_restore(cols, rows);
                if let Some(ref mut slog) = state.structured_log {
                    slog.on_resize(cols as usize, rows as usize);
                }
            }
            state.attached = true;
            set_socket_permissions(socket_path, true);

            // Send OK to confirm attach
            let resp = Response::Ok("attached".into());
            send_response(&mut stream, &resp).await;

            // Now relay data bidirectionally (filtered — no <<html>> markers)
            let mut pty_rx = pty_filtered_tx.subscribe();
            let (reader, mut writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);

            // Relay client input → PTY
            let pty_writer = state.pty.writer_clone();
            let input_handle = tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Some(ref pty_w) = pty_writer {
                                use std::io::Write;
                                let mut w = pty_w.lock().unwrap();
                                if w.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                                let _ = w.flush();
                            }
                        }
                        Err(_) => break,
                    }
                }
            });

            // Relay PTY output → client
            let output_handle = tokio::spawn(async move {
                loop {
                    match pty_rx.recv().await {
                        Ok(data) => {
                            if writer.write_all(&data).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    }
                }
            });

            // Wait for either side to close
            tokio::select! {
                _ = input_handle => {},
                _ = output_handle => {},
            }

            state.attached = false;
            set_socket_permissions(socket_path, false);
        }
        Request::Detach => {
            state.attached = false;
            set_socket_permissions(socket_path, false);
            let resp = Response::Ok("detached".into());
            send_response(&mut stream, &resp).await;
        }
        Request::Resize { cols, rows } => {
            state.pty.resize(cols, rows);
            state.terminal.resize(cols as usize, rows as usize);
            state.process_deferred_restore(cols, rows);
            if let Some(ref mut slog) = state.structured_log {
                slog.on_resize(cols as usize, rows as usize);
            }
            let resp = Response::Ok("resized".into());
            send_response(&mut stream, &resp).await;
        }
        Request::Kill => {
            let resp = Response::Ok("killing".into());
            send_response(&mut stream, &resp).await;
            // Write death event
            write_death_event(&state.structured_log_dir, "kill_request", &state.name);
            // Flush structured logs before exit
            if let Some(ref mut slog) = state.structured_log {
                slog.on_shutdown(&state.terminal);
            }
            // Deregister from session registry
            crate::registry::deregister_session();
            // Clean up socket and WebSocket port file
            fs::remove_file(socket_path).ok();
            let ws_file = socket_dir().join(format!(
                "{}.{}.ws",
                std::process::id(),
                state.name,
            ));
            fs::remove_file(&ws_file).ok();
            std::process::exit(0);
        }
        Request::ReadScreen => {
            let lines = grid_to_text(&state.terminal);
            let resp = Response::ScreenContent {
                lines,
                cursor_row: state.terminal.cursor.row,
                cursor_col: state.terminal.cursor.col,
                cursor_visible: state.terminal.cursor.visible,
                cols: state.terminal.cols(),
                rows: state.terminal.rows(),
                title: state.terminal.title.clone(),
            };
            send_response(&mut stream, &resp).await;
        }
        Request::ReadScrollback { lines, pattern } => {
            let sb = &state.terminal.scrollback;
            let total = sb.len();
            let limit = lines.min(total);
            let start = total.saturating_sub(limit);

            let mut result = Vec::new();
            for i in start..total {
                if let Some(row) = sb.get(i) {
                    let line = row_to_text(row);
                    if let Some(ref pat) = pattern {
                        if line.to_lowercase().contains(&pat.to_lowercase()) {
                            result.push(line);
                        }
                    } else {
                        result.push(line);
                    }
                }
            }

            let resp = Response::ScrollbackContent {
                lines: result,
                total_lines: total,
            };
            send_response(&mut stream, &resp).await;
        }
        Request::GetStatusBar => {
            let sb = &state.status_bar;
            let secs_ago = sb.last_activity.elapsed().as_secs();
            let activity = if secs_ago < 60 {
                format!("{}s ago", secs_ago)
            } else if secs_ago < 3600 {
                format!("{}m ago", secs_ago / 60)
            } else {
                format!("{}h ago", secs_ago / 3600)
            };
            let stats = match sb.stats_mode {
                0 => &sb.ai_process_stats,
                1 => &sb.ai_api_stats,
                _ => &sb.ai_process_stats, // TODO: combine both
            };
            // Return as JSON string in Ok variant
            let json = serde_json::json!({
                "project": sb.project,
                "title": state.title,
                "stats": stats,
                "stats_mode": sb.stats_mode,
                "last_activity": activity,
                "theme": sb.theme,
            });
            let resp = Response::Ok(json.to_string());
            send_response(&mut stream, &resp).await;
        }
        Request::GetClaudeInfo => {
            let api = &state.claude.api_stats;
            let resp = Response::ClaudeInfo {
                claude_pid: state.claude.claude_pid,
                session_id: state.claude.session_id.clone(),
                rss_kb: state.claude.rss_kb,
                cpu_percent: state.claude.cpu_percent,
                runtime_secs: state.claude.runtime_secs(),
                active: state.claude.claude_pid.is_some() || !state.claude.api_stats.model.is_empty(),
                model: if api.model.is_empty() { None } else { Some(api.model.clone()) },
                cost_usd: if api.cost_usd > 0.0 { Some(api.cost_usd) } else { None },
                context_pct: if api.context_pct > 0.0 { Some(api.context_pct) } else { None },
                transcript_path: if api.transcript_path.is_empty() { None } else { Some(api.transcript_path.clone()) },
                permission_mode: state.claude.permission_mode.clone(),
                tool: state.claude.detected_tool.map(|t| t.name().to_string()),
            };
            send_response(&mut stream, &resp).await;
        }
        Request::UpdateClaudeSession {
            session_id,
            model,
            cost_usd,
            context_pct,
            transcript_path,
            permission_mode,
        } => {
            // Event-driven Claude session push from statusline.sh.
            // This is the primary path — no polling needed.
            let old_session = state.claude.session_id.clone();
            state.claude.session_id = Some(session_id.clone());
            state.claude.had_ai_session = true;
            state.claude.api_stats = crate::claude::ClaudeApiStats {
                model,
                cost_usd,
                context_pct,
                transcript_path,
            };

            // Update permission mode if provided (None = unchanged)
            if let Some(mode) = permission_mode {
                state.claude.permission_mode = Some(mode);
            }

            // Update status bar immediately
            state.status_bar.ai_api_stats = state.claude.format_api_stats();

            // Update registry with session + stats
            if !state.window_id.is_empty() {
                let mut registry = crate::registry::Registry::load();
                if old_session.as_deref() != Some(&session_id) {
                    info!("Claude session pushed via IPC: {}", &session_id[..8.min(session_id.len())]);
                    registry.update_claude_session(&state.window_id, &session_id);
                }
                registry.update_claude_stats(&state.window_id, &state.claude);
                if let Err(e) = registry.save() {
                    error!("Failed to update registry with Claude stats: {}", e);
                }
            }

            // Fire control event for WebSocket subscribers
            let ctrl_event = crate::websocket::build_control_event("claude_update", &state.claude);
            let _ = control_tx.send(Arc::new(ctrl_event));

            let resp = Response::Ok("updated".into());
            send_response(&mut stream, &resp).await;
        }
        // ─── AI Canvas Layer handlers ─────────────────────────────
        Request::DrawRect { x, y, width, height, color, border_color, border_width, anchor, anchor_to, name } => {
            let anchor_mode = parse_anchor_mode(&anchor, &anchor_to, &state.terminal);
            let id = state.terminal.ai_layer.add_rect(immorterm_core::ai_layer::AiRect {
                x, y, width, height, color,
                border_color,
                border_width: border_width.unwrap_or(0.0),
            }, anchor_mode, name);
            let resp = Response::PrimitiveId { id };
            send_response(&mut stream, &resp).await;
        }
        Request::DrawText { text, x, y, color, font_size_scale, anchor, anchor_to, name } => {
            let anchor_mode = parse_anchor_mode(&anchor, &anchor_to, &state.terminal);
            let id = state.terminal.ai_layer.add_text(immorterm_core::ai_layer::AiText {
                text, x, y, color,
                font_size_scale: font_size_scale.unwrap_or(1.0),
            }, anchor_mode, name);
            let resp = Response::PrimitiveId { id };
            send_response(&mut stream, &resp).await;
        }
        Request::DrawButton { text, x, y, width, height, bg_color, text_color, anchor, anchor_to, name } => {
            let anchor_mode = parse_anchor_mode(&anchor, &anchor_to, &state.terminal);
            let id = state.terminal.ai_layer.add_button(immorterm_core::ai_layer::AiButton {
                text, x, y, width, height, bg_color, text_color,
                hovered: false,
            }, anchor_mode, name);
            let resp = Response::PrimitiveId { id };
            send_response(&mut stream, &resp).await;
        }
        Request::DrawLine { x1, y1, x2, y2, color, thickness, anchor, anchor_to, name } => {
            let anchor_mode = parse_anchor_mode(&anchor, &anchor_to, &state.terminal);
            let id = state.terminal.ai_layer.add_line(immorterm_core::ai_layer::AiLine {
                x1, y1, x2, y2, color,
                thickness: thickness.unwrap_or(2.0),
            }, anchor_mode, name);
            let resp = Response::PrimitiveId { id };
            send_response(&mut stream, &resp).await;
        }
        Request::DrawHtml { html, css, x, y, width, height, anchor, anchor_to, name, on_click_prompt, on_click_inject_context } => {
            let anchor_mode = parse_anchor_mode(&anchor, &anchor_to, &state.terminal);
            let id = state.terminal.ai_layer.add_html(immorterm_core::ai_layer::AiHtml {
                html, css, x, y, width, height, anchor_row: None, on_click_prompt, on_click_inject_context,
            }, anchor_mode, name);
            let resp = Response::PrimitiveId { id };
            send_response(&mut stream, &resp).await;
        }
        Request::RemoveAiPrimitive { id } => {
            let removed = state.terminal.ai_layer.remove(id);
            let resp = if removed {
                Response::Ok("removed".into())
            } else {
                Response::Error(format!("No AI primitive with id {}", id))
            };
            send_response(&mut stream, &resp).await;
        }
        // ─── Self-driven browser screencast (MCP process → panel) ────────
        // Relay the panel messages straight over control_tx; the raw-mode WS
        // loop forwards them and the browser panel's handleServerMessage
        // already dispatches on these `type`s. Fire-and-forget.
        Request::BrowserFrame { png_base64, title, url, seq } => {
            let env = serde_json::json!({
                "type": "browser_frame",
                "png_base64": png_base64,
                "title": title,
                "url": url,
                "seq": seq,
            });
            if let Ok(json) = serde_json::to_string(&env) {
                let _ = control_tx.send(Arc::new(json));
            }
            send_response(&mut stream, &Response::Ok("frame".into())).await;
        }
        Request::BrowserState { paused } => {
            let env = serde_json::json!({ "type": "browser_state", "paused": paused });
            if let Ok(json) = serde_json::to_string(&env) {
                let _ = control_tx.send(Arc::new(json));
            }
            send_response(&mut stream, &Response::Ok("state".into())).await;
        }
        Request::BrowserHumanRequest { reason, instructions } => {
            let env = serde_json::json!({
                "type": "browser_human_request",
                "reason": reason,
                "instructions": instructions,
            });
            if let Ok(json) = serde_json::to_string(&env) {
                let _ = control_tx.send(Arc::new(json));
            }
            send_response(&mut stream, &Response::Ok("human_request".into())).await;
        }
        Request::PollBrowserInput => {
            let events = std::mem::take(&mut state.browser_input_queue);
            send_response(&mut stream, &Response::BrowserInput { events }).await;
        }
        Request::EvalInPrimitive { id, js } => {
            // Verify the primitive exists so we can fail fast on bad ids.
            let exists = state.terminal.ai_layer.primitives.iter().any(|p| p.id == id);
            if !exists {
                let resp = Response::Error(format!("No AI primitive with id {}", id));
                send_response(&mut stream, &resp).await;
                return;
            }
            // Build the broadcast envelope. WS handler wraps as
            // `{"type":"ai_eval","data":<this>}` — client looks up the card by
            // primitive_id and runs the JS in its Shadow DOM.
            let envelope = serde_json::json!({
                "primitive_id": id,
                "js": js,
            });
            let json = envelope.to_string();
            let _ = ai_eval_tx.send(Arc::new(json));
            let resp = Response::Ok("eval dispatched".into());
            send_response(&mut stream, &resp).await;
        }
        Request::ClearAiLayer => {
            state.terminal.ai_layer.clear();
            let resp = Response::Ok("cleared".into());
            send_response(&mut stream, &resp).await;
        }
        Request::ListAiPrimitives => {
            let primitives = serialize_ai_primitives(&state.terminal.ai_layer.primitives);
            let json = serde_json::to_string_pretty(&primitives).unwrap_or_else(|_| "[]".into());
            let resp = Response::AiPrimitiveList { primitives_json: json };
            send_response(&mut stream, &resp).await;
        }
        Request::UpdateAiPrimitive { id, x, y, width, height, color, text, visible, alpha } => {
            if let Some(prim) = state.terminal.ai_layer.primitives.iter_mut().find(|p| p.id == id) {
                if let Some(v) = visible { prim.visible = v; }
                if let Some(a) = alpha { prim.alpha = a; }
                match &mut prim.kind {
                    immorterm_core::ai_layer::AiPrimitiveKind::Rect(r) => {
                        if let Some(v) = x { r.x = v; }
                        if let Some(v) = y { r.y = v; }
                        if let Some(v) = width { r.width = v; }
                        if let Some(v) = height { r.height = v; }
                        if let Some(c) = color { r.color = c; }
                    }
                    immorterm_core::ai_layer::AiPrimitiveKind::Text(t) => {
                        if let Some(v) = x { t.x = v; }
                        if let Some(v) = y { t.y = v; }
                        if let Some(c) = color { t.color = c; }
                        if let Some(s) = text { t.text = s; }
                    }
                    immorterm_core::ai_layer::AiPrimitiveKind::Button(b) => {
                        if let Some(v) = x { b.x = v; }
                        if let Some(v) = y { b.y = v; }
                        if let Some(v) = width { b.width = v; }
                        if let Some(v) = height { b.height = v; }
                        if let Some(c) = color { b.bg_color = c; }
                        if let Some(s) = text { b.text = s; }
                    }
                    immorterm_core::ai_layer::AiPrimitiveKind::Line(l) => {
                        if let Some(v) = x { l.x1 = v; }
                        if let Some(v) = y { l.y1 = v; }
                        if let Some(c) = color { l.color = c; }
                    }
                    immorterm_core::ai_layer::AiPrimitiveKind::Html(h) => {
                        if let Some(v) = x { h.x = v; }
                        if let Some(v) = y { h.y = v; }
                        if let Some(v) = width { h.width = v; }
                        if let Some(v) = height { h.height = v; }
                        if let Some(s) = text { h.html = s; }
                    }
                }
                state.terminal.ai_layer.dirty = true;
                let resp = Response::Ok("updated".into());
                send_response(&mut stream, &resp).await;
            } else {
                let resp = Response::Error(format!("No AI primitive with id {}", id));
                send_response(&mut stream, &resp).await;
            }
        }
        Request::AnimatePrimitive { primitive_id, property, from, to, duration_ms, easing } => {
            let prop = match property.as_str() {
                "x" => Some(immorterm_core::ai_layer::AnimProperty::X),
                "y" => Some(immorterm_core::ai_layer::AnimProperty::Y),
                "width" => Some(immorterm_core::ai_layer::AnimProperty::Width),
                "height" => Some(immorterm_core::ai_layer::AnimProperty::Height),
                "alpha" => Some(immorterm_core::ai_layer::AnimProperty::Alpha),
                _ => None,
            };
            let ease = match easing.as_deref() {
                Some("ease_in") => immorterm_core::ai_layer::EasingFunc::EaseIn,
                Some("ease_out") => immorterm_core::ai_layer::EasingFunc::EaseOut,
                Some("ease_in_out") => immorterm_core::ai_layer::EasingFunc::EaseInOut,
                _ => immorterm_core::ai_layer::EasingFunc::Linear,
            };
            match prop {
                Some(p) => {
                    state.terminal.ai_layer.animate(primitive_id, p, from, to, duration_ms, ease);
                    let resp = Response::Ok("animating".into());
                    send_response(&mut stream, &resp).await;
                }
                None => {
                    let resp = Response::Error(format!(
                        "Unknown property '{}'. Use: x, y, width, height, alpha", property
                    ));
                    send_response(&mut stream, &resp).await;
                }
            }
        }
        Request::GetViewport { include_text } => {
            let lines = if include_text {
                Some(grid_to_text(&state.terminal))
            } else {
                None
            };
            let resp = Response::ViewportState {
                lines,
                cursor_row: state.terminal.cursor.row,
                cursor_col: state.terminal.cursor.col,
                cursor_visible: state.terminal.cursor.visible,
                cols: state.terminal.cols(),
                rows: state.terminal.rows(),
                ai_primitive_count: state.terminal.ai_layer.primitives.len(),
                theme_name: state.status_bar.theme.clone(),
            };
            send_response(&mut stream, &resp).await;
        }
        Request::PollAiEvents => {
            let events = state.terminal.ai_layer.drain_mcp_events();
            let resp = Response::AiEvents { events };
            send_response(&mut stream, &resp).await;
        }
        Request::WaitForAiEvent { event_type, primitive_id, name, timeout_ms } => {
            // 1. Check if a matching event is already queued
            if let Some(ev) = state.terminal.ai_layer.take_matching_mcp_event(
                event_type.as_deref(), primitive_id, name.as_deref(),
            ) {
                let resp = Response::AiEventOccurred { event: ev };
                send_response(&mut stream, &resp).await;
                return;
            }

            // 2. Guard: if no client is currently rendering this session in
            // raw_mode, no click can ever reach us. Fail fast instead of
            // burning the timeout. Caller can poll instead, or first focus
            // the session's GPU canvas to bring a raw subscriber online.
            let raw_count = state
                .raw_subscriber_count
                .load(std::sync::atomic::Ordering::Relaxed);
            if raw_count == 0 {
                let resp = Response::Error(
                    "No client is rendering this session in raw_mode — no overlay \
                     can be displayed and no click can reach the daemon. Focus the \
                     session's GPU canvas tab and retry, or use background=true and \
                     poll_events instead."
                        .to_string(),
                );
                send_response(&mut stream, &resp).await;
                return;
            }

            // 3. Spawn a separate task for the blocking wait so the main loop
            // continues processing PTY data, WebSocket commands, and new IPC
            // connections. The spawned task only needs the broadcast receiver,
            // Unix stream, and a snapshot of primitives for name matching.
            let ai_rx = ai_event_tx.subscribe();
            let primitives_snapshot = state.terminal.ai_layer.primitives.clone();
            tokio::spawn(async move {
                let mut ai_rx = ai_rx;
                let deadline = tokio::time::Instant::now()
                    + tokio::time::Duration::from_millis(timeout_ms);

                let found = loop {
                    tokio::select! {
                        result = ai_rx.recv() => {
                            if let Ok(ev) = result
                                && matches_ai_event(
                                    &ev,
                                    event_type.as_deref(),
                                    primitive_id,
                                    name.as_deref(),
                                    &primitives_snapshot,
                                ) {
                                    break Some(ev);
                                }
                        }
                        _ = tokio::time::sleep_until(deadline) => {
                            break None;
                        }
                    }
                };

                let resp = match found {
                    Some(ev) => Response::AiEventOccurred { event: ev },
                    None => Response::Error(format!(
                        "Timeout after {}ms waiting for AI event", timeout_ms
                    )),
                };
                send_response(&mut stream, &resp).await;
            });
        }
        Request::GetWebSocketPort => {
            let resp = if state.ws_port > 0 {
                Response::WebSocketInfo {
                    port: state.ws_port,
                    url: format!("ws://127.0.0.1:{}", state.ws_port),
                }
            } else {
                Response::Error("WebSocket server not running".into())
            };
            send_response(&mut stream, &resp).await;
        }
        // ─── End AI Canvas Layer ─────────────────────────────────

        Request::Screenshot { .. } => {
            // GPU rendering can't work in daemon processes (no MTLCompilerService
            // access after double-fork). Use DumpState + client-side rendering.
            let resp = Response::Error(
                "Screenshot not available in daemon — use DumpState and render client-side".into(),
            );
            send_response(&mut stream, &resp).await;
        }
        Request::DumpState => {
            let snapshot = state.terminal.snapshot();
            match serde_json::to_string(&snapshot) {
                Ok(json) => {
                    let resp = Response::TerminalState {
                        snapshot_json: json,
                        session_name: state.name.clone(),
                        status_bar_project: state.status_bar.project.clone(),
                        status_bar_ai_stats: state.status_bar.ai_api_stats.clone(),
                    };
                    send_response(&mut stream, &resp).await;
                }
                Err(e) => {
                    let resp = Response::Error(format!("Failed to serialize terminal state: {}", e));
                    send_response(&mut stream, &resp).await;
                }
            }
        }
        Request::ShowImage { png_data, col, row, width, height } => {
            use base64::Engine;
            // Decode base64 PNG and insert as an image placement
            match base64::engine::general_purpose::STANDARD.decode(&png_data) {
                Ok(png_bytes) => {
                    match image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png) {
                        Ok(img) => {
                            let rgba = img.to_rgba8();
                            let (iw, ih) = rgba.dimensions();
                            let cw = col.unwrap_or(state.terminal.cursor.col);
                            let rw = row.unwrap_or(state.terminal.cursor.row);
                            let cell_w = width.unwrap_or((iw as usize / 8).max(1));
                            let cell_h = height.unwrap_or((ih as usize / 16).max(1));
                            let abs_row = state.terminal.scrollback.len() + rw;
                            let id = state.terminal.graphics.alloc_id();
                            state.terminal.graphics.insert(immorterm_core::graphics::ImagePlacement {
                                id,
                                data: rgba.into_raw(),
                                width: iw,
                                height: ih,
                                placement: immorterm_core::graphics::PlacementMode::Inline,
                                z_index: 10,
                                row: abs_row,
                                col: cw,
                                cell_width: cell_w,
                                cell_height: cell_h,
                            });
                            let resp = Response::Ok(format!("image_displayed:{}", id));
                            send_response(&mut stream, &resp).await;
                        }
                        Err(e) => {
                            let resp = Response::Error(format!("Invalid PNG: {}", e));
                            send_response(&mut stream, &resp).await;
                        }
                    }
                }
                Err(e) => {
                    let resp = Response::Error(format!("Invalid base64: {}", e));
                    send_response(&mut stream, &resp).await;
                }
            }
        }
        Request::AddAnnotation { col, row, width, height, color, label } => {
            let abs_row = state.terminal.scrollback.len() + row;
            let color = color.unwrap_or([1.0, 0.9, 0.2, 1.0]); // default yellow
            let id = state.terminal.overlays.add_annotation(col, abs_row, width, height, color, label);
            let resp = Response::Ok(id.to_string());
            send_response(&mut stream, &resp).await;
        }
        Request::ShowChart { col, row, width, height, values, chart_type, color } => {
            let abs_row = state.terminal.scrollback.len() + row;
            let color = color.unwrap_or([0.0, 0.9, 0.9, 1.0]); // default cyan
            let ct = match chart_type.to_lowercase().as_str() {
                "bar" => immorterm_core::overlays::ChartType::Bar,
                _ => immorterm_core::overlays::ChartType::Sparkline,
            };
            // Normalize values to 0.0-1.0
            let max_val = values.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let normalized: Vec<f32> = if max_val > 0.0 {
                values.iter().map(|v| v / max_val).collect()
            } else {
                values
            };
            let id = state.terminal.overlays.add_chart(col, abs_row, width, height, normalized, color, ct);
            let resp = Response::Ok(id.to_string());
            send_response(&mut stream, &resp).await;
        }
        Request::ClearOverlays => {
            // Clear both the annotations/charts overlay subsystem AND the AI
            // canvas primitives (rects, text, buttons, html). Callers
            // intuitively expect "clear overlays" to wipe everything visual
            // they drew via MCP — leaving html primitives untouched here was
            // a footgun. Use clear_ai_layer specifically to clear ONLY the
            // AI primitives if you ever need to keep annotations/charts.
            state.terminal.overlays.clear();
            state.terminal.ai_layer.clear();
            let resp = Response::Ok("cleared".into());
            send_response(&mut stream, &resp).await;
        }
        Request::GetCapabilities => {
            let resp = Response::Capabilities {
                features: vec![
                    "images".into(),
                    "annotations".into(),
                    "charts".into(),
                    "scrollback".into(),
                    "kitty_graphics".into(),
                    "status_bar".into(),
                    "screenshot".into(),
                    "viewport_stream".into(),
                ],
                version: env!("CARGO_PKG_VERSION").into(),
                renderer: "wgpu".into(),
            };
            send_response(&mut stream, &resp).await;
        }
        Request::SubscribeOutput => {
            let resp = Response::Subscribed {
                cols: state.terminal.cols(),
                rows: state.terminal.rows(),
                title: state.title.clone(),
                project: state.status_bar.project.clone(),
            };
            send_response(&mut stream, &resp).await;

            // Spawn relay: broadcast → client (length-prefixed binary framing, markers stripped)
            let mut rx = pty_filtered_tx.subscribe();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(data) => {
                            let len = (data.len() as u32).to_be_bytes();
                            if stream.write_all(&len).await.is_err() { break; }
                            if stream.write_all(&data).await.is_err() { break; }
                            if stream.flush().await.is_err() { break; }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("GUI output subscriber lagged by {} messages", n);
                            continue;
                        }
                    }
                }
            });
            // Handler returns immediately — relay runs independently
        }
        Request::SubscribeInput => {
            let resp = Response::Subscribed {
                cols: state.terminal.cols(),
                rows: state.terminal.rows(),
                title: state.title.clone(),
                project: state.status_bar.project.clone(),
            };
            send_response(&mut stream, &resp).await;

            // Clone PTY writer and spawn input relay task
            let pty_writer = state.pty.writer_clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Some(ref w) = pty_writer {
                                use std::io::Write;
                                let mut w = w.lock().unwrap();
                                if w.write_all(&buf[..n]).is_err() { break; }
                                let _ = w.flush();
                            }
                        }
                    }
                }
            });
            // Handler returns immediately — relay runs independently
        }
        Request::SubscribeAiLayer => {
            // Send initial state: current AI primitives
            let resp = Response::Subscribed {
                cols: state.terminal.cols(),
                rows: state.terminal.rows(),
                title: state.title.clone(),
                project: state.status_bar.project.clone(),
            };
            send_response(&mut stream, &resp).await;

            // Send current AI layer state immediately (so GUI starts with correct state)
            if let Ok(initial_json) = serde_json::to_vec(&state.terminal.ai_layer.primitives) {
                let len = (initial_json.len() as u32).to_be_bytes();
                let _ = stream.write_all(&len).await;
                let _ = stream.write_all(&initial_json).await;
                let _ = stream.flush().await;
            }

            // Spawn relay: ai_layer broadcast → client (length-prefixed JSON framing)
            let mut rx = ai_layer_tx.subscribe();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(data) => {
                            let len = (data.len() as u32).to_be_bytes();
                            if stream.write_all(&len).await.is_err() { break; }
                            if stream.write_all(&data).await.is_err() { break; }
                            if stream.flush().await.is_err() { break; }
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("AI layer subscriber lagged by {} messages", n);
                            continue;
                        }
                    }
                }
            });
            // Handler returns immediately — relay runs independently
        }
        Request::GetCwd => {
            let cwd = state.terminal.cwd.clone();
            let resp = if cwd.is_empty() {
                Response::Ok("unknown".into())
            } else {
                Response::Ok(cwd)
            };
            send_response(&mut stream, &resp).await;
        }
        Request::GetExitCode => {
            let resp = match state.terminal.last_exit_code {
                Some(code) => Response::Ok(code.to_string()),
                None => Response::Ok("unknown".into()),
            };
            send_response(&mut stream, &resp).await;
        }
        Request::WaitFor { pattern, timeout_ms } => {
            // First check if pattern already exists in current screen
            let screen_text = grid_to_text(&state.terminal).join("\n");
            if screen_text.to_lowercase().contains(&pattern.to_lowercase()) {
                let resp = Response::Ok("found".into());
                send_response(&mut stream, &resp).await;
                return;
            }

            // Not found yet — subscribe to PTY output and watch
            let mut pty_rx = pty_tx.subscribe();
            let deadline = tokio::time::Instant::now()
                + tokio::time::Duration::from_millis(timeout_ms);
            let pat_lower = pattern.to_lowercase();

            let found = loop {
                tokio::select! {
                    data = pty_rx.recv() => {
                        if let Ok(data) = data {
                            // Feed through terminal emulator + ring buffer
                            state.ingest_pty(&data);
                            // Structured logging
                            if let Some(ref mut slog) = state.structured_log {
                                slog.on_pty_output(&data, &state.terminal);
                                let events = state.terminal.drain_prompt_events();
                                if !events.is_empty() {
                                    slog.on_prompt_events(&events, &state.terminal);
                                }
                            }
                            // Drain inline <<html>> blocks
                            for block in state.terminal.drain_html_blocks() {
                                let anchor_mode = match block.attrs.get("anchor").map(|s| s.as_str()) {
                                    Some("fixed") => immorterm_core::ai_layer::AnchorMode::Fixed,
                                    _ => immorterm_core::ai_layer::AnchorMode::Scroll {
                                        scrollback_at_creation: block.scrollback_at_creation,
                                    },
                                };
                                let visible_row = block.anchor_row.saturating_sub(block.scrollback_at_creation);
                                state.terminal.ai_layer.add_html(
                                    immorterm_core::ai_layer::AiHtml {
                                        html: block.content,
                                        css: String::new(),
                                        x: 0.0,
                                        y: 0.0,
                                        width: 0.0,
                                        height: 0.0,
                                        anchor_row: Some(visible_row),
                                        on_click_prompt: None,
                                        on_click_inject_context: None,
                                    },
                                    anchor_mode,
                                    block.attrs.get("name").cloned(),
                                );
                                // Space reservation (same as main event loop)
                                if let Some(h) = block.attrs.get("height")
                                    .and_then(|s| s.parse::<usize>().ok())
                                {
                                    let newlines = "\n".repeat(h);
                                    state.ingest_pty(newlines.as_bytes());
                                    let _ = pty_filtered_tx.send(newlines.into_bytes());
                                }
                            }
                            // Legacy raw log
                            if state.logging
                                && let Some(ref mut writer) = state.log_writer {
                                    use std::io::Write;
                                    let _ = writer.write_all(&data);
                                    let _ = writer.flush();
                                }
                            // Check if pattern now visible on screen
                            let screen = grid_to_text(&state.terminal).join("\n");
                            if screen.to_lowercase().contains(&pat_lower) {
                                break true;
                            }
                        }
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        break false;
                    }
                }
            };

            let resp = if found {
                Response::Ok("found".into())
            } else {
                Response::Error(format!(
                    "Timeout after {}ms waiting for pattern '{}'",
                    timeout_ms, pattern
                ))
            };
            send_response(&mut stream, &resp).await;
        }

        Request::TakeSnapshot => {
            if let Some(ref mut slog) = state.structured_log {
                slog.take_manual_snapshot(&state.terminal);
                let resp = Response::SnapshotTaken {
                    grid_log_path: slog.grid_log_path().to_string_lossy().into_owned(),
                };
                send_response(&mut stream, &resp).await;
            } else {
                let resp = Response::Error("Structured logging not enabled".into());
                send_response(&mut stream, &resp).await;
            }
        }

        Request::UpdatePermissionMode { mode } => {
            state.claude.permission_mode = Some(mode);

            // Fire control event for WebSocket subscribers
            let ctrl_event = crate::websocket::build_control_event("permission_mode_changed", &state.claude);
            let _ = control_tx.send(Arc::new(ctrl_event));

            let resp = Response::Ok("updated".into());
            send_response(&mut stream, &resp).await;
        }
        Request::GetSubagents => {
            let agents = if let Some(ref session_id) = state.claude.session_id {
                crate::subagent_watcher::discover_subagents(session_id)
            } else {
                Vec::new()
            };
            let resp = Response::SubagentList { agents };
            send_response(&mut stream, &resp).await;
        }

        // ─── Agent Teams (handled independently of session state) ────
        Request::ListTeams => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let team_names = team_watcher::discover_teams(&home);
            let mut teams = Vec::new();
            for name in team_names {
                let config_path = immorterm_core::team::team_config_path(&home, &name);
                if let Ok(json) = std::fs::read_to_string(&config_path)
                    && let Ok(config) = immorterm_core::team::parse_team_config(&json) {
                        teams.push(ipc::TeamSummary {
                            name: config.name.clone(),
                            description: config.description.clone(),
                            member_count: config.members.len(),
                            task_counts: (0, 0, 0), // Quick summary, no full parse
                        });
                    }
            }
            let resp = Response::TeamList { teams };
            send_response(&mut stream, &resp).await;
        }
        Request::GetTeamState { team_name } => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            // Do a fresh parse of the team state
            let config_path = immorterm_core::team::team_config_path(&home, &team_name);
            let resp = if let Ok(config_json) = std::fs::read_to_string(&config_path) {
                if let Ok(config) = immorterm_core::team::parse_team_config(&config_json) {
                    let tasks = team_watcher::load_tasks_pub(&home, &team_name);
                    let inboxes = team_watcher::load_inboxes_pub(&home, &team_name);
                    let state = immorterm_core::team::TeamState::new(config, tasks, inboxes);
                    match serde_json::to_string(&state) {
                        Ok(json) => Response::TeamStateData { state_json: json },
                        Err(e) => Response::Error(format!("Serialize error: {}", e)),
                    }
                } else {
                    Response::Error(format!("Failed to parse team config: {}", team_name))
                }
            } else {
                Response::Error(format!("Team not found: {}", team_name))
            };
            send_response(&mut stream, &resp).await;
        }
        Request::SendTeamMessage { team_name, recipient, content } => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let resp = match team_watcher::send_team_message(&home, &team_name, &recipient, &content) {
                Ok(()) => Response::Ok("Message sent".into()),
                Err(e) => Response::Error(format!("Failed to send message: {}", e)),
            };
            send_response(&mut stream, &resp).await;
        }

        // ─── AI Expression Protocol ──────────────────────────────────
        Request::SetExpression {
            confidence, danger, mood, animation, celebrate,
            intensity, color, reset,
        } => {
            if reset {
                state.terminal.reset_expression();
            }
            let expr = &mut state.terminal.expression;
            if let Some(c) = confidence {
                expr.confidence = Some(c.clamp(0.0, 1.0));
            }
            if let Some(ref d) = danger {
                expr.danger = immorterm_core::expression::DangerLevel::from_str_loose(d);
            }
            if let Some(ref m) = mood {
                expr.mood = immorterm_core::expression::Mood::from_str_loose(m);
            }
            if let Some(ref a) = animation {
                expr.animation = immorterm_core::expression::Animation::from_str_loose(a);
            }
            if let Some(ref c) = celebrate {
                expr.celebrate = immorterm_core::expression::Celebration::from_str_loose(c);
            }
            if let Some(i) = intensity {
                expr.intensity = i.clamp(0.0, 1.0);
            }
            if let Some(ref hex) = color {
                expr.color_override = parse_hex_color(hex);
            }
            // Recompute cached metadata
            state.terminal.expression_meta = state.terminal.expression.to_meta();
            let resp = Response::Ok("expression updated".into());
            send_response(&mut stream, &resp).await;

            // Broadcast expression update to WebSocket clients (GPU terminals)
            let expr_msg = serde_json::json!({
                "type": "expression_update",
                "state": &state.terminal.expression,
            });
            if let Ok(json) = serde_json::to_string(&expr_msg) {
                let _ = control_tx.send(Arc::new(json));
            }
        }
        Request::ResetExpression => {
            state.terminal.reset_expression();
            let resp = Response::Ok("expression reset".into());
            send_response(&mut stream, &resp).await;

            // Broadcast expression reset to WebSocket clients (GPU terminals)
            let expr_msg = serde_json::json!({
                "type": "expression_update",
                "state": &state.terminal.expression,
            });
            if let Ok(json) = serde_json::to_string(&expr_msg) {
                let _ = control_tx.send(Arc::new(json));
            }
        }

        // ─── BiDi / Alignment ────────────────────────────────────────
        Request::SetAlignment { alignment, direction } => {
            let mut parts = Vec::new();
            if let Some(ref a) = alignment {
                parts.push(format!("alignment={}", a));
            }
            if let Some(ref d) = direction {
                parts.push(format!("direction={}", d));
            }
            let resp = Response::Ok(format!("alignment updated: {}", parts.join(", ")));
            send_response(&mut stream, &resp).await;

            // Broadcast to WebSocket clients (GPU terminals apply to their renderer)
            let msg = serde_json::json!({
                "type": "alignment_update",
                "alignment": alignment,
                "direction": direction,
            });
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = control_tx.send(Arc::new(json));
            }
        }

        // ─── Audio ──────────────────────────────────────────────────
        Request::PlaySound { sound, path } => {
            // Daemon is double-forked and may not have audio access.
            // Return Ok but note that MCP-side playback is preferred.
            let resp = Response::Ok("audio playback not available in daemon process (use MCP tool)".into());
            let _ = (sound, path); // suppress unused warnings
            send_response(&mut stream, &resp).await;
        }
        Request::SetVolume { volume } => {
            let resp = Response::Ok(format!("volume set to {} (daemon-side noop)", volume));
            send_response(&mut stream, &resp).await;
        }
        Request::ToggleMute => {
            let resp = Response::Ok("mute toggled (daemon-side noop)".into());
            send_response(&mut stream, &resp).await;
        }
        Request::ChannelReply { message } => {
            let resp = if let Some(partner_id) = channel_partner_id.as_deref() {
                // Write message to partner's inbox
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                let inbox_dir =
                    std::path::PathBuf::from(&home).join(".immorterm").join("channel-inbox");
                let channel_msg = crate::channel_registry::ChannelMessage {
                    from_immorterm_id: state.window_id.clone(),
                    from_name: state.name.clone(),
                    message: message.clone(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                };
                match crate::channel_registry::write_to_inbox(
                    &inbox_dir,
                    partner_id,
                    &channel_msg,
                ) {
                    Ok(()) => {
                        // Update local chat overlay with outgoing message
                        if let Some(overlay) = chat_overlay {
                            overlay.add_message(
                                "You",
                                &message,
                                true, // local = outgoing
                                &mut state.terminal.ai_layer,
                            );
                        }
                        Response::Ok(format!(
                            "Message sent to paired session {}",
                            partner_id
                        ))
                    }
                    Err(e) => Response::Error(format!("Failed to send: {}", e)),
                }
            } else {
                Response::Error(
                    "No active pairing. Use interactive session sharing (drag a session pill → Interactive) to pair first.".into(),
                )
            };
            send_response(&mut stream, &resp).await;
        }
        Request::OpenWorkshop { name, html, css, on_click_prompt, on_click_inject_context } => {
            let resp = match validate_workshop_name(&name) {
                Ok(()) => {
                    let workshop = Workshop {
                        name: name.clone(),
                        html: html.clone(),
                        css: css.clone(),
                        modified: std::time::SystemTime::now(),
                        on_click_prompt,
                        on_click_inject_context,
                    };
                    persist_workshop(&state.name, &workshop);
                    let envelope = serde_json::json!({
                        "event": "open",
                        "name": name,
                        "html": html,
                        "css": css,
                    }).to_string();
                    let _ = workshop_tx.send(Arc::new(envelope));
                    state.workshops.insert(name.clone(), workshop);
                    Response::Ok(format!("workshop opened: {}", name))
                }
                Err(e) => Response::Error(e),
            };
            send_response(&mut stream, &resp).await;
        }
        Request::UpdateWorkshop { name, html, css } => {
            let resp = match state.workshops.get_mut(&name) {
                Some(workshop) => {
                    workshop.html = html.clone();
                    workshop.css = css.clone();
                    workshop.modified = std::time::SystemTime::now();
                    persist_workshop(&state.name, workshop);
                    let envelope = serde_json::json!({
                        "event": "update",
                        "name": name,
                        "html": html,
                        "css": css,
                    }).to_string();
                    let _ = workshop_tx.send(Arc::new(envelope));
                    Response::Ok(format!("workshop updated: {}", name))
                }
                None => Response::Error(format!("No workshop named '{}'", name)),
            };
            send_response(&mut stream, &resp).await;
        }
        Request::EvalInWorkshop { name, js } => {
            let resp = if state.workshops.contains_key(&name) {
                let envelope = serde_json::json!({
                    "event": "eval",
                    "name": name,
                    "js": js,
                }).to_string();
                let _ = workshop_tx.send(Arc::new(envelope));
                Response::Ok(format!("eval dispatched: {}", name))
            } else {
                Response::Error(format!("No workshop named '{}'", name))
            };
            send_response(&mut stream, &resp).await;
        }
        Request::CloseWorkshop { name } => {
            // Idempotent: removing a non-existent workshop is Ok.
            state.workshops.remove(&name);
            let _ = std::fs::remove_file(workshop_html_path(&state.name, &name));
            let envelope = serde_json::json!({
                "event": "close",
                "name": name,
            }).to_string();
            let _ = workshop_tx.send(Arc::new(envelope));
            let resp = Response::Ok(format!("workshop closed: {}", name));
            send_response(&mut stream, &resp).await;
        }
        Request::ListWorkshops => {
            let entries: Vec<_> = state.workshops.values().map(|w| {
                let modified_ms = w.modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                serde_json::json!({
                    "name": w.name,
                    "html_size": w.html.len(),
                    "modified_unix_ms": modified_ms,
                })
            }).collect();
            let json = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into());
            send_response(&mut stream, &Response::WorkshopList { workshops_json: json }).await;
        }
        Request::ReadWorkshop { name } => {
            let resp = match state.workshops.get(&name) {
                Some(workshop) => {
                    let modified_unix_ms = workshop
                        .modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    Response::WorkshopState {
                        name: workshop.name.clone(),
                        html: workshop.html.clone(),
                        css: workshop.css.clone(),
                        modified_unix_ms,
                    }
                }
                None => Response::Error(format!("No workshop named '{}'", name)),
            };
            send_response(&mut stream, &resp).await;
        }
    }
}

/// Validate a Workshop name — must be a safe filename component (no slashes,
/// no `..`, non-empty, reasonable length). Names appear in the sidebar AND
/// become file paths under `~/.immorterm/workshops/<session>/<name>.html` so
/// path-traversal would let a misbehaving MCP client write anywhere.
fn validate_workshop_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Workshop name cannot be empty".into());
    }
    if name.len() > 64 {
        return Err("Workshop name too long (max 64 chars)".into());
    }
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." || name.contains("..") {
        return Err("Workshop name must not contain path separators or '..'".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err("Workshop name may only contain [a-zA-Z0-9_.-]".into());
    }
    Ok(())
}

/// `~/.immorterm/workshops/<session>/<name>.html` — the on-disk home for a
/// Workshop's HTML. Persisting lets the user pop the workshop out into a
/// real browser tab (just open the file://) and survives daemon restarts.
fn workshop_html_path(session_name: &str, workshop_name: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home)
        .join(".immorterm")
        .join("workshops")
        .join(session_name)
        .join(format!("{}.html", workshop_name))
}

/// Best-effort write of the workshop's HTML to disk. Failure is logged but
/// not surfaced — the in-memory workshop is the source of truth for live
/// rendering; the file is for pop-out / share / debug.
/// Extract every `data-click="..."` value from an HTML string. Plain string
/// scan — no regex dep. Returns labels in document order, duplicates kept.
fn extract_data_click_labels(html: &str) -> Vec<String> {
    let pat = r#"data-click=""#;
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(pat) {
        let after = &rest[i + pat.len()..];
        if let Some(j) = after.find('"') {
            out.push(after[..j].to_string());
            rest = &after[j + 1..];
        } else {
            break;
        }
    }
    out
}

/// Truncate an HTML string for inclusion in the click context. Workshops can
/// be large (40+ KB); injecting all of it on every click bloats the AI's
/// context. Keep first/last fragments so the AI can read structure + the most
/// recent eval_in_workshop additions.
fn truncate_html_for_context(html: &str, max_chars: usize) -> String {
    if html.len() <= max_chars {
        return html.to_string();
    }
    let head = max_chars / 2;
    let tail = max_chars - head - 32; // 32 chars for the marker
    let head_str: String = html.chars().take(head).collect();
    let tail_str: String = html
        .chars()
        .rev()
        .take(tail)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!(
        "{}\n…[{} chars truncated]…\n{}",
        head_str,
        html.len() - head - tail,
        tail_str
    )
}

/// Resolve the hook-inject template + marker payload for a click event, if
/// the source entity has `on_click_inject_context` set. Returns
/// `(formatted_context, marker_json_value, source_label)` or None.
///
/// The marker payload is self-describing — it includes the workshop/primitive
/// HTML (truncated) and the list of data-click buttons. The hook turns this
/// into a single `additionalContext` block so even cross-session or
/// post-compact AIs (which never saw the original `open_workshop` tool call)
/// understand what was clicked and what other choices existed.
fn resolve_hook_inject(
    state: &SessionState,
    event: &immorterm_core::ai_layer::AiEvent,
) -> Option<(String, serde_json::Value, String)> {
    use immorterm_core::ai_layer::{AiEvent, AiPrimitiveKind};
    const HTML_CONTEXT_CAP: usize = 3200; // ~800 tokens, generous but bounded
    match event {
        AiEvent::WorkshopClicked { name, data_click } => {
            let ws = state.workshops.get(name)?;
            let tpl = ws.on_click_inject_context.clone()?;
            let dc = data_click.as_deref().unwrap_or("");
            let formatted = tpl.replace("{data_click}", dc).replace("{name}", name);
            let payload = serde_json::json!({
                "source": "workshop",
                "workshop": name,
                "data_click": dc,
                "context": formatted,
                "available_buttons": extract_data_click_labels(&ws.html),
                "html_excerpt": truncate_html_for_context(&ws.html, HTML_CONTEXT_CAP),
                "html_size": ws.html.len(),
            });
            Some((formatted, payload, format!("workshop '{}'", name)))
        }
        AiEvent::ButtonClicked { id, data_click } => {
            let prim = state.terminal.ai_layer.primitives.iter().find(|p| p.id == *id)?;
            let html_str = match &prim.kind {
                AiPrimitiveKind::Html(h) => &h.html,
                _ => return None,
            };
            let tpl = match &prim.kind {
                AiPrimitiveKind::Html(h) => h.on_click_inject_context.clone()?,
                _ => return None,
            };
            let dc = data_click.as_deref().unwrap_or("");
            let formatted = tpl.replace("{data_click}", dc).replace("{id}", &id.to_string());
            let payload = serde_json::json!({
                "source": "primitive",
                "primitive_id": id,
                "data_click": dc,
                "context": formatted,
                "available_buttons": extract_data_click_labels(html_str),
                "html_excerpt": truncate_html_for_context(html_str, HTML_CONTEXT_CAP),
                "html_size": html_str.len(),
            });
            Some((formatted, payload, format!("primitive #{}", id)))
        }
        _ => None,
    }
}

/// Write a pending-click marker JSON file the UserPromptSubmit hook will
/// read on the next prompt. Path: `~/.immorterm/pending-click/<session>.json`.
/// Idempotent: overwriting an existing marker is fine — only the most recent
/// click matters since the hook clears the file after reading.
fn write_pending_click_marker(session_name: &str, payload: &serde_json::Value) {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            warn!("pending-click marker: HOME unset, skipping");
            return;
        }
    };
    let dir = std::path::PathBuf::from(&home)
        .join(".immorterm")
        .join("pending-click");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!("pending-click marker: mkdir {:?} failed: {}", dir, e);
        return;
    }
    let path = dir.join(format!("{}.json", session_name));
    if let Err(e) = std::fs::write(&path, payload.to_string()) {
        warn!("pending-click marker: write {:?} failed: {}", path, e);
    }
}

/// Single PTY-inject path shared by workshop clicks AND draw_html overlay
/// button clicks. If the source entity (workshop / AiHtml primitive) has
/// `on_click_prompt` set, format the template against the event and write
/// it to the Claude PTY — Claude treats it as a typed prompt and reacts
/// without any background `wait-event` subprocess.
///
/// Template placeholders supported (entity-aware):
/// - `{data_click}` — the clicked element's `data-click` attribute value
/// - `{name}` — workshop name (when source is a workshop)
/// - `{id}` — primitive numeric id (when source is a draw_html primitive)
fn inject_on_click_prompt(
    state: &mut SessionState,
    event: &immorterm_core::ai_layer::AiEvent,
) {
    use immorterm_core::ai_layer::{AiEvent, AiPrimitiveKind};

    // Diagnostic log so we can see in daemon.log whether the click event
    // reached us and what entity it points at. Cheap, one line per click.
    match event {
        AiEvent::WorkshopClicked { name, data_click } => {
            let ws = state.workshops.get(name);
            warn!(
                "inject_on_click_prompt: WorkshopClicked name={:?} data_click={:?} ws_found={} has_on_click_prompt={} has_on_click_inject_context={}",
                name,
                data_click,
                ws.is_some(),
                ws.map(|w| w.on_click_prompt.is_some()).unwrap_or(false),
                ws.map(|w| w.on_click_inject_context.is_some()).unwrap_or(false),
            );
        }
        AiEvent::ButtonClicked { id, data_click } => {
            let prim = state.terminal.ai_layer.primitives.iter().find(|p| p.id == *id);
            let (is_html, has_p, has_ctx) = match prim {
                Some(p) => match &p.kind {
                    AiPrimitiveKind::Html(h) => (true, h.on_click_prompt.is_some(), h.on_click_inject_context.is_some()),
                    _ => (false, false, false),
                },
                None => (false, false, false),
            };
            warn!(
                "inject_on_click_prompt: ButtonClicked id={} data_click={:?} prim_found={} is_html={} has_on_click_prompt={} has_on_click_inject_context={}",
                id, data_click, prim.is_some(), is_html, has_p, has_ctx,
            );
        }
        _ => {}
    }

    // First check the HOOK-INJECTION path. If `on_click_inject_context` is set
    // on the entity, write a marker file for the UserPromptSubmit hook and
    // type a tiny trigger to fire it. Hook supplies the rich context as
    // `additionalContext` so the terminal stays clean.
    if let Some((ctx_template, marker_payload, source_label)) =
        resolve_hook_inject(state, event)
    {
        write_pending_click_marker(&state.name, &marker_payload);
        // Type the smallest possible visible trigger. Claude Code's TUI rejects
        // truly empty submissions; "." is one byte, easy to filter out of
        // history visually, and reliably submits.
        let _ = state.pty.write_all(b".");
        if let Some(writer) = state.pty.writer_clone() {
            let label = source_label.clone();
            tokio::task::spawn_blocking(move || {
                std::thread::sleep(std::time::Duration::from_millis(80));
                if let Ok(mut w) = writer.lock() {
                    let _ = w.write_all(b"\r").and_then(|_| w.flush());
                }
                drop(label);
            });
        }
        info!(
            "{}: hook-inject path — wrote marker + typed '.', ctx_template={} chars",
            source_label,
            ctx_template.len()
        );
        return;
    }

    // Otherwise fall through to the PTY-text path (writes the formatted
    // prompt as if the user typed it).
    let (template, dc, source_label) = match event {
        AiEvent::WorkshopClicked { name, data_click } => {
            let Some(ws) = state.workshops.get(name) else { return };
            let Some(t) = ws.on_click_prompt.clone() else { return };
            let formatted = t
                .replace("{data_click}", data_click.as_deref().unwrap_or(""))
                .replace("{name}", name);
            (formatted, data_click.clone(), format!("workshop '{}'", name))
        }
        AiEvent::ButtonClicked { id, data_click } => {
            // Look up the primitive; only HTML primitives carry on_click_prompt.
            let Some(prim) = state
                .terminal
                .ai_layer
                .primitives
                .iter()
                .find(|p| p.id == *id)
            else { return };
            let Some(template) = (match &prim.kind {
                AiPrimitiveKind::Html(h) => h.on_click_prompt.clone(),
                _ => None,
            }) else { return };
            let formatted = template
                .replace("{data_click}", data_click.as_deref().unwrap_or(""))
                .replace("{id}", &id.to_string());
            (formatted, data_click.clone(), format!("primitive #{}", id))
        }
        _ => return,
    };

    // Two-stage write so Claude Code's TUI doesn't treat the burst as a paste:
    //   1. write the text WITHOUT a CR
    //   2. sleep briefly (~80ms) — beyond any reasonable paste-burst window
    //   3. write just `\r` as a discrete Enter keystroke that submits
    //
    // Without the split, the TUI sees text+\r arriving in one read() and treats
    // `\r` as a literal newline in the input buffer (multiline paste behavior)
    // rather than "submit." That made every click stack a new line in the
    // prompt without sending it. Splitting in time fixes the heuristic.
    let text_bytes = template.into_bytes();
    if let Err(e) = state.pty.write_all(&text_bytes) {
        warn!("{}: on_click_prompt text PTY write failed: {}", source_label, e);
        return;
    }
    // Defer the CR via the cloneable writer Arc — no blocking the select loop,
    // no fd lifetime issues. The Arc holds the same handle the main loop uses,
    // so the spawned task's write goes to the same PTY.
    if let Some(writer) = state.pty.writer_clone() {
        let label_for_log = source_label.clone();
        tokio::task::spawn_blocking(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            let mut w = match writer.lock() {
                Ok(w) => w,
                Err(_) => return, // poisoned lock — main loop already in trouble
            };
            if let Err(e) = w.write_all(b"\r").and_then(|_| w.flush()) {
                warn!("{}: deferred CR write failed: {}", label_for_log, e);
            }
        });
    }
    info!(
        "{}: injected on_click_prompt to PTY (data_click={:?}, {} bytes + deferred CR)",
        source_label,
        dc,
        text_bytes.len()
    );
}

fn persist_workshop(session_name: &str, workshop: &Workshop) {
    let path = workshop_html_path(session_name, &workshop.name);
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("workshop persist: mkdir {:?} failed: {}", parent, e);
            return;
        }
    // Wrap the body in a minimal HTML document so file:// pop-out renders.
    // CSS goes in <style>; HTML goes in <body>. Same Shadow-DOM rules apply
    // when the in-app webview re-renders, but the standalone file works in
    // any browser without extra glue.
    let document = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{name}</title>\n<style>{css}</style></head>\n<body>{html}</body></html>",
        name = workshop.name,
        css = workshop.css,
        html = workshop.html,
    );
    if let Err(e) = std::fs::write(&path, document) {
        tracing::warn!("workshop persist: write {:?} failed: {}", path, e);
    }
}

/// Execute a command in the session (from -X).
fn execute_command(
    state: &mut SessionState,
    command: &str,
    args: &[String],
) -> Result<String> {
    match command {
        "stuff" => {
            // Send text to the shell
            if let Some(text) = args.first() {
                // Unescape \n → actual newline (screen compat)
                let unescaped = text.replace("\\n", "\n");
                state.pty.write_all(unescaped.as_bytes())?;
            }
            Ok(String::new())
        }
        "title" => {
            if let Some(title) = args.first() {
                state.title = title.clone();
            }
            Ok(String::new())
        }
        "quit" => {
            // Signal the shell to exit
            state.pty.signal(nix::sys::signal::Signal::SIGHUP)?;
            // Give it a moment, then force
            std::thread::sleep(std::time::Duration::from_millis(100));
            state.pty.signal(nix::sys::signal::Signal::SIGKILL).ok();
            Ok(String::new())
        }
        "log" => {
            match args.first().map(|s| s.as_str()) {
                Some("on") => {
                    state.logging = true;
                    if state.log_writer.is_none()
                        && let Some(ref path) = state.log_file {
                            state.log_writer = fs::File::create(path).ok();
                        }
                }
                Some("off") => {
                    state.logging = false;
                }
                _ => {}
            }
            Ok(String::new())
        }
        "logfile" => {
            if let Some(path) = args.first() {
                state.log_file = Some(path.clone());
                if state.logging {
                    state.log_writer = fs::File::create(path).ok();
                }
            }
            Ok(String::new())
        }
        "setenv" => {
            if args.len() >= 2 {
                state.env.insert(args[0].clone(), args[1].clone());
            }
            Ok(String::new())
        }
        "scrollback" => {
            if let Some(n) = args.first().and_then(|s| s.parse::<usize>().ok()) {
                state.scrollback_max = n;
                state.terminal.set_scrollback(n);
            }
            Ok(String::new())
        }
        "aistats" => {
            // AI stats display — store for status bar rendering
            // args[0] = process stats, args[1] = API stats (optional)
            // No args = clear both
            match args.len() {
                0 => {
                    state.status_bar.ai_process_stats.clear();
                    state.status_bar.ai_api_stats.clear();
                }
                1 => {
                    state.status_bar.ai_process_stats = args[0].clone();
                }
                _ => {
                    state.status_bar.ai_process_stats = args[0].clone();
                    state.status_bar.ai_api_stats = args[1].clone();
                }
            }
            Ok(String::new())
        }
        "aistatstoggle" => {
            // Cycle stats mode: 0=process → 1=api → 2=both → 0
            state.status_bar.stats_mode = (state.status_bar.stats_mode + 1) % 3;
            Ok(String::new())
        }
        "hardstatus" | "redisplay" | "scrollback_dump" => {
            // Screen compatibility — accept but ignore
            Ok(String::new())
        }
        _ => {
            Ok(format!("Unknown command: {}", command))
        }
    }
}

/// Query a value from the session (from -Q).
///
/// The extension calls: `immorterm -S "name" -Q echo '${VAR}'`
/// Shell strips single quotes, so we receive args ["echo", "$VAR"].
/// Joined: "echo $VAR". We need to parse the var name from "$VAR".
fn query_command(state: &SessionState, command: &str) -> String {
    match command {
        "title" => state.title.clone(),
        _ => {
            // Handle echo query in multiple formats:
            // Format 1 (from shell): "echo $VAR"
            // Format 2 (quoted):     "echo '${VAR}'"
            if let Some(rest) = command.strip_prefix("echo ") {
                let var_name = rest
                    .trim_start_matches('\'')
                    .trim_end_matches('\'')
                    .trim_start_matches("${")
                    .trim_end_matches('}')
                    .trim_start_matches('$');
                state
                    .env
                    .get(var_name)
                    .cloned()
                    .unwrap_or_else(|| format!("${}", var_name))
            } else {
                String::new()
            }
        }
    }
}

/// Set socket file permissions to indicate attached/detached state.
fn set_socket_permissions(path: &PathBuf, attached: bool) {
    let mode = if attached { 0o700 } else { 0o600 };
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

// ─── WebSocket streaming helpers ─────────────────────────────────────

/// Compute a viewport diff from dirty rows. Only includes rows where `row.dirty == true`.
fn compute_viewport_diff(
    terminal: &immorterm_core::Terminal,
    seq: u64,
    ai_dirty: bool,
    ai_events: &[immorterm_core::ai_layer::AiEvent],
) -> crate::websocket::WsServerMsg {
    let mut dirty_rows = Vec::new();
    for i in 0..terminal.rows() {
        if let Some(row) = terminal.grid.row(i)
            && row.dirty {
                dirty_rows.push(crate::websocket::ViewportRow {
                    idx: i,
                    text: row_to_text(row),
                });
            }
    }

    let ai_primitives = if ai_dirty {
        Some(serialize_ai_primitives(&terminal.ai_layer.primitives))
    } else {
        None
    };

    let ai_events_json: Vec<serde_json::Value> = ai_events
        .iter()
        .map(|e| match e {
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
        })
        .collect();

    crate::websocket::WsServerMsg::ViewportDiff {
        seq,
        dirty_rows,
        cursor: crate::websocket::CursorState {
            row: terminal.cursor.row,
            col: terminal.cursor.col,
            visible: terminal.cursor.visible,
        },
        scrollback_len: terminal.scrollback.len(),
        ai_layer_changed: ai_dirty,
        ai_primitives,
        ai_events: ai_events_json,
    }
}

/// Clear all dirty flags after broadcasting a viewport diff.
fn clear_dirty_flags(terminal: &mut immorterm_core::Terminal) {
    terminal.dirty = false;
    terminal.ai_layer.dirty = false;
    terminal.overlays.dirty = false;
    for i in 0..terminal.rows() {
        if let Some(row) = terminal.grid.row_mut(i) {
            row.dirty = false;
        }
    }
}

/// Build a full viewport state snapshot (for hello and lag recovery).
fn build_full_viewport(state: &SessionState) -> crate::websocket::InitialState {
    crate::websocket::InitialState {
        session: state.name.clone(),
        cols: state.terminal.cols(),
        rows: state.terminal.rows(),
        title: state.title.clone(),
        project: state.status_bar.project.clone(),
        theme: state.status_bar.theme.clone(),
        lines: grid_to_text(&state.terminal),
        cursor: crate::websocket::CursorState {
            row: state.terminal.cursor.row,
            col: state.terminal.cursor.col,
            visible: state.terminal.cursor.visible,
        },
        scrollback_len: state.terminal.scrollback.len(),
        ai_primitives: serialize_ai_primitives(&state.terminal.ai_layer.primitives),
    }
}

/// Serialize AI primitives to JSON values for WS messages.
/// Check if an AI event matches the given type and primitive ID filters.
fn matches_ai_event(
    ev: &immorterm_core::ai_layer::AiEvent,
    event_type: Option<&str>,
    primitive_id: Option<u32>,
    name: Option<&str>,
    primitives: &[immorterm_core::ai_layer::AiPrimitive],
) -> bool {
    use immorterm_core::ai_layer::AiEvent;
    // Workshop clicks carry the workshop name directly; they have no
    // numeric primitive id (workshops live in a separate store). Match by
    // event_type=click and the name field on the event itself.
    if let AiEvent::WorkshopClicked { name: ws_name, .. } = ev {
        if event_type.is_some() && event_type != Some("click") {
            return false;
        }
        // primitive_id filter is meaningless for workshops — only match
        // workshops if the caller didn't pin a specific primitive id.
        if primitive_id.is_some() {
            return false;
        }
        if let Some(filter) = name
            && filter != ws_name {
                return false;
            }
        return true;
    }

    let (ev_type, ev_id) = match ev {
        AiEvent::ButtonClicked { id, .. } => ("click", *id),
        AiEvent::ButtonHovered { id, .. } => ("hover", *id),
        AiEvent::WorkshopClicked { .. } => unreachable!("handled above"),
    };
    if event_type.is_some() && event_type != Some(ev_type) {
        return false;
    }
    if primitive_id.is_some() && primitive_id != Some(ev_id) {
        return false;
    }
    if let Some(name_filter) = name {
        let prim_name = primitives.iter()
            .find(|p| p.id == ev_id)
            .and_then(|p| p.name.as_deref());
        if prim_name != Some(name_filter) {
            return false;
        }
    }
    true
}

/// Convert an anchor string ("fixed"/"scroll") to an `AnchorMode`, capturing
/// the current scrollback length for scroll-anchored primitives.
/// If `anchor_to` references an existing primitive, copy its scroll anchor.
fn parse_anchor_mode(
    anchor: &Option<String>,
    anchor_to: &Option<u32>,
    terminal: &immorterm_core::terminal::Terminal,
) -> immorterm_core::ai_layer::AnchorMode {
    // anchor_to takes precedence: copy the referenced primitive's anchor mode
    if let Some(ref_id) = anchor_to
        && let Some(prim) = terminal.ai_layer.primitives.iter().find(|p| p.id == *ref_id) {
            return prim.anchor.clone();
        }
    match anchor.as_deref() {
        Some("scroll") => immorterm_core::ai_layer::AnchorMode::Scroll {
            scrollback_at_creation: terminal.scrollback.len(),
        },
        _ => immorterm_core::ai_layer::AnchorMode::Fixed,
    }
}

/// Parse a hex color string (e.g., "#ff0000" or "ff0000") to [f32; 4] RGBA.
fn parse_hex_color(hex: &str) -> Option<[f32; 4]> {
    let hex = hex.trim_start_matches('#');
    if hex.len() == 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.0;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.0;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.0;
        Some([r, g, b, 1.0])
    } else if hex.len() == 8 {
        let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.0;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.0;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.0;
        let a = u8::from_str_radix(&hex[6..8], 16).ok()? as f32 / 255.0;
        Some([r, g, b, a])
    } else {
        None
    }
}

fn serialize_ai_primitives(primitives: &[immorterm_core::ai_layer::AiPrimitive]) -> Vec<serde_json::Value> {
    primitives
        .iter()
        .filter_map(|p| serde_json::to_value(p).ok())
        .collect()
}

/// Handle a WebSocket command dispatched from a client handler.
fn handle_ws_command(cmd: crate::websocket::WsCommand, state: &mut SessionState) {
    use crate::websocket::{WsCommand, WsDrawReply};

    match cmd {
        WsCommand::Input(data) => {
            state.pty.write_all(&data).ok();
        }
        WsCommand::BrowserInput(event) => {
            state.browser_input_queue.push(event);
        }
        WsCommand::DrawRect {
            x, y, width, height, color, border_color, border_width, anchor, reply,
        } => {
            let anchor_mode = parse_anchor_mode(&anchor, &None, &state.terminal);
            let id = state.terminal.ai_layer.add_rect(immorterm_core::ai_layer::AiRect {
                x, y, width, height, color,
                border_color,
                border_width: border_width.unwrap_or(0.0),
            }, anchor_mode, None);
            let _ = reply.send(WsDrawReply { id, ptype: "rect".into() });
        }
        WsCommand::DrawText {
            text, x, y, color, font_size_scale, anchor, reply,
        } => {
            let anchor_mode = parse_anchor_mode(&anchor, &None, &state.terminal);
            let id = state.terminal.ai_layer.add_text(immorterm_core::ai_layer::AiText {
                text, x, y, color,
                font_size_scale: font_size_scale.unwrap_or(1.0),
            }, anchor_mode, None);
            let _ = reply.send(WsDrawReply { id, ptype: "text".into() });
        }
        WsCommand::DrawButton {
            text, x, y, width, height, bg_color, text_color, anchor, reply,
        } => {
            // Intercept "__click__:{id}" or "__click__:{id}:{data-click}" — the HTML overlay sends this when a button is clicked
            if let Some(id_str) = text.strip_prefix("__click__:") {
                let (id_part, data_click) = match id_str.split_once(':') {
                    Some((id_p, dc)) => (id_p, Some(dc.to_string())),
                    None => (id_str, None),
                };
                if let Ok(btn_id) = id_part.parse::<u32>() {
                    state.terminal.ai_layer.push_button_click(btn_id, data_click);
                    let _ = reply.send(WsDrawReply { id: btn_id, ptype: "button_click".into() });
                } else {
                    let _ = reply.send(WsDrawReply { id: 0, ptype: "error".into() });
                }
            } else {
                let anchor_mode = parse_anchor_mode(&anchor, &None, &state.terminal);
                let id = state.terminal.ai_layer.add_button(immorterm_core::ai_layer::AiButton {
                    text, x, y, width, height, bg_color, text_color,
                    hovered: false,
                }, anchor_mode, None);
                let _ = reply.send(WsDrawReply { id, ptype: "button".into() });
            }
        }
        WsCommand::DrawLine {
            x1, y1, x2, y2, color, thickness, anchor, reply,
        } => {
            let anchor_mode = parse_anchor_mode(&anchor, &None, &state.terminal);
            let id = state.terminal.ai_layer.add_line(immorterm_core::ai_layer::AiLine {
                x1, y1, x2, y2, color,
                thickness: thickness.unwrap_or(2.0),
            }, anchor_mode, None);
            let _ = reply.send(WsDrawReply { id, ptype: "line".into() });
        }
        WsCommand::RemovePrimitive { id, reply } => {
            if state.terminal.ai_layer.remove(id) {
                let _ = reply.send(Ok(()));
            } else {
                let _ = reply.send(Err(format!("No AI primitive with id {}", id)));
            }
        }
        WsCommand::ClearAiLayer => {
            state.terminal.ai_layer.clear();
        }
        WsCommand::Animate {
            primitive_id, property, from, to, duration_ms, easing, reply,
        } => {
            let prop = match property.as_str() {
                "x" => Some(immorterm_core::ai_layer::AnimProperty::X),
                "y" => Some(immorterm_core::ai_layer::AnimProperty::Y),
                "width" => Some(immorterm_core::ai_layer::AnimProperty::Width),
                "height" => Some(immorterm_core::ai_layer::AnimProperty::Height),
                "alpha" => Some(immorterm_core::ai_layer::AnimProperty::Alpha),
                _ => None,
            };
            let ease = match easing.as_deref() {
                Some("ease_in") => immorterm_core::ai_layer::EasingFunc::EaseIn,
                Some("ease_out") => immorterm_core::ai_layer::EasingFunc::EaseOut,
                Some("ease_in_out") => immorterm_core::ai_layer::EasingFunc::EaseInOut,
                _ => immorterm_core::ai_layer::EasingFunc::Linear,
            };
            match prop {
                Some(p) => {
                    state.terminal.ai_layer.animate(primitive_id, p, from, to, duration_ms, ease);
                    let _ = reply.send(Ok(()));
                }
                None => {
                    let _ = reply.send(Err(format!(
                        "Unknown property '{}'. Use: x, y, width, height, alpha",
                        property
                    )));
                }
            }
        }
        WsCommand::Resize { cols, rows } => {
            let prev_cols = state.terminal.cols();
            state.pty.resize(cols, rows);
            state.terminal.resize(cols as usize, rows as usize);
            state.process_deferred_restore(cols, rows);

            // Record the resize in PTY history so future replays (Cmd+Shift+R,
            // reattach restore) reproduce bytes at the cols they were
            // originally processed at — not all bytes forced through current
            // cols. Without this, replay would re-wrap a separator row Claude
            // wrote at the old cols, overflow by the cols delta, and shift
            // every subsequent cursor write by that amount — interleaving
            // garbage like `Self─{` into scrollback.
            if cols as usize != prev_cols {
                state.pty_history.record_resize(cols, rows);

                // Trigger a full snapshot on the next subscribe_raw so the
                // WASM client replaces its (now-stale-cols) local scrollback
                // with the daemon's Phase-1-reflowed version. Without this
                // the client's snapshot path takes the viewport-only branch,
                // load_snapshot preserves WASM-local scrollback, and the
                // user sees rows above the viewport stuck at the old cols.
                // NOTE: the auto byte-replay that used to live here was
                // removed — it corrupted scrollback when the session had
                // intermediate resizes (single-cols replay forced every
                // historical byte through current cols). Phase 1 reflow
                // inside terminal.resize above handles the reflow correctly;
                // we only need to nudge the client to fetch it.
                state.pending_full_snapshot = true;
            }

            if let Some(ref mut slog) = state.structured_log {
                slog.on_resize(cols as usize, rows as usize);
            }
        }
        WsCommand::DismissAttention => {
            // Frontend cleared the bell visually (active session received PTY data,
            // or user explicitly acknowledged). Mirror to registry so reload doesn't
            // bring the bell back. No broadcast — frontend already updated its view.
            if state.needs_attention {
                state.needs_attention = false;
                let mut registry = crate::registry::Registry::load();
                if let Some(entry) = registry.sessions.iter_mut().find(|e| e.pid == std::process::id()) {
                    entry.needs_attention = false;
                    if let Err(e) = registry.save() {
                        warn!("Failed to clear needs_attention on dismiss: {}", e);
                    }
                }
            }
        }
        WsCommand::RerenderBacklog => {
            // Manual escape hatch (Ctrl+Shift+R) — mimic a user drag-resize
            // round trip. Cycling the emulator through (cols-1, cols) runs
            // Phase 1 reflow twice on scrollback+grid, ending at the same
            // dims but with all rows freshly reflowed to current cols. This
            // replaces the previous byte-replay approach, which corrupted
            // sessions whose PTY ring overflowed mid-history. Now nothing
            // touches the byte stream — the emulator's own resize logic does
            // the rebuild, the pending_full_snapshot flag ships it to the
            // client on the next subscribe_raw, and a single SIGWINCH kicks
            // any live TUI to redraw at current dims. No PTY winsize change
            // (we don't call pty.resize) so the child app sees only the one
            // SIGWINCH at its original size — no flicker.
            let cols = state.terminal.cols();
            let rows = state.terminal.rows();
            if cols >= 2 {
                state.terminal.resize(cols - 1, rows);
                state.terminal.resize(cols, rows);
                state.pending_full_snapshot = true;
                info!(
                    "RerenderBacklog: reflow cycle {}→{}→{}; flagged full snapshot",
                    cols,
                    cols - 1,
                    cols
                );
            } else {
                info!("RerenderBacklog: cols < 2, skipping reflow cycle");
            }
            if let Err(e) = state.pty.signal(signal::Signal::SIGWINCH) {
                warn!("SIGWINCH on rerender_backlog failed: {}", e);
            } else {
                info!("RerenderBacklog: sent SIGWINCH to force child redraw");
            }
        }
        WsCommand::ReconnectAi => {
            // Right-click "Reconnect to AI" — force-quit the running AI and
            // re-invoke the recall cascade. Mirrors the extension's
            // `gracefulClaudeExit` but daemon-side, with proper PID-poll
            // wait-for-exit (no fragile fixed delays).
            let writer = match state.pty.writer_clone() {
                Some(w) => w,
                None => {
                    warn!("ReconnectAi: PTY writer unavailable");
                    return;
                }
            };
            let bin = std::env::current_exe()
                .ok()
                .and_then(|p| p.into_os_string().into_string().ok())
                .unwrap_or_else(|| "immorterm-ai".into());
            let recall_bytes = format!("{} recall\r", bin).into_bytes();

            // Re-scan before deciding. claude_pid is normally refreshed by a
            // 10s periodic tick — without this nudge, a Claude that started
            // (or respawned) inside the last tick window still looks like
            // `None` here. The no-pid branch below assumes "no AI to kill"
            // and stuffs the recall string straight into the PTY, which then
            // shows up as literal input inside the live Claude TUI.
            let shell_pid = state.pty.child_pid();
            state.claude.scan(shell_pid);

            let claude_pid = state.claude.claude_pid;
            // Sticky: true once any AI was observed in this terminal (via
            // process-tree scan, OSC 1337, registry env backfill, or
            // boot-time restored UUID). Survives AI exit, unlike
            // session_id alone which is only set by OSC.
            let has_history = state.claude.had_ai_session;

            // Guard: if no AI is running AND no AI has ever been tracked in
            // this terminal session, silently skip. Without this, the recall
            // cascade's tier-4 fallback ("no UUID, no skill → plain claude")
            // would spawn a fresh Claude on bare-shell sessions — surprising
            // behavior when the user expected reconnect-to-existing.
            if claude_pid.is_none() && !has_history {
                info!("ReconnectAi: no AI in this session (no current pid, no recorded session_id) — skipping");
                return;
            }

            match claude_pid {
                None => {
                    info!("ReconnectAi: AI not running but session_id known, stuffing recall (will reattach via cascade)");
                    pty_try_write(&writer, &recall_bytes);
                }
                Some(pid) => {
                    info!("ReconnectAi: force-quitting AI pid {} then stuffing recall", pid);
                    tokio::spawn(async move {
                        let target = nix::unistd::Pid::from_raw(pid as i32);
                        const MAX_ATTEMPTS: u32 = 2;
                        let mut exited = false;
                        for attempt in 1..=MAX_ATTEMPTS {
                            if attempt > 1 {
                                warn!(
                                    "ReconnectAi: pid {} did not exit on attempt {}, retrying",
                                    pid, attempt - 1
                                );
                            }
                            if attempt_claude_exit(&writer, target).await {
                                exited = true;
                                break;
                            }
                        }
                        if exited {
                            info!("ReconnectAi: pid {} exited, stuffing recall", pid);
                        } else {
                            warn!(
                                "ReconnectAi: pid {} still alive after {} attempts; stuffing recall anyway (process may orphan)",
                                pid, MAX_ATTEMPTS
                            );
                        }
                        pty_try_write(&writer, &recall_bytes);
                    });
                }
            }
        }
        WsCommand::GetInitialState(reply) => {
            let _ = reply.send(build_full_viewport(state));
        }
        WsCommand::SubscribeRaw { reply, .. } => {
            // This is handled specially in the event loop (needs pty_tx access)
            // Unreachable from handle_ws_command — handled inline in the select loop
            let _ = reply;
        }
        WsCommand::GetControlState(reply) => {
            // Handled inline in the event loop's select! — unreachable here
            let _ = reply;
        }
        WsCommand::ScrollRequest { reply, .. } => {
            // Handled inline in the event loop's select! — unreachable here
            let _ = reply;
        }
        WsCommand::RegisterChannel { .. } | WsCommand::PairSessions { .. } | WsCommand::UnpairSessions => {
            // Handled inline in the event loop's select! — unreachable here
        }
        WsCommand::CloseWorkshop { .. } => {
            // Handled inline in the event loop's select! — unreachable here
        }
    }
}
