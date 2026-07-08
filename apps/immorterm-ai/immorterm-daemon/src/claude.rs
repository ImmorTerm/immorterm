//! AI coding tool process detection and tracking.
//!
//! Replaces these bash scripts (total ~816 lines):
//! - claude-session-capture (125 lines) — one-shot session ID extraction via /status
//! - claude-session-map (142 lines) — timestamp-based session correlation
//! - claude-session-sync (406 lines) — background process/content/timestamp sync
//! - claude-session-tracker (143 lines) — real-time history.jsonl monitoring
//! - claude-stats (80 lines) — status bar formatting
//!
//! Architecture: **Push-based** session tracking.
//!
//! Claude Code's `statusLine` feature calls `.claude/statusline.sh` which writes
//! session data to `/tmp/immorterm-claude-ctx-<sessionId>`. Each file contains
//! the terminal's WINDOW_ID, giving us a deterministic mapping — no heuristics.
//!
//! The daemon:
//! 1. Walks the process tree to detect IF an AI tool is running (ps-based, lightweight)
//! 2. Reads context files written by statusline.sh, matches by WINDOW_ID
//! 3. Extracts model, cost, context window %, transcript path from the context file

use std::collections::HashMap;
use std::time::Instant;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tracing::{info, warn};

/// Known AI coding tools we can detect in the process tree.
///
/// Duplicate of `structured_logs::AiTool` — kept in sync so the daemon
/// doesn't pull a structured_logs dep here. When you add a vendor, mirror
/// it in `libs/structured-logs/src/ai_extractor.rs::AiTool` and
/// `apps/immorterm-ai/immorterm-daemon/src/daemon.rs::name → AiTool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiTool {
    Claude,
    Aider,
    Cursor,
    Copilot,
    Codex,
    Windsurf,
    Cline,
    Opencode,
    Gemini,
    Continue,
    Cody,
    Unknown,
}

impl AiTool {
    pub fn name(&self) -> &'static str {
        match self {
            AiTool::Claude => "claude",
            AiTool::Aider => "aider",
            AiTool::Cursor => "cursor",
            AiTool::Copilot => "copilot",
            AiTool::Codex => "codex",
            AiTool::Windsurf => "windsurf",
            AiTool::Cline => "cline",
            AiTool::Opencode => "opencode",
            AiTool::Gemini => "gemini",
            AiTool::Continue => "continue",
            AiTool::Cody => "cody",
            AiTool::Unknown => "unknown",
        }
    }

    /// Status-bar display name. Title-cased + brand-correct ("Opencode",
    /// "Copilot") — kept separate from `name()` (which is the lowercase
    /// cross-codebase identifier used by the registry, hub session-link,
    /// and `IMMORTERM_AI_TOOL` env var). Falls through to "AI" for
    /// Unknown/legacy callers so the status bar never shows "Unknown".
    pub fn display_name(&self) -> &'static str {
        match self {
            AiTool::Claude => "Claude",
            AiTool::Aider => "Aider",
            AiTool::Cursor => "Cursor",
            AiTool::Copilot => "Copilot",
            AiTool::Codex => "Codex",
            AiTool::Windsurf => "Windsurf",
            AiTool::Cline => "Cline",
            AiTool::Opencode => "Opencode",
            AiTool::Gemini => "Gemini",
            AiTool::Continue => "Continue",
            AiTool::Cody => "Cody",
            AiTool::Unknown => "AI",
        }
    }
}

/// Stats pushed by Claude Code via the statusline script.
#[derive(Debug, Clone, Default)]
pub struct ClaudeApiStats {
    /// Model display name (e.g., "Claude Opus 4")
    pub model: String,
    /// Total cost in USD
    pub cost_usd: f64,
    /// Context window usage percentage (0-100)
    pub context_pct: f64,
    /// Path to the JSONL transcript
    pub transcript_path: String,
}

