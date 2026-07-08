//! SessionManager — direct port of `apps/extension/src/session-manager.ts`.
//!
//! Responsibilities (identical to the TS source):
//!   * Watch `~/.immorterm/registry.json` (debounced fs events, OS-native)
//!   * For each alive entry matching the configured project, open a typed
//!     adapter: `Ai` (WebSocket control channel to the Rust daemon) or
//!     `Regular` (polls `<project>/.immorterm/claude-ctx/<windowId>` every
//!     cycle).
//!   * Surface aggregated `ClaudeState` over `/api/v1/sessions/*` and push
//!     changes out via the same WebSocket channel the standalone webview
//!     subscribes to for live stats.
//!   * Push formatted mode0 / mode1 AI stats into the C-binary screen
//!     session's hardstatus (`aistats` screen command) with dedupe.
//!   * Auto-toggle the active-window stats mode every 30 s.
//!
//! Every TS branch has been preserved. Logic that diverges because the
//! underlying runtime is different (tokio-tungstenite instead of
//! `MinimalWsClient`, `notify` instead of `fs.watch`, an explicit command
//! for `process.kill(pid, 0)`) is flagged inline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::{broadcast, Mutex, RwLock};
use tokio::time::{interval, sleep};
use tokio_tungstenite::tungstenite::Message;

// ── Types (1:1 with TS `ClaudeState`/`SessionInfo`) ────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaudeState {
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(rename = "rssKb")]
    pub rss_kb: u64,
    #[serde(rename = "cpuPercent")]
    pub cpu_percent: f64,
    #[serde(rename = "runtimeSecs")]
    pub runtime_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(rename = "costUsd", skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(rename = "contextPct", skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<f64>,
    #[serde(rename = "transcriptPath", skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    #[serde(rename = "windowId")]
    pub window_id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "type")]
    pub kind: SessionKind,
    pub pid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Regular,
    Ai,
}

// ── Session-level event (broadcast to webviews) ────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum SessionEvent {
    ClaudeUpdate { window_id: String, state: ClaudeState },
    ClaudeExited { window_id: String },
    SessionClosing { window_id: String },
    SessionAdded { info: SessionInfo },
    SessionRemoved { window_id: String },
}

// ── AI (WebSocket) adapter ─────────────────────────────────────────────────

/// Port of `ImmorTermAiAdapter`. Connects to `ws://127.0.0.1:<port>`, sends
/// `{"type":"subscribe_control"}` after the handshake completes, and parses
/// `control_hello` / `control_event` frames.
struct AiAdapter {
    window_id: String,
    ws_port: u16,
    display_name: Arc<RwLock<String>>,
    pid: u32,
    claude_state: Arc<RwLock<Option<ClaudeState>>>,
    events: broadcast::Sender<SessionEvent>,
    cancel: tokio_util::sync::CancellationToken,
}

