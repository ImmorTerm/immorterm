//! IPC protocol — JSON messages over Unix socket.

use serde::{Deserialize, Serialize};

use immorterm_core::subagent::SubagentInfo;

/// Summary of a team for the ListTeams response.
#[derive(Debug, Serialize, Deserialize)]
pub struct TeamSummary {
    pub name: String,
    pub description: String,
    pub member_count: usize,
    pub task_counts: (usize, usize, usize), // (pending, in_progress, completed)
}

/// Client → Daemon request.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    /// Health check
    Ping,
    /// Get session info
    GetInfo,
    /// Execute a screen command (-X)
    Execute {
        command: String,
        args: Vec<String>,
    },
    /// Query a value (-Q)
    Query {
        command: String,
    },
    /// Attach to the session (bidirectional relay)
    Attach {
        cols: u16,
        rows: u16,
    },
    /// Detach from the session
    Detach,
    /// Resize the terminal
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Kill the session
    Kill,
    /// Read the current screen content (viewport + cursor)
    ReadScreen,
    /// Read scrollback history, optionally filtered by pattern
    ReadScrollback {
        lines: usize,
        pattern: Option<String>,
    },
    /// Get the current working directory (from OSC 7)
    GetCwd,
    /// Get the last command's exit code (from OSC 133;D)
    GetExitCode,
    /// Wait for a pattern to appear in terminal output
    WaitFor {
        pattern: String,
        timeout_ms: u64,
    },
    /// Get status bar data (project, stats, theme, activity)
    GetStatusBar,
    /// Get Claude Code process info (session ID, PID, stats)
    GetClaudeInfo,
    /// Display a PNG image inline in the terminal (Kitty graphics).
    ShowImage {
        /// Base64-encoded PNG data
        png_data: String,
        /// Column position (default: cursor col)
        col: Option<usize>,
        /// Row position (default: cursor row)
        row: Option<usize>,
        /// Display width in columns (default: auto from image)
        width: Option<usize>,
        /// Display height in rows (default: auto from image)
        height: Option<usize>,
    },
    /// Add an annotation overlay (highlighted region with label).
    AddAnnotation {
        col: usize,
        row: usize,
        width: usize,
        height: usize,
        /// Border color [R, G, B, A] (default: yellow)
        color: Option<[f32; 4]>,
        label: String,
    },
    /// Show a chart overlay (sparkline or bar chart).
    ShowChart {
        col: usize,
        row: usize,
        width: usize,
        height: usize,
        /// Data values (will be normalized to 0.0-1.0)
        values: Vec<f32>,
        /// Chart type: "sparkline" or "bar"
        chart_type: String,
        /// Chart color [R, G, B, A] (default: cyan)
        color: Option<[f32; 4]>,
    },
    /// Remove all overlays (annotations + charts).
    ClearOverlays,
    /// Query terminal capabilities.
    GetCapabilities,
    /// Subscribe to raw PTY output. Returns initial `Subscribed` response, then
    /// streams length-prefixed chunks (4-byte BE length + raw bytes) until disconnect.
    /// Used by the GUI window to receive terminal output from the daemon.
    SubscribeOutput,
    /// Subscribe for keyboard input relay. After initial `Subscribed` response,
    /// all raw bytes sent on this connection are forwarded to the PTY.
    /// Used by the GUI window to send keyboard input to the daemon.
    SubscribeInput,
    /// Render the terminal to a PNG screenshot and return as base64.
    Screenshot {
        /// Include status bar in screenshot (default: true)
        include_status_bar: bool,
        /// Custom width in pixels (default: auto from terminal cols)
        width: Option<u32>,
        /// Custom height in pixels (default: auto from terminal rows)
        height: Option<u32>,
    },
    /// Dump serialized terminal state for client-side rendering.
    ///
    /// Returns the full terminal snapshot (grid, scrollback, cursor, modes,
    /// overlays) that a client can use to render a screenshot with GPU access.
    /// The daemon can't use Metal/GPU because it's a double-forked process
    /// disconnected from WindowServer.
    DumpState,
    /// Push Claude session data from statusline script (event-driven, no polling).
    ///
    /// Called by `immorterm claude-push` which receives JSON from Claude Code's
    /// statusLine feature. This is the primary path — the daemon stores the data
    /// immediately instead of polling /tmp context files.
    UpdateClaudeSession {
        /// Claude's session UUID
        session_id: String,
        /// Model display name (e.g., "Claude Opus 4")
        model: String,
        /// Total cost in USD
        cost_usd: f64,
        /// Context window usage percentage (0-100)
        context_pct: f64,
        /// Path to the JSONL transcript file
        transcript_path: String,
        /// Permission mode (e.g., "delegate", "plan", "default"). None = unchanged.
        permission_mode: Option<String>,
    },

    /// Update just the permission mode (from hook or CLI).
    UpdatePermissionMode {
        mode: String,
    },

    /// Get detected subagents for the current Claude session.
    GetSubagents,

    // ─── AI Canvas Layer ─────────────────────────────────────────────

    /// Draw a filled rectangle with optional border on the AI canvas.
    DrawRect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        /// Fill color [R, G, B, A] (0.0-1.0)
        color: [f32; 4],
        /// Optional border color
        border_color: Option<[f32; 4]>,
        /// Border width in pixels (0 = no border)
        border_width: Option<f32>,
        /// Anchor mode: "fixed" (default) or "scroll"
        anchor: Option<String>,
        /// Copy scroll anchor from an existing primitive (overrides anchor)
        anchor_to: Option<u32>,
        /// Optional element name for event matching (e.g., "sidebar-bg")
        name: Option<String>,
    },
    /// Draw text at pixel coordinates on the AI canvas.
    DrawText {
        text: String,
        x: f32,
        y: f32,
        /// Text color [R, G, B, A]
        color: [f32; 4],
        /// Font size scale (1.0 = normal)
        font_size_scale: Option<f32>,
        /// Anchor mode: "fixed" (default) or "scroll"
        anchor: Option<String>,
        /// Copy scroll anchor from an existing primitive (overrides anchor)
        anchor_to: Option<u32>,
        /// Optional element name for event matching
        name: Option<String>,
    },
    /// Draw a clickable button on the AI canvas.
    DrawButton {
        text: String,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        /// Background color [R, G, B, A]
        bg_color: [f32; 4],
        /// Text color [R, G, B, A]
        text_color: [f32; 4],
        /// Anchor mode: "fixed" (default) or "scroll"
        anchor: Option<String>,
        /// Copy scroll anchor from an existing primitive (overrides anchor)
        anchor_to: Option<u32>,
        /// Optional element name for event matching (e.g., "approve-btn", "design-a")
        name: Option<String>,
    },
    /// Draw a line between two points on the AI canvas.
    DrawLine {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        /// Line color [R, G, B, A]
        color: [f32; 4],
        /// Line thickness in pixels
        thickness: Option<f32>,
        /// Anchor mode: "fixed" (default) or "scroll"
        anchor: Option<String>,
        /// Copy scroll anchor from an existing primitive (overrides anchor)
        anchor_to: Option<u32>,
        /// Optional element name for event matching
        name: Option<String>,
    },
    /// Draw an HTML/CSS component on the AI canvas (DOM overlay, no GPU).
    DrawHtml {
        /// HTML content to render.
        html: String,
        /// CSS styles (scoped to this primitive's wrapper).
        css: String,
        x: f32,
        y: f32,
        /// Width in pixels (0 = auto).
        width: f32,
        /// Height in pixels (0 = auto).
        height: f32,
        /// Anchor mode: "fixed" (default) or "scroll"
        anchor: Option<String>,
        /// Copy scroll anchor from an existing primitive (overrides anchor)
        anchor_to: Option<u32>,
        /// Optional element name for event matching
        name: Option<String>,
        /// Optional prompt template auto-written to the Claude PTY on each
        /// `data-click` activation. Same mechanism as workshops — see
        /// `Request::OpenWorkshop::on_click_prompt` doc. Placeholders:
        /// `{data_click}`, `{id}`.
        #[serde(default)]
        on_click_prompt: Option<String>,
        /// Hook-injection variant — see `Request::OpenWorkshop::on_click_inject_context`.
        #[serde(default)]
        on_click_inject_context: Option<String>,
    },
    /// Remove a specific AI primitive by ID.
    RemoveAiPrimitive {
        id: u32,
    },
    /// Run a JS snippet inside an existing HTML primitive's Shadow DOM.
    /// Fire-and-forget: daemon broadcasts the snippet to all raw-mode WS
    /// clients; each client finds the card by primitive id and executes the
    /// JS with `root`, `wrapper`, `card`, `prim` in scope (same context as
    /// inline `<script>` blocks in `draw_html`).
    EvalInPrimitive {
        id: u32,
        js: String,
    },
    /// Open (or replace) a Workshop — a persistent, full-size webview pane
    /// living next to the terminal. Unlike `DrawHtml` overlays which are
    /// ephemeral and inline, a Workshop survives across response turns and
    /// is the AI's surface for "build me a real app I can iterate on" tasks.
    /// HTML is rendered inside an isolated Shadow DOM (same model as
    /// `DrawHtml`), persisted to `~/.immorterm/workshops/<session>/<name>.html`
    /// so it can be popped out into a real browser tab. Idempotent on `name`:
    /// re-opening with the same name replaces in place without flicker.
    OpenWorkshop {
        /// Stable identifier — drives sidebar entry, file path, event matching.
        name: String,
        /// HTML body. Inline styles or `<style>` tags only (Shadow DOM).
        html: String,
        /// Optional CSS injected ahead of the body (also Shadow-scoped).
        #[serde(default)]
        css: String,
        /// Optional prompt template auto-injected into the Claude PTY on each
        /// workshop button click. Placeholders: `{data_click}` (the clicked
        /// button's `data-click` attribute), `{name}` (the workshop name).
        /// When set, the daemon writes `<formatted-template>\n` to the session's
        /// PTY — Claude treats it as if the user typed the prompt and reacts
        /// immediately, with NO background bash needed. Leave None to keep the
        /// classic background-wait-event flow.
        #[serde(default)]
        on_click_prompt: Option<String>,
        /// Optional rich context template injected via UserPromptSubmit hook on
        /// each click. Same placeholders as on_click_prompt. Different mechanism:
        /// daemon writes a marker file then PTY-types a tiny trigger ('.') so
        /// Claude's UserPromptSubmit hook fires; the hook reads the marker and
        /// emits the full context as `additionalContext`. Terminal stays clean
        /// (only the dot is visible), conversation history is short, but Claude
        /// sees the rich context. Mutually exclusive with on_click_prompt — if
        /// both are set, hook path wins.
        #[serde(default)]
        on_click_inject_context: Option<String>,
    },
    /// Replace the HTML/CSS of an existing Workshop. Use for full-tree
    /// rewrites; for surgical updates prefer `EvalInWorkshop`.
    UpdateWorkshop {
        name: String,
        html: String,
        #[serde(default)]
        css: String,
    },
    /// Run a JS snippet inside an existing Workshop's Shadow DOM. Same
    /// execution context as inline `<script>` blocks (`root`, `wrapper`,
    /// `card`, `prim` available). The Workshop equivalent of
    /// `EvalInPrimitive` — turn-by-turn surgical mutation without redrawing
    /// the whole pane.
    EvalInWorkshop {
        name: String,
        js: String,
    },
    /// Tear down a Workshop — removes the sidebar entry, closes the panel,
    /// deletes the persisted HTML file. Returns Ok even if the name was
    /// already absent (idempotent close).
    CloseWorkshop {
        name: String,
    },
    /// List active Workshops (name + last-modified timestamp + html size).
    /// Used by the AI to discover what's open across turns.
    ListWorkshops,
    /// Read a single Workshop's html + css as last set by the daemon (the
    /// last `open_workshop` / `update_workshop` payload). Lets the AI
    /// re-orient on its own authored state without scrolling back through
    /// the conversation. CAVEAT: live DOM mutations from `eval_in_workshop`
    /// run inside the webview's Shadow DOM and are NOT reflected here — the
    /// daemon doesn't see them. To capture truly-live state we'd need a
    /// roundtrip to the webview; v1 returns last-full-write state.
    ReadWorkshop {
        name: String,
    },
    /// Fire-and-forget: a project-scoped Plan changed on disk
    /// (~/.immorterm/plans/<project>/<id>/). The daemon holds no plan state —
    /// it just fans a `plan_changed` envelope out over the workshop broadcast
    /// channel for the S4 Plans sidebar consumer, which is not built yet (no
    /// client consumes the envelope today). Responds Ok.
    NotifyPlanChanged {
        project: String,
        id: String,
        status: String,
        title: String,
        summary: String,
        unresolved_decisions: u64,
    },
    /// Clear all AI canvas content (primitives, animations, events).
    ClearAiLayer,
    /// List all AI canvas primitives with their full state.
    ListAiPrimitives,
    /// Update properties of an existing AI primitive.
    UpdateAiPrimitive {
        id: u32,
        x: Option<f32>,
        y: Option<f32>,
        width: Option<f32>,
        height: Option<f32>,
        color: Option<[f32; 4]>,
        text: Option<String>,
        visible: Option<bool>,
        alpha: Option<f32>,
    },
    /// Animate a property of an AI primitive over time.
    /// The daemon interpolates at 60fps — no per-frame IPC needed.
    AnimatePrimitive {
        primitive_id: u32,
        /// Property to animate: "x", "y", "width", "height", "alpha"
        property: String,
        from: f32,
        to: f32,
        /// Duration in milliseconds
        duration_ms: u32,
        /// Easing: "linear", "ease_in", "ease_out", "ease_in_out"
        easing: Option<String>,
    },
    /// Get the current viewport state (visible cells, cursor, dimensions).
    GetViewport {
        /// Whether to include cell text content
        include_text: bool,
    },
    /// Poll queued AI events (button clicks, hovers). Returns and clears the queue.
    PollAiEvents,
    /// Block until a specific AI event occurs (e.g., button click) or timeout.
    /// Used by background processes to wait for user interaction without polling.
    /// AI can filter by any combination: event type, numeric ID, or element name.
    WaitForAiEvent {
        /// "click" or "hover" (None = match any event type)
        event_type: Option<String>,
        /// Specific primitive ID to wait for (None = match any)
        primitive_id: Option<u32>,
        /// Element name to match (e.g., "approve-btn"). Matched against the
        /// primitive's `name` field set at draw time.
        name: Option<String>,
        /// Maximum wait time in milliseconds
        timeout_ms: u64,
    },

    /// Get the WebSocket streaming port for this session.
    GetWebSocketPort,

    // ─── Agent Teams ─────────────────────────────────────────────────

    /// List all active Claude Code teams (from `~/.claude/teams/`).
    ListTeams,
    /// Get full team state: config, tasks, messages, member statuses.
    GetTeamState {
        team_name: String,
    },
    /// Send a message to a teammate's inbox.
    SendTeamMessage {
        team_name: String,
        recipient: String,
        content: String,
    },

    /// Send a message to a paired interactive session via the channel inbox.
    ChannelReply {
        message: String,
    },

    /// Subscribe to AI layer state updates. Returns initial `Subscribed` response,
    /// then streams length-prefixed JSON (`Vec<AiPrimitive>`) whenever the AI canvas
    /// changes (draw/remove/animate). Used by the GUI window to sync AI canvas state
    /// from the daemon's terminal to its own renderer.
    SubscribeAiLayer,

    /// Take a structured grid snapshot on demand (from MCP or manual trigger).
    TakeSnapshot,

    // ─── AI Expression Protocol ──────────────────────────────────────

    /// Set the AI expression state (confidence, danger, mood, animation, color).
    /// Applied to all subsequent terminal cells until changed or reset.
    SetExpression {
        /// Text brightness (0.0 = invisible, 1.0 = full). None = unchanged.
        confidence: Option<f32>,
        /// Danger level: "none", "low", "medium", "high", "critical". None = unchanged.
        danger: Option<String>,
        /// Semantic mood: "neutral", "confident", "cautious", "creative", etc. None = unchanged.
        mood: Option<String>,
        /// Animation: "none", "pulse", "glow", "wave", "typewriter", etc. None = unchanged.
        animation: Option<String>,
        /// One-shot celebration: "confetti", "sparkle", "fireworks". None = no celebration.
        celebrate: Option<String>,
        /// Effect intensity (0.0-1.0). None = unchanged.
        intensity: Option<f32>,
        /// Explicit color override as hex string (e.g., "#ff0000"). None = use mood color.
        color: Option<String>,
        /// Reset all to defaults before applying.
        reset: bool,
    },
    /// Reset AI expression to defaults.
    ResetExpression,

    /// Set text alignment and/or paragraph direction for BiDi rendering.
    SetAlignment {
        /// "left", "right", "center", "auto" (None = unchanged)
        alignment: Option<String>,
        /// "ltr", "rtl", "auto" (None = unchanged)
        direction: Option<String>,
    },

    // ─── Audio ────────────────────────────────────────────────────────

    /// Play a named sound or custom audio file.
    PlaySound {
        /// Named sound: "chime", "alert", "click", "rumble", "fanfare", "ping", "tick"
        sound: Option<String>,
        /// Path to a custom WAV/OGG/MP3 file (used if `sound` is None)
        path: Option<String>,
    },
    /// Set audio volume (0-100).
    SetVolume {
        volume: u8,
    },
    /// Toggle mute state.
    ToggleMute,

    // ─── Self-driven browser screencast ──────────────────────────────
    // The browser lives in the MCP-server process, not the daemon. The MCP
    // process pumps CDP screencast frames + status through these requests; the
    // daemon just relays them to raw-mode webview clients over `control_tx`,
    // which the browser panel already listens for (`browser_frame` etc).

    /// Relay one screencast frame to the webview browser panel.
    BrowserFrame {
        /// Base64 screencast frame (JPEG q75 from envoyage). Field name kept as
        /// `png_base64` for wire compat; the webview panel sniffs the base64
        /// magic bytes for the MIME, so JPEG or PNG both render.
        png_base64: String,
        title: String,
        url: String,
        /// Monotonic sequence; the panel drops any frame <= the last shown.
        seq: u64,
    },
    /// Relay the AI-driving pause state to the panel.
    BrowserState {
        paused: bool,
    },
    /// Ask the human to take over the browser pane (handoff banner).
    BrowserHumanRequest {
        reason: String,
        #[serde(default)]
        instructions: Option<String>,
    },
    /// Glide the panel's "Mort" cursor to where the AI is about to act. Coords
    /// are PAGE CSS pixels; `action` ∈ {move, click, type, scroll}.
    BrowserCursor {
        x: f64,
        y: f64,
        action: String,
    },
    /// Show a short intent balloon in the panel (what the AI is doing now).
    BrowserNarration {
        text: String,
    },
    /// Relay the page's copied selection back to the panel so the webview writes
    /// it to the OS clipboard (response to a human Cmd/Ctrl+C on the frame).
    BrowserCopy {
        text: String,
    },
    /// Drain queued human→browser input the webview forwarded to the daemon
    /// (clicks/keys/scroll/pause). The MCP pump dispatches these to the live
    /// browser. Returns `BrowserInput` and clears the queue.
    PollBrowserInput,
}