/// Tracks AI coding tool processes running inside this terminal session.
pub struct ClaudeTracker {
    /// PID of the detected AI process (if running)
    pub claude_pid: Option<u32>,
    /// Which AI tool was detected
    pub detected_tool: Option<AiTool>,
    /// Claude session UUID — deterministically matched via WINDOW_ID
    pub session_id: Option<String>,
    /// When the AI process was first detected
    pub start_time: Option<Instant>,
    /// Latest RSS in kilobytes (from ps)
    pub rss_kb: u64,
    /// Latest CPU percentage (from ps)
    pub cpu_percent: f32,
    /// API stats pushed by Claude via statusline.sh
    pub api_stats: ClaudeApiStats,
    /// Current permission mode (e.g., "delegate", "plan", "default")
    pub permission_mode: Option<String>,
    /// Sticky: true once an AI process has been observed in this terminal
    /// (via process-tree scan, OSC 1337, or restored from registry).
    /// Never cleared on exit. Used by ReconnectAi to distinguish
    /// "had-AI-now-exited" (reconnect makes sense) from "bare-shell"
    /// (don't summon Claude out of nothing).
    pub had_ai_session: bool,
    /// This terminal's WINDOW_ID — used to match context files
    window_id: String,
    /// Persistent process sampler, reused across scans. Kept alive so per-process
    /// CPU% is a delta over the scan interval, and so detection never forks `ps`.
    /// Cross-platform (macOS/Linux) via the `sysinfo` crate.
    sys: System,
}

impl ClaudeTracker {
    pub fn new(window_id: &str) -> Self {
        Self {
            claude_pid: None,
            detected_tool: None,
            session_id: None,
            start_time: None,
            rss_kb: 0,
            cpu_percent: 0.0,
            api_stats: ClaudeApiStats::default(),
            permission_mode: None,
            had_ai_session: false,
            window_id: window_id.to_string(),
            sys: System::new(),
        }
    }

    /// Perform a scan of the process tree and AI tool state.
    /// Returns true if the AI tool started, stopped, or session ID changed.
    pub fn scan(&mut self, shell_pid: u32) -> bool {
        let old_pid = self.claude_pid;
        let old_session = self.session_id.clone();

        // Fast path: if we already know the AI pid, just confirm it's still
        // alive (and still an AI tool — guards pid reuse) and refresh RSS/CPU.
        // This is a single in-process probe — no `ps` fork, no full-tree walk —
        // which is what runs on the vast majority of 10s ticks.
        // (When the pid is known but the probe fails — process gone or pid reused
        // by a non-AI process — this condition is false and we fall through to
        // the discovery/clear path below.)
        if let Some(pid) = self.claude_pid
            && let Some((tool, rss, cpu)) = self.probe_pid(pid)
        {
            self.detected_tool = Some(tool);
            self.rss_kb = rss;
            self.cpu_percent = cpu;
            return self.claude_pid != old_pid || self.session_id != old_session;
        }

        // Discovery path: enumerate the process tree once and BFS for an AI
        // descendant. Only runs when we have no live pid (startup or after exit).
        match self.detect_ai_in_tree(shell_pid) {
            Some((pid, tool)) => {
                // AI tool is running
                if old_pid.is_none() {
                    info!("{} process detected (PID {})", tool.name(), pid);
                    self.start_time = Some(Instant::now());
                    self.had_ai_session = true;
                }
                self.claude_pid = Some(pid);
                self.detected_tool = Some(tool);

                // Seed RSS/CPU from the same in-process sampler.
                if let Some((_, rss, cpu)) = self.probe_pid(pid) {
                    self.rss_kb = rss;
                    self.cpu_percent = cpu;
                }

                // API stats (session_id, model, cost, ctx%) arrive via OSC 1337;ImmorTerm
                // through the PTY — no polling or context file scanning needed.
            }
            None => {
                // No AI tool running
                if old_pid.is_some() {
                    info!(
                        "{} process exited",
                        self.detected_tool.map(|t| t.name()).unwrap_or("AI")
                    );
                    // Keep session_id for restore — clear everything else
                    self.claude_pid = None;
                    self.detected_tool = None;
                    self.start_time = None;
                    self.rss_kb = 0;
                    self.cpu_percent = 0.0;
                    self.api_stats = ClaudeApiStats::default();
                    self.permission_mode = None;
                } else if !self.api_stats.model.is_empty() {
                    // Diagnostic: the push path says an AI is active (model set
                    // via statusline/IPC) but the BFS found no process. With the
                    // dual-root seeding this should not happen — if it logs, the
                    // scan IS running but detection still fails (investigate the
                    // tree), distinguishing this from the scan-not-firing case.
                    warn!(
                        "BFS found no AI process despite active push stats (shell_pid={}, daemon_pid={}) — model={}",
                        shell_pid,
                        std::process::id(),
                        self.api_stats.model
                    );
                }
            }
        }

        self.claude_pid != old_pid || self.session_id != old_session
    }