impl AiAdapter {
    fn new(
        window_id: String,
        ws_port: u16,
        display_name: String,
        pid: u32,
        events: broadcast::Sender<SessionEvent>,
    ) -> Self {
        Self {
            window_id,
            ws_port,
            display_name: Arc::new(RwLock::new(display_name)),
            pid,
            claude_state: Arc::new(RwLock::new(None)),
            events,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// Equivalent to the TS constructor: spawn the reconnect loop.
    fn start(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut delay_ms = 1000u64;
            loop {
                if this.cancel.is_cancelled() { break; }
                let url = format!("ws://127.0.0.1:{}", this.ws_port);
                match tokio_tungstenite::connect_async(&url).await {
                    Ok((mut ws, _resp)) => {
                        // Mirror TS: small delay, then subscribe_control.
                        sleep(Duration::from_millis(100)).await;
                        let _ = ws
                            .send(Message::Text(
                                json!({ "type": "subscribe_control" }).to_string(),
                            ))
                            .await;
                        delay_ms = 1000; // reset backoff on successful connect

                        while let Some(frame) = ws.next().await {
                            if this.cancel.is_cancelled() { break; }
                            let frame = match frame { Ok(f) => f, Err(_) => break };
                            match frame {
                                Message::Text(t) => { this.handle_message(&t).await; }
                                Message::Ping(p) => { let _ = ws.send(Message::Pong(p)).await; }
                                Message::Close(_) => break,
                                _ => {}
                            }
                        }
                    }
                    Err(_) => { /* fall through to reconnect backoff */ }
                }
                if this.cancel.is_cancelled() { break; }
                sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms * 2).min(30_000);
            }
        });
    }

    async fn handle_message(&self, data: &str) {
        let msg: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return,
        };
        let ty = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "control_hello" {
            if let Some(dn) = msg.get("display_name").and_then(|v| v.as_str()) {
                *self.display_name.write().await = dn.to_string();
            }
            if let Some(raw) = msg.get("claude") {
                let parsed = parse_claude_state(raw);
                *self.claude_state.write().await = Some(parsed.clone());
                let _ = self.events.send(SessionEvent::ClaudeUpdate {
                    window_id: self.window_id.clone(),
                    state: parsed,
                });
            }
        } else if ty == "control_event" {
            match msg.get("event").and_then(|v| v.as_str()).unwrap_or("") {
                "claude_update" => {
                    if let Some(raw) = msg.get("claude") {
                        let parsed = parse_claude_state(raw);
                        *self.claude_state.write().await = Some(parsed.clone());
                        let _ = self.events.send(SessionEvent::ClaudeUpdate {
                            window_id: self.window_id.clone(),
                            state: parsed,
                        });
                    }
                }
                "claude_exited" => {
                    *self.claude_state.write().await = None;
                    let _ = self
                        .events
                        .send(SessionEvent::ClaudeExited { window_id: self.window_id.clone() });
                }
                "session_closing" => {
                    let _ = self
                        .events
                        .send(SessionEvent::SessionClosing { window_id: self.window_id.clone() });
                }
                _ => {}
            }
        }
    }

    async fn claude_state(&self) -> Option<ClaudeState> {
        self.claude_state.read().await.clone()
    }
    fn window_id(&self) -> &str { &self.window_id }
    fn pid(&self) -> u32 { self.pid }
    async fn session_info(&self) -> SessionInfo {
        SessionInfo {
            window_id: self.window_id.clone(),
            display_name: self.display_name.read().await.clone(),
            kind: SessionKind::Ai,
            pid: self.pid,
        }
    }
    fn dispose(&self) { self.cancel.cancel(); }
}

fn parse_claude_state(raw: &Value) -> ClaudeState {
    ClaudeState {
        active: raw.get("active").and_then(|v| v.as_bool()).unwrap_or(false),
        pid: raw.get("pid").and_then(|v| v.as_u64()).map(|x| x as u32),
        session_id: raw.get("session_id").and_then(|v| v.as_str()).map(String::from),
        rss_kb: raw.get("rss_kb").and_then(|v| v.as_u64()).unwrap_or(0),
        cpu_percent: raw.get("cpu_percent").and_then(|v| v.as_f64()).unwrap_or(0.0),
        runtime_secs: raw.get("runtime_secs").and_then(|v| v.as_u64()).unwrap_or(0),
        model: raw.get("model").and_then(|v| v.as_str()).map(String::from),
        cost_usd: raw.get("cost_usd").and_then(|v| v.as_f64()),
        context_pct: raw.get("context_pct").and_then(|v| v.as_f64()),
        transcript_path: raw.get("transcript_path").and_then(|v| v.as_str()).map(String::from),
    }
}

// ── Regular (C-binary) adapter ─────────────────────────────────────────────

/// Port of `ImmorTermAdapter`. Reads
/// `<project>/.immorterm/claude-ctx/<windowId>` on demand (the consolidated
/// 30 s poll lives in `SessionManager::poll_all_context_files`).
struct RegularAdapter {
    window_id: String,
    display_name: String,
    pid: u32,
    context_file: PathBuf,
    claude_state: std::sync::Mutex<Option<ClaudeState>>,
    events: broadcast::Sender<SessionEvent>,
}

impl RegularAdapter {
    fn new(
        window_id: String,
        display_name: String,
        pid: u32,
        project_dir: &str,
        events: broadcast::Sender<SessionEvent>,
    ) -> Self {
        let context_file = PathBuf::from(project_dir)
            .join(".immorterm")
            .join("claude-ctx")
            .join(&window_id);
        let adapter = Self {
            window_id,
            display_name,
            pid,
            context_file,
            claude_state: std::sync::Mutex::new(None),
            events,
        };
        adapter.read_context_file_blocking();
        adapter
    }