/// One human-driven browser action forwarded webview → daemon → MCP pump.
/// Mirrors the webview's `browser_input` / `browser_control` wire messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum BrowserInputEvent {
    /// Click at page CSS pixels (webview already un-letterboxed to page space).
    Click { x: f64, y: f64 },
    /// A single named key (Enter/Tab/Backspace/Escape/Arrow*) or printable char.
    Key { key: String },
    /// Paste text into the focused field (Cmd/Ctrl+V). The webview reads the
    /// clipboard under the user gesture; the pump inserts it via Input.insertText.
    Paste { text: String },
    /// Copy the page's current selection (Cmd/Ctrl+C). The pump evals the
    /// selection and relays it back for the webview to write to the clipboard.
    Copy,
    /// Vertical wheel scroll by `dy` CSS pixels (positive = down).
    Scroll { dy: f64 },
    /// Panel pixel size changed (open / drag-resize / debounced). The MCP pump
    /// sets the browser viewport to match so the page fills the panel with no
    /// letterbox. CSS px.
    Resize { width: f64, height: f64 },
    /// pause / continue the AI's automation from the panel toggle.
    Control { action: String },
}

/// Daemon → Client response.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Response {
    Ok(String),
    Error(String),
    SessionInfo {
        name: String,
        pid: u32,
        attached: bool,
        title: String,
        cols: usize,
        rows: usize,
    },
    /// Screen viewport content (from ReadScreen)
    ScreenContent {
        lines: Vec<String>,
        cursor_row: usize,
        cursor_col: usize,
        cursor_visible: bool,
        cols: usize,
        rows: usize,
        title: String,
    },
    /// Scrollback history (from ReadScrollback)
    ScrollbackContent {
        lines: Vec<String>,
        total_lines: usize,
    },
    /// Terminal capabilities (from GetCapabilities)
    Capabilities {
        features: Vec<String>,
        version: String,
        renderer: String,
    },
    /// Initial response to SubscribeOutput/SubscribeInput.
    /// After this JSON response, the connection switches to raw streaming mode.
    Subscribed {
        cols: usize,
        rows: usize,
        title: String,
        project: String,
    },
    /// Screenshot PNG data (from Screenshot request)
    ScreenshotData {
        /// Base64-encoded PNG image
        png_base64: String,
        /// Image width in pixels
        width: u32,
        /// Image height in pixels
        height: u32,
    },
    /// Serialized terminal state for client-side GPU rendering.
    TerminalState {
        /// JSON-serialized TerminalSnapshot
        snapshot_json: String,
        /// Session name
        session_name: String,
        /// Status bar data: project name, AI stats, etc.
        status_bar_project: String,
        status_bar_ai_stats: String,
    },
    /// ID of a newly created AI primitive (from Draw* requests).
    PrimitiveId {
        id: u32,
    },
    /// Current viewport state (from GetViewport).
    ViewportState {
        /// Text content of visible rows (if requested)
        lines: Option<Vec<String>>,
        cursor_row: usize,
        cursor_col: usize,
        cursor_visible: bool,
        cols: usize,
        rows: usize,
        /// Number of AI primitives currently drawn
        ai_primitive_count: usize,
        /// Current theme name
        theme_name: String,
    },
    /// Queued AI events (from PollAiEvents).
    AiEvents {
        events: Vec<immorterm_core::ai_layer::AiEvent>,
    },
    /// Single AI event that matched a WaitForAiEvent filter.
    AiEventOccurred {
        event: immorterm_core::ai_layer::AiEvent,
    },
    /// WebSocket streaming port info (from GetWebSocketPort).
    WebSocketInfo {
        port: u16,
        url: String,
    },
    /// List of active teams (from ListTeams).
    TeamList {
        teams: Vec<TeamSummary>,
    },
    /// Full team state snapshot (from GetTeamState).
    TeamStateData {
        /// JSON-serialized TeamState (avoids pulling the full struct into IPC)
        state_json: String,
    },
    /// List of detected subagents (from GetSubagents).
    SubagentList {
        agents: Vec<SubagentInfo>,
    },
    /// List of AI canvas primitives (from ListAiPrimitives).
    AiPrimitiveList {
        /// JSON-serialized Vec<AiPrimitive>
        primitives_json: String,
    },
    /// List of active Workshops (from ListWorkshops).
    WorkshopList {
        /// JSON: [{name, html_size, modified_unix_ms}, ...]
        workshops_json: String,
    },
    /// Current state of a single Workshop (from ReadWorkshop).
    WorkshopState {
        name: String,
        html: String,
        css: String,
        modified_unix_ms: u64,
    },
    /// Grid snapshot taken (from TakeSnapshot)
    SnapshotTaken {
        /// Path to the grid log file
        grid_log_path: String,
    },
    /// Claude Code session info (from GetClaudeInfo)
    ClaudeInfo {
        /// Claude process PID (None if not running)
        claude_pid: Option<u32>,
        /// Claude session UUID
        session_id: Option<String>,
        /// RSS in kilobytes
        rss_kb: u64,
        /// CPU percentage
        cpu_percent: f32,
        /// Runtime in seconds since detection
        runtime_secs: u64,
        /// Whether Claude is currently running
        active: bool,
        /// Model display name (e.g., "Claude Opus 4")
        model: Option<String>,
        /// Total cost in USD
        cost_usd: Option<f64>,
        /// Context window usage percentage (0-100)
        context_pct: Option<f64>,
        /// Path to the JSONL transcript
        transcript_path: Option<String>,
        /// Current permission mode (e.g., "delegate", "plan", "default")
        permission_mode: Option<String>,
        /// Active vendor identifier — lowercase cross-codebase name
        /// ("claude", "codex", "cursor", "copilot", "windsurf", "cline",
        /// "opencode", "gemini", "aider"). Sent so the GPU terminal status
        /// bar and other consumers can prefix the AI stats line with the
        /// active vendor — model name alone is ambiguous (Cursor/Copilot
        /// can wrap Claude or GPT). Defaults to None on legacy clients
        /// that haven't been re-detected since this field was added.
        #[serde(default)]
        tool: Option<String>,
    },
    /// Queued human→browser input events (from PollBrowserInput). Cleared on read.
    BrowserInput {
        events: Vec<BrowserInputEvent>,
    },
}