    /// Whether any AI tool is currently active.
    pub fn is_ai_active(&self) -> bool {
        self.claude_pid.is_some()
    }

    /// Returns the currently active AI tool, if any.
    pub fn active_tool(&self) -> Option<AiTool> {
        if self.claude_pid.is_some() {
            self.detected_tool
        } else {
            None
        }
    }

    /// Format process stats for status bar display: "Claude 240M 5% 1h23m"
    /// (or "Codex …", "Cursor …", etc.). Vendor name comes from
    /// `detected_tool.display_name()`; fall back to "AI" only when no tool
    /// has been detected yet (legacy / pre-classify state).
    pub fn format_process_stats(&self) -> String {
        if self.claude_pid.is_none() {
            return String::new();
        }

        let mem = if self.rss_kb >= 1_048_576 {
            format!("{:.1}G", self.rss_kb as f64 / 1_048_576.0)
        } else if self.rss_kb >= 1024 {
            format!("{}M", self.rss_kb / 1024)
        } else {
            format!("{}K", self.rss_kb)
        };

        let runtime = self
            .start_time
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);

        let time = if runtime >= 3600 {
            format!("{}h{}m", runtime / 3600, (runtime % 3600) / 60)
        } else if runtime >= 60 {
            format!("{}m", runtime / 60)
        } else {
            format!("{}s", runtime)
        };