    fn read_context_file_blocking(&self) {
        match std::fs::read_to_string(&self.context_file) {
            Ok(content) => {
                let vars = parse_key_value(&content);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let ts: u64 = vars.get("TIMESTAMP").and_then(|s| s.parse().ok()).unwrap_or(0);
                if now.saturating_sub(ts) > 300 {
                    self.clear_and_emit_exited();
                    return;
                }
                let state = ClaudeState {
                    active: true,
                    pid: None,
                    session_id: vars.get("SESSION_ID").cloned(),
                    rss_kb: vars.get("RSS_KB").and_then(|s| s.parse().ok()).unwrap_or(0),
                    cpu_percent: vars
                        .get("CPU_PCT")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0.0),
                    runtime_secs: vars
                        .get("RUNTIME_SECS")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                    model: vars.get("MODEL").cloned(),
                    cost_usd: vars.get("COST").and_then(|s| s.parse().ok()),
                    context_pct: vars.get("CTX_PCT").and_then(|s| s.parse().ok()),
                    transcript_path: vars.get("TRANSCRIPT_PATH").cloned(),
                };
                *self.claude_state.lock().unwrap() = Some(state.clone());
                let _ = self.events.send(SessionEvent::ClaudeUpdate {
                    window_id: self.window_id.clone(),
                    state,
                });
            }
            Err(_) => self.clear_and_emit_exited(),
        }
    }

    fn clear_and_emit_exited(&self) {
        let mut g = self.claude_state.lock().unwrap();
        let was_active = g.as_ref().map(|s| s.active).unwrap_or(false);
        *g = None;
        drop(g);
        if was_active {
            let _ = self
                .events
                .send(SessionEvent::ClaudeExited { window_id: self.window_id.clone() });
        }
    }

    fn claude_state(&self) -> Option<ClaudeState> {
        self.claude_state.lock().unwrap().clone()
    }
    fn session_info(&self) -> SessionInfo {
        SessionInfo {
            window_id: self.window_id.clone(),
            display_name: self.display_name.clone(),
            kind: SessionKind::Regular,
            pid: self.pid,
        }
    }
    fn pid(&self) -> u32 { self.pid }
}

fn parse_key_value(content: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in content.lines() {
        if let Some(idx) = line.find('=') {
            let k = &line[..idx];
            let v = &line[idx + 1..];
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

// ── Stats formatters (mirror gpu-terminal.html exactly) ────────────────────

fn format_memory(kb: u64) -> String {
    if kb >= 1_048_576 {
        format!("{:.1}G", (kb as f64) / 1_048_576.0)
    } else {
        format!("{}M", (kb + 512) / 1024)
    }
}

fn format_runtime(secs: u64) -> String {
    if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m > 0 { format!("{}h{}m", h, m) } else { format!("{}h", h) }
    } else {
        let m = secs / 60;
        let s = secs % 60;
        if s > 0 { format!("{}m{}s", m, s) } else { format!("{}m", m) }
    }
}

fn get_ctx_bar_color(pct: f64) -> &'static str {
    if pct >= 95.0 { "#FF0000" }
    else if pct >= 85.0 { "#FF3333" }
    else if pct >= 70.0 { "#FF6B00" }
    else if pct >= 50.0 { "#FFB800" }
    else { "#00CC44" }
}

fn clr(hex: &str) -> String { format!("\x03{}", hex) }
const CLR_POP: &str = "\x03-";

/// Mode 0 — "🔋 RAM:240M CPU:5% 1h23m"
pub fn format_process_stats(c: &ClaudeState) -> String {
    format!(
        "\u{1F50B} RAM:{} CPU:{}% {}",
        format_memory(c.rss_kb),
        c.cpu_percent.round() as i64,
        format_runtime(c.runtime_secs),
    )
}

/// Mode 1 — "🤖 CTX: ▰▰▰▰▱▱▱▱▱▱ 42%" with inline \x03#RRGGBB escapes.
pub fn format_api_stats(c: &ClaudeState) -> String {
    let pct = match c.context_pct { Some(p) => p, None => return String::new() };
    let bar_width = 10usize;
    let filled = ((pct / 100.0) * bar_width as f64).round() as usize;
    let empty = bar_width.saturating_sub(filled);
    let fill_color = get_ctx_bar_color(pct);
    let empty_color = "#444444";
    let mut bar = String::new();
    if filled > 0 {
        bar.push_str(&clr(fill_color));
        for _ in 0..filled { bar.push('\u{25B0}'); }
    }
    if empty > 0 {
        bar.push_str(&clr(empty_color));
        for _ in 0..empty { bar.push('\u{25B1}'); }
    }
    bar.push_str(CLR_POP);
    format!("\u{1F916} CTX: {} {}%", bar, pct.round() as i64)
}

// ── SessionManager ─────────────────────────────────────────────────────────

// RegularAdapter is materially smaller than Arc<AiAdapter>; boxing the
// larger variant (AiAdapter is already heap via Arc) keeps Clippy happy
// without changing behaviour — AiAdapter access still goes through Arc.
#[allow(clippy::large_enum_variant)]
enum Adapter {
    Ai(Arc<AiAdapter>),
    Regular(RegularAdapter),
}

/// Process-global registry of SessionManagers keyed by project_dir so the
/// multi-tab Tauri app can juggle N projects without a fresh hub per tab.
/// Lazy-created on first call to `manager_for()`. The claude_tracker's 30s
/// loop is spawned alongside the manager at creation time.
static MANAGERS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<SessionManager>>>,
> = std::sync::OnceLock::new();

fn managers_slot() -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<SessionManager>>>
{
    MANAGERS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Return (and lazily create) the SessionManager for this project. The
/// project_name is derived from the final path component — matches how
/// the filter in load_registry distinguishes same-basename projects across
/// different parent dirs. Cheap after first call.
pub fn manager_for(project_dir: &str) -> Arc<SessionManager> {
    let key = project_dir.to_string();
    let mut guard = managers_slot().lock().unwrap();
    if let Some(mgr) = guard.get(&key) {
        return mgr.clone();
    }
    let project_name = std::path::Path::new(project_dir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "terminal".to_string());
    let mgr = SessionManager::new(project_name, project_dir.to_string());
    crate::claude_tracker::start(mgr.clone());
    guard.insert(key, mgr.clone());
    mgr
}

/// Process-global "which sidebar row is the user looking at" — written by
/// the /api/v1/registry/active-window endpoint, read by any SessionManager
/// that spins up. Separate from `SessionManager::active_window_id` so the
/// REST handler doesn't need an Arc<SessionManager> threaded through state.
static GLOBAL_ACTIVE_WINDOW: std::sync::OnceLock<std::sync::Mutex<Option<String>>> = std::sync::OnceLock::new();

fn global_active_slot() -> &'static std::sync::Mutex<Option<String>> {
    GLOBAL_ACTIVE_WINDOW.get_or_init(|| std::sync::Mutex::new(None))
}

pub fn set_global_active_window_id(wid: Option<String>) {
    if let Ok(mut g) = global_active_slot().lock() {
        *g = wid;
    }
}

pub fn get_global_active_window_id() -> Option<String> {
    global_active_slot().lock().ok().and_then(|g| g.clone())
}

pub struct SessionManager {
    project_name: String,
    project_path: String,
    screen_bin: String,
    adapters: Mutex<HashMap<String, Adapter>>,
    last_pushed_stats: Mutex<HashMap<String, String>>,
    active_window_id: Mutex<Option<String>>,
    events: broadcast::Sender<SessionEvent>,
    _registry_watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

impl SessionManager {
    pub fn new(project_name: impl Into<String>, project_path: impl Into<String>) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(128);
        let mgr = Arc::new(Self {
            project_name: project_name.into(),
            project_path: project_path.into(),
            screen_bin: "immorterm".into(),
            adapters: Mutex::new(HashMap::new()),
            last_pushed_stats: Mutex::new(HashMap::new()),
            active_window_id: Mutex::new(None),
            events: tx,
            _registry_watcher: Mutex::new(None),
        });
        mgr.clone().spawn_background_loops();
        mgr
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> { self.events.subscribe() }

    fn registry_path() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
            .join(".immorterm")
            .join("registry.json")
    }

    fn spawn_background_loops(self: Arc<Self>) {
        // 1. Registry fs watcher — debounce 200ms, match TS behavior.
        let path = Self::registry_path();
        let mgr = Arc::clone(&self);
        tokio::spawn(async move {
            let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
            let ev_tx_watcher = ev_tx.clone();
            let mut debounce: Option<tokio::task::JoinHandle<()>> = None;
            let watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
                if let Ok(ev) = res {
                    if matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)) {
                        let _ = ev_tx_watcher.send(());
                    }
                }
            });
            if let Ok(mut w) = watcher {
                // Watch the parent dir — file may be recreated atomically.
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                    let _ = w.watch(parent, RecursiveMode::NonRecursive);
                }
                *mgr._registry_watcher.lock().await = Some(w);
            }
            // Prime the table once.
            mgr.load_registry().await;
            while ev_rx.recv().await.is_some() {
                if let Some(h) = debounce.take() { h.abort(); }
                let mgr2 = Arc::clone(&mgr);
                debounce = Some(tokio::spawn(async move {
                    sleep(Duration::from_millis(200)).await;
                    mgr2.load_registry().await;
                }));
            }
        });

        // 2. Consolidated 30s poll: refresh regular-session ctx files.
        let mgr = Arc::clone(&self);
        tokio::spawn(async move {
            let mut t = interval(Duration::from_secs(30));
            loop {
                t.tick().await;
                mgr.poll_all_context_files().await;
            }
        });

        // 3. Auto stats toggle (30 s) — only fires if the active window
        //    is a regular (C-binary) session with an active Claude state.
        let mgr = Arc::clone(&self);
        tokio::spawn(async move {
            let mut t = interval(Duration::from_secs(30));
            loop {
                t.tick().await;
                let wid = mgr.active_window_id.lock().await.clone();
                let Some(wid) = wid else { continue };
                let adapters = mgr.adapters.lock().await;
                let Some(Adapter::Regular(r)) = adapters.get(&wid) else { continue };
                let state = r.claude_state();
                drop(adapters);
                if !state.map(|s| s.active).unwrap_or(false) { continue; }
                let session_name = format!("{}-{}", mgr.project_name, wid);
                let _ = Command::new(&mgr.screen_bin)
                    .args(["-S", &session_name, "-X", "eval", "aistatstoggle"])
                    .output().await;
            }
        });
    }

    pub async fn set_active_window_id(&self, wid: Option<String>) {
        *self.active_window_id.lock().await = wid;
    }

    pub async fn get_session_type(&self, wid: &str) -> Option<SessionKind> {
        let adapters = self.adapters.lock().await;
        adapters.get(wid).map(|a| match a {
            Adapter::Ai(_) => SessionKind::Ai,
            Adapter::Regular(_) => SessionKind::Regular,
        })
    }

    pub async fn all_sessions(&self) -> Vec<SessionInfo> {
        let adapters = self.adapters.lock().await;
        let mut out = Vec::with_capacity(adapters.len());
        for a in adapters.values() {
            out.push(match a {
                Adapter::Ai(x) => x.session_info().await,
                Adapter::Regular(x) => x.session_info(),
            });
        }
        out
    }

    pub async fn is_alive(&self, wid: &str) -> bool {
        let adapters = self.adapters.lock().await;
        let Some(a) = adapters.get(wid) else { return false };
        let pid = match a {
            Adapter::Ai(x) => x.pid(),
            Adapter::Regular(x) => x.pid(),
        };
        pid > 0 && is_process_alive(pid)
    }

    pub async fn claude_state(&self, wid: &str) -> Option<ClaudeState> {
        let adapters = self.adapters.lock().await;
        let a = adapters.get(wid)?;
        match a {
            Adapter::Ai(x) => x.claude_state().await,
            Adapter::Regular(x) => x.claude_state(),
        }
    }

    pub async fn all_claude_states(&self) -> HashMap<String, ClaudeState> {
        let mut out = HashMap::new();
        let adapters = self.adapters.lock().await;
        for (wid, a) in adapters.iter() {
            let state = match a {
                Adapter::Ai(x) => x.claude_state().await,
                Adapter::Regular(x) => x.claude_state(),
            };
            if let Some(s) = state { out.insert(wid.clone(), s); }
        }
        out
    }

    pub async fn poll_all_context_files(&self) {
        let adapters = self.adapters.lock().await;
        for a in adapters.values() {
            if let Adapter::Regular(r) = a { r.read_context_file_blocking(); }
        }
    }

    /// Same filter as the TS loadRegistry(): project_dir exact match OR
    /// ends_with `/<project_name>` — two regressions have been fixed here
    /// by keeping the identical check.
    async fn load_registry(&self) {
        let Ok(raw) = std::fs::read_to_string(Self::registry_path()) else { return };
        let Ok(registry): Result<Value, _> = serde_json::from_str(&raw) else { return };
        let Some(sessions) = registry.get("sessions").and_then(|s| s.as_array()) else { return };
        let relevant: Vec<&Value> = sessions
            .iter()
            .filter(|s| {
                let pd = s.get("project_dir").and_then(|v| v.as_str()).unwrap_or("");
                pd == self.project_path
                    || pd.ends_with(&format!("/{}", self.project_name))
            })
            .collect();

        let mut active_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut adapters = self.adapters.lock().await;
        for entry in &relevant {
            let Some(wid) = entry.get("window_id").and_then(|v| v.as_str()) else { continue };
            active_ids.insert(wid.to_string());
            if adapters.contains_key(wid) { continue; }

            let session_type = entry
                .get("session_type")
                .and_then(|v| v.as_str())
                .or_else(|| entry.get("type").and_then(|v| v.as_str()))
                .unwrap_or("regular");
            let display_name = entry
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let pid = entry.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

            if session_type == "ai" {
                if let Some(ws_port) = entry.get("ws_port").and_then(|v| v.as_u64()) {
                    let adapter = Arc::new(AiAdapter::new(
                        wid.to_string(),
                        ws_port as u16,
                        display_name,
                        pid,
                        self.events.clone(),
                    ));
                    adapter.start();
                    let info = adapter.session_info().await;
                    let _ = self.events.send(SessionEvent::SessionAdded { info });
                    adapters.insert(wid.to_string(), Adapter::Ai(adapter));
                }
            } else {
                let adapter = RegularAdapter::new(
                    wid.to_string(),
                    display_name,
                    pid,
                    &self.project_path,
                    self.events.clone(),
                );
                let info = adapter.session_info();
                adapters.insert(wid.to_string(), Adapter::Regular(adapter));
                let _ = self.events.send(SessionEvent::SessionAdded { info });
            }
        }

        // Garbage-collect sessions no longer in the registry.
        let existing: Vec<String> = adapters.keys().cloned().collect();
        for wid in existing {
            if !active_ids.contains(&wid) {
                if let Some(adapter) = adapters.remove(&wid) {
                    if let Adapter::Ai(a) = adapter { a.dispose(); }
                    let _ = self.events.send(SessionEvent::SessionRemoved { window_id: wid.clone() });
                    let mut pushed = self.last_pushed_stats.lock().await;
                    pushed.remove(&wid);
                }
            }
        }
    }

    /// Push formatted mode0/mode1 AI stats into the C binary's screen
    /// hardstatus. Dedupe by concatenated key.
    pub async fn push_ai_stats(&self, window_id: &str, state: &ClaudeState) {
        if !state.active { return; }
        let mode0 = format_process_stats(state);
        let mode1 = format_api_stats(state);
        let key = format!("{}|{}", mode0, mode1);
        {
            let mut pushed = self.last_pushed_stats.lock().await;
            if pushed.get(window_id) == Some(&key) { return; }
            pushed.insert(window_id.to_string(), key);
        }
        let session_name = format!("{}-{}", self.project_name, window_id);
        let cmd = format!(
            "aistats \"{}\" \"{}\"",
            mode0.replace('"', ""),
            mode1.replace('"', ""),
        );
        let _ = Command::new(&self.screen_bin)
            .args(["-S", &session_name, "-X", "eval", &cmd])
            .output().await;
    }

    pub async fn clear_ai_stats(&self, window_id: &str) {
        self.last_pushed_stats.lock().await.remove(window_id);
        let session_name = format!("{}-{}", self.project_name, window_id);
        let _ = Command::new(&self.screen_bin)
            .args(["-S", &session_name, "-X", "eval", "aistats \"\" \"\""])
            .output().await;
    }
}