        let vendor = self
            .detected_tool
            .map(|t| t.display_name())
            .unwrap_or("AI");
        format!("{} {} {:.0}% {}", vendor, mem, self.cpu_percent, time)
    }

    /// Format API stats for status bar: "Claude · Sonnet 4.6 · $1.23 · 45% ctx"
    /// (or "Codex · GPT-5 · …", "Cursor · Sonnet 4.6 · …" — the model
    /// alone doesn't tell you the vendor when one wraps another, e.g.
    /// Cursor running Sonnet vs Claude running Sonnet).
    pub fn format_api_stats(&self) -> String {
        if self.api_stats.model.is_empty() {
            return String::new();
        }

        let model = &self.api_stats.model;
        let cost = if self.api_stats.cost_usd >= 1.0 {
            format!("${:.2}", self.api_stats.cost_usd)
        } else if self.api_stats.cost_usd > 0.0 {
            format!("${:.3}", self.api_stats.cost_usd)
        } else {
            "$0".into()
        };
        let ctx = format!("{:.0}%", self.api_stats.context_pct);

        match self.detected_tool {
            Some(tool) => format!(
                "{} · {} · {} · {} ctx",
                tool.display_name(),
                model,
                cost,
                ctx
            ),
            None => format!("{} · {} · {} ctx", model, cost, ctx),
        }
    }

    /// Runtime in seconds since Claude was first detected.
    pub fn runtime_secs(&self) -> u64 {
        self.start_time
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0)
    }

    /// Probe a single known pid in-process: refresh just that process and return
    /// `(tool, rss_kb, cpu_pct)` iff it still exists AND still classifies as an
    /// AI tool. No subprocess. CPU% is usage since this pid's previous refresh
    /// (i.e. over the scan interval); the first probe after discovery reads ~0.
    fn probe_pid(&mut self, pid: u32) -> Option<(AiTool, u64, f32)> {
        let p = Pid::from_u32(pid);
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[p]),
            true,
            ProcessRefreshKind::nothing()
                .with_memory()
                .with_cpu()
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );
        let proc = self.sys.process(p)?;
        let (name, args) = proc_name_and_args(proc);
        let tool = classify_ai_process(&name, &args)?;
        // sysinfo reports memory in bytes (v0.30+); the status bar wants KB.
        let rss_kb = proc.memory() / 1024;
        Some((tool, rss_kb, proc.cpu_usage()))
    }

    /// Find an AI coding tool process that's a descendant of `shell_pid` (or of
    /// the daemon itself). Full in-process enumeration + BFS — replaces the old
    /// `ps -eo pid,ppid,args` fork. Only called on (re)discovery, not every tick.
    fn detect_ai_in_tree(&mut self, shell_pid: u32) -> Option<(u32, AiTool)> {
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );

        // Build parent → children adjacency, plus pid → (binary_name, full_args).
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut meta: HashMap<u32, (String, String)> = HashMap::new();
        for (pid, proc) in self.sys.processes() {
            let pidu = pid.as_u32();
            let ppid = proc.parent().map(|p| p.as_u32()).unwrap_or(0);
            children.entry(ppid).or_default().push(pidu);
            meta.insert(pidu, proc_name_and_args(proc));
        }

        // BFS from TWO roots: the recorded PTY child (shell_pid) and the daemon's
        // own pid. shell_pid can go stale on a respawned session, but the AI is
        // always a descendant of the daemon (daemon → shell → claude), so seeding
        // the daemon pid makes detection self-correct. The daemon owns exactly one
        // session, so its subtree can't bleed in another session's AI process.
        let mut queue = vec![shell_pid, std::process::id()];
        let mut visited = std::collections::HashSet::new();
        while let Some(pid) = queue.pop() {
            if !visited.insert(pid) {
                continue;
            }
            if let Some(kids) = children.get(&pid) {
                for &kid in kids {
                    if let Some((name, args)) = meta.get(&kid)
                        && let Some(tool) = classify_ai_process(name, args)
                    {
                        return Some((kid, tool));
                    }
                    queue.push(kid);
                }
            }
        }
        None
    }
}

// ─── Process tree detection ─────────────────────────────────────────

/// Classify a process name + args into an AI tool, if it matches.
fn classify_ai_process(name: &str, args: &str) -> Option<AiTool> {
    match name {
        "claude" => Some(AiTool::Claude),
        "aider" => Some(AiTool::Aider),
        "cursor" | "cursor-agent" => Some(AiTool::Cursor),
        "continue" => Some(AiTool::Continue),
        "cody" => Some(AiTool::Cody),
        // Codex CLI ships as `codex` (https://github.com/openai/codex).
        "codex" => Some(AiTool::Codex),
        // Windsurf TUI binaries seen as `windsurf-next`/`windsurf` in
        // process listings; both route to the same vendor.
        "windsurf" | "windsurf-next" => Some(AiTool::Windsurf),
        // opencode TUI by SST.
        "opencode" => Some(AiTool::Opencode),
        // Gemini CLI binary is `gemini`.
        "gemini" => Some(AiTool::Gemini),
        // GitHub Copilot CLI: either `copilot` directly or `gh copilot ...`.
        "copilot" => Some(AiTool::Copilot),
        "gh" => {
            if args.contains("copilot") {
                Some(AiTool::Copilot)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Extract `(binary_name, full_args)` from a sysinfo process, mirroring the old
/// `ps args`-based shape: `binary_name` is the basename of argv[0] (falling back
/// to the process name when the cmdline is empty), `full_args` is the whole
/// command line. `classify_ai_process` keys on both (e.g. `gh … copilot`), so
/// preserving this shape keeps classification identical to the `ps` version.
fn proc_name_and_args(proc: &sysinfo::Process) -> (String, String) {
    let cmd: Vec<String> = proc
        .cmd()
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let args = cmd.join(" ");
    let binary_name = cmd
        .first()
        .map(|a| a.rsplit('/').next().unwrap_or(a).to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| proc.name().to_string_lossy().into_owned());
    (binary_name, args)
}

// ─── Session ID from statusline context files (legacy, kept for reference) ───

/// Parsed context file written by `immorterm-statusline.sh`.
#[allow(dead_code)]
struct ContextFileMatch {
    session_id: String,
    api_stats: ClaudeApiStats,
    permission_mode: Option<String>,
}

/// Find Claude session ID by matching WINDOW_ID in context files.
///
/// Claude Code's `statusLine` feature calls `.claude/statusline.sh` which writes:
/// ```text
/// MODEL=Claude Opus 4
/// COST=1.23
/// CTX_PCT=45
/// TIMESTAMP=1708723456
/// WINDOW_ID=50369-ADj1BNMV
/// TRANSCRIPT_PATH=/Users/.../.claude/projects/.../abc123.jsonl
/// ```
///
/// We scan `/tmp/immorterm-claude-ctx-*` and match by WINDOW_ID.
/// If multiple files match (e.g., after /clear), we pick the freshest one.
/// Files older than 5 minutes are ignored (stale — Claude exited or /clear'd).
#[allow(dead_code)]
fn find_session_by_window_id(window_id: &str) -> Option<ContextFileMatch> {
    if window_id.is_empty() {
        return None;
    }

    let entries = match std::fs::read_dir("/tmp") {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read /tmp: {}", e);
            return None;
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut best: Option<(ContextFileMatch, u64)> = None; // (match, timestamp)

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if !name.starts_with("immorterm-claude-ctx-") {
            continue;
        }

        // Extract session ID from filename
        let session_id = name.strip_prefix("immorterm-claude-ctx-").unwrap_or("");
        if session_id.is_empty() {
            continue;
        }

        // Read and parse the context file
        let path = entry.path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let vars = parse_context_file(&content);

        // Check timestamp — skip stale files (>5 min)
        let ts: u64 = vars
            .get("TIMESTAMP")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if now.saturating_sub(ts) > 300 {
            continue;
        }

        // Match by WINDOW_ID
        let file_window_id = vars.get("WINDOW_ID").map(|s| s.as_str()).unwrap_or("");
        if file_window_id != window_id {
            continue;
        }

        // Build API stats
        let api_stats = ClaudeApiStats {
            model: vars.get("MODEL").cloned().unwrap_or_default(),
            cost_usd: vars
                .get("COST")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            context_pct: vars
                .get("CTX_PCT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            transcript_path: vars.get("TRANSCRIPT_PATH").cloned().unwrap_or_default(),
        };

        // Extract permission mode (may be empty)
        let permission_mode = vars
            .get("PERMISSION_MODE")
            .filter(|v| !v.is_empty())
            .cloned();

        // Keep the freshest match (handles /clear — old + new sessions share WINDOW_ID)
        let is_better = best.as_ref().map(|(_, t)| ts > *t).unwrap_or(true);
        if is_better {
            best = Some((
                ContextFileMatch {
                    session_id: session_id.to_string(),
                    api_stats,
                    permission_mode,
                },
                ts,
            ));
        }
    }

    best.map(|(m, _)| m)
}

/// Parse a KEY=VALUE context file into a HashMap.
#[allow(dead_code)]
fn parse_context_file(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in content.lines() {
        if let Some((key, value)) = line.split_once('=') {
            map.insert(key.to_string(), value.to_string());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt; // arg0
    use std::process::{Command, Stdio};

    #[test]
    fn classify_matches_known_tools() {
        assert_eq!(classify_ai_process("claude", "claude"), Some(AiTool::Claude));
        assert_eq!(classify_ai_process("gh", "gh copilot suggest"), Some(AiTool::Copilot));
        assert_eq!(classify_ai_process("gh", "gh pr list"), None);
        assert_eq!(classify_ai_process("zsh", "/bin/zsh"), None);
    }

    /// End-to-end check of the sysinfo-based detection: spawn a real child whose
    /// argv[0] is "claude" (the executable is `sleep`), then confirm the in-process
    /// BFS finds it and `probe_pid` reports it as a live AI tool with non-zero RSS
    /// — i.e. no `ps` fork, correct units, correct tree walk.
    #[test]
    fn detects_and_probes_fake_ai_child() {
        let mut child = Command::new("sleep");
        child
            .arg0("claude")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut handle = child.spawn().expect("spawn fake claude");
        let pid = handle.id();

        let mut tracker = ClaudeTracker::new("test-win");
        // BFS seeds from std::process::id() (this test process); the fake claude
        // is our direct child. Don't pin the exact pid — other tests run in
        // parallel within the same test process and may spawn their own fake
        // "claude" children, so BFS could return either. Assert it found *a*
        // Claude, then verify our specific pid via the pid-targeted probe.
        let found = tracker.detect_ai_in_tree(std::process::id());
        let probe = tracker.probe_pid(pid);

        let _ = handle.kill();
        let _ = handle.wait();

        assert!(
            matches!(found, Some((_, AiTool::Claude))),
            "BFS should find a Claude descendant, got {found:?}"
        );
        let (tool, rss_kb, _cpu) = probe.expect("probe should find the live pid");
        assert_eq!(tool, AiTool::Claude);
        assert!(rss_kb > 0, "RSS should be > 0 KB, got {rss_kb}");

        // After the child is reaped, probing its pid must report it gone.
        let mut tracker2 = ClaudeTracker::new("test-win");
        assert!(tracker2.probe_pid(pid).is_none(), "dead pid must probe as None");
    }

    /// Verify CPU% is real, not stuck at 0. sysinfo computes CPU as a delta
    /// against the pid's previous sample, and the pid needs to have existed for
    /// two prior refreshes before a usable delta appears — i.e. the first ~2
    /// probes read 0 (in the live daemon, the first ~20s after Claude is
    /// detected), then real values. We probe a busy-looping process several
    /// times with gaps > MINIMUM_CPU_UPDATE_INTERVAL and assert that at least one
    /// reading is non-zero (observed ~95% for a single-core busy loop).
    #[test]
    fn probe_reports_nonzero_cpu_for_busy_process() {
        // argv[0] = "claude" so it classifies; the executable burns one core.
        let mut child = Command::new("sh");
        child
            .arg0("claude")
            .arg("-c")
            .arg("while true; do :; done")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut handle = child.spawn().expect("spawn busy fake claude");
        let pid = handle.id();

        let mut tracker = ClaudeTracker::new("test-win");
        let mut readings = Vec::new();
        let mut rss_seen = 0u64;
        for _ in 0..5 {
            if let Some((_, rss, cpu)) = tracker.probe_pid(pid) {
                rss_seen = rss;
                readings.push(cpu);
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
        }

        let _ = handle.kill();
        let _ = handle.wait();

        assert!(rss_seen > 0, "busy process should report RSS, got {rss_seen}");
        assert!(
            readings.iter().any(|&c| c > 0.0),
            "busy process should report CPU% > 0 within 5 probes, got {readings:?}"
        );
    }
}