fn is_process_alive(pid: u32) -> bool {
    // Matches TS `process.kill(pid, 0)` — signal 0 only checks existence.
    // SAFETY: kill(2) is signal-safe; returns -1 + ESRCH when process is gone.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_formatter_matches_ts() {
        assert_eq!(format_memory(240 * 1024), "240M");
        assert_eq!(format_memory(2 * 1048576), "2.0G");
    }

    #[test]
    fn runtime_formatter_matches_ts() {
        assert_eq!(format_runtime(45), "0m45s");
        assert_eq!(format_runtime(60), "1m");
        assert_eq!(format_runtime(125), "2m5s");
        assert_eq!(format_runtime(3600), "1h");
        assert_eq!(format_runtime(3660), "1h1m");
    }

    #[test]
    fn ctx_bar_colors() {
        assert_eq!(get_ctx_bar_color(96.0), "#FF0000");
        assert_eq!(get_ctx_bar_color(90.0), "#FF3333");
        assert_eq!(get_ctx_bar_color(75.0), "#FF6B00");
        assert_eq!(get_ctx_bar_color(60.0), "#FFB800");
        assert_eq!(get_ctx_bar_color(10.0), "#00CC44");
    }

    #[test]
    fn api_stats_renders_bar() {
        let c = ClaudeState { context_pct: Some(42.0), ..Default::default() };
        let out = format_api_stats(&c);
        assert!(out.contains("CTX:"));
        assert!(out.contains("42%"));
        assert!(out.starts_with("\u{1F916}"));
    }
}
