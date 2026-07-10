//! WebSocket streaming server — 60fps bidirectional AI channel.
//!
//! Each daemon session starts a localhost-only WebSocket server on a dynamic port.
//! Clients (Claude Code, VS Code webview, WASM demo) connect and receive:
//!   - A `hello` message with full viewport state on connect
//!   - `viewport_diff` messages pushed at 60fps (only dirty rows)
//!   - `viewport_full` resync if the client falls behind
//!
//! Clients can send draw commands, terminal input, and resize requests on the
//! same connection. All commands are forwarded to the event loop via an mpsc channel.
//!
//! No shared mutable state — SessionState stays exclusively owned by the event loop.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Read a `public.file-url` from NSPasteboard (Finder Cmd+C). Returns the
/// POSIX path on hit, None otherwise.
///
/// Earlier impl spawned `osascript` — that worked for plain Finder copies
/// but had two killer bugs: (a) ~1.3s latency on image-bearing clipboards
/// because AppleScript decodes every image rep to negotiate type, blowing
/// past the webview RPC timeout and breaking screenshot paste; and (b)
/// `the clipboard as «class furl»` silently coerces plain-text clipboards
/// into a fake `/text` path. Native NSPasteboard.readObjects is microsecond
/// fast and only matches real file URL entries.
#[cfg(target_os = "macos")]
pub(crate) fn read_clipboard_file_url() -> Option<String> {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSString, NSURL};

    // stringForType("public.file-url") returns the URL only when a real file
    // URL pasteboard entry exists — never coerced from text — and matches in
    // microseconds without scanning image reps. Then NSURL parses the
    // percent-encoded string into a POSIX path so Unicode/space-laden
    // filenames (e.g. Hebrew or "My File.pdf") survive.
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        let type_name = NSString::from_str("public.file-url");
        let url_str = pb.stringForType(&type_name)?;
        let url = NSURL::URLWithString(&url_str)?;
        let path = url.path()?;
        Some(path.to_string())
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn read_clipboard_file_url() -> Option<String> { None }

/// Write provided PNG bytes to the per-session paste dir and return the
/// path. Same naming + prune logic as `save_clipboard_image_to_temp`,
/// just sourced from explicit bytes rather than the local clipboard.
/// Used for remote-tab paste: webview ships the bytes, daemon writes the
/// file on its filesystem (which is the remote box when via tunnel).
pub(crate) fn save_image_bytes_to_temp(png_bytes: &[u8]) -> Option<String> {
    let dir = paste_dir_for_session();
    std::fs::create_dir_all(&dir).ok()?;
    let n = next_paste_index(&dir);
    let path = dir.join(format!("{n}.png"));
    std::fs::write(&path, png_bytes).ok()?;
    prune_old_paste_files(&dir);

    // Also mirror the bytes into the clipboard staging file so the
    // xclip/wl-paste shim (~/.immorterm/bin/{xclip,wl-paste,xsel}) can
    // serve them when Claude Code asks the OS clipboard for an image.
    // This is what gives Cmd+V on a remote tab the `[Image #N]`
    // semantics — without it, Claude on a headless container has no
    // clipboard at all. Override path with $IMMORTERM_CLIPBOARD_FILE
    // (the shim reads the same env var).
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .ok();
    let staging = std::env::var("IMMORTERM_CLIPBOARD_FILE")
        .map(std::path::PathBuf::from)
        .ok()
        .or_else(|| home.as_ref().map(|h| h.join(".immorterm").join("clipboard").join("current.png")));
    if let Some(staging) = staging {
        if let Some(parent) = staging.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&staging, png_bytes);
    }

    Some(path.to_string_lossy().into_owned())
}

/// Write an attachment/share item into `~/.immorterm/pending-share/<window_id>/
/// <id>.json` on THIS host, so the host's UserPromptSubmit hook drains it on
/// the next prompt. `window_id` must be empty-guarded (an empty id would
/// collapse the path and let every terminal read it). The item's `id` field
/// names the file; a fallback id is minted if missing.
pub(crate) fn write_share_item_to_queue(
    window_id: &str,
    item: &serde_json::Value,
) -> bool {
    // window_id is caller-controlled (webview → WS, possibly across the SSH
    // tunnel). It names a path segment, so reject anything that could escape
    // the pending-share base. Real ids are `{pid}-{8hex}` → strict allowlist.
    if window_id.is_empty()
        || !window_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        warn!("[share] refused write: invalid window_id");
        return false;
    }
    let home = match std::env::var("HOME") {
        Ok(h) => std::path::PathBuf::from(h),
        Err(_) => return false,
    };
    let dir = home.join(".immorterm").join("pending-share").join(window_id);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let id = item
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && !s.contains('/') && !s.contains(".."))
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("att-{}", std::process::id()));
    let path = dir.join(format!("{id}.json"));
    match serde_json::to_vec(item) {
        Ok(bytes) => std::fs::write(&path, bytes).is_ok(),
        Err(_) => false,
    }
}

/// Write the clipboard image to a per-session paste dir and return its path.
/// Backs Cmd+Option+V — Claude Code reads the file lazily so the image
/// doesn't count against the many-image dimension limit unless Claude opens
/// it.
///
/// Files live at `~/.immorterm/paste/<window_id>/<n>.png` (where `<n>` is
/// 1-based, per-session counter — first paste of a session is always
/// `1.png`, mirroring Claude's per-session `[Image #1]` numbering). When
/// the env var is unset (CLI / non-VS-Code users), falls back to a
/// `default` subdir so the structure stays uniform.
///
/// Best-effort prunes the same dir on each call (files >7 days). macOS
/// will eventually clean `~/.immorterm/paste/` itself only at our
/// initiative, so the in-process sweep is what bounds usage.
pub(crate) fn save_clipboard_image_to_temp() -> Option<String> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let img = clipboard.get_image().ok()?;
    let dir = paste_dir_for_session();
    std::fs::create_dir_all(&dir).ok()?;
    let n = next_paste_index(&dir);
    let path = dir.join(format!("{n}.png"));
    let buf = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(
        img.width as u32,
        img.height as u32,
        img.bytes.into_owned(),
    )?;
    buf.save(&path).ok()?;
    prune_old_paste_files(&dir);
    Some(path.to_string_lossy().into_owned())
}

/// `~/.immorterm/paste/<window_id>/` — per-session paste dir. Falls back to
/// a `default` namespace if the daemon was launched without IMMORTERM_WINDOW_ID
/// (CLI users, smoke tests).
fn paste_dir_for_session() -> std::path::PathBuf {
    let id = std::env::var("IMMORTERM_WINDOW_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".immorterm/paste").join(id)
}

/// Pick the next free `<n>.png` index in the dir. Walks once and takes
/// max(existing) + 1 — the gap-tolerant variant of "count + 1" so a manual
/// rm of file #2 won't make a future paste collide with an existing #N.
fn next_paste_index(dir: &std::path::Path) -> u64 {
    let mut max_n: u64 = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name_s = name.to_string_lossy();
            if let Some(stem) = name_s.strip_suffix(".png")
                && let Ok(n) = stem.parse::<u64>()
                && n > max_n {
                    max_n = n;
                }
        }
    }
    max_n + 1
}

/// Delete `<n>.png` files in the per-session dir older than 7 days.
/// Opportunistic — errors swallowed so paste never fails.
fn prune_old_paste_files(dir: &std::path::Path) {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(7 * 24 * 60 * 60));
    let Some(cutoff) = cutoff else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if !name_s.ends_with(".png") { continue; }
        if let Ok(meta) = entry.metadata()
            && let Ok(modified) = meta.modified()
            && modified < cutoff {
                let _ = std::fs::remove_file(entry.path());
            }
    }
}

// ─── Server → Client messages ────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum WsServerMsg {
    /// Sent on connection — full state snapshot.
    #[serde(rename = "hello")]
    Hello {
        session: String,
        cols: usize,
        rows: usize,
        title: String,
        project: String,
        theme: String,
        lines: Vec<String>,
        cursor: CursorState,
        scrollback_len: usize,
        ai_primitives: Vec<serde_json::Value>,
        capabilities: Vec<String>,
    },
    /// 60fps incremental update — only changed rows.
    #[serde(rename = "viewport_diff")]
    ViewportDiff {
        seq: u64,
        dirty_rows: Vec<ViewportRow>,
        cursor: CursorState,
        scrollback_len: usize,
        ai_layer_changed: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        ai_primitives: Option<Vec<serde_json::Value>>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        ai_events: Vec<serde_json::Value>,
    },
    /// Full viewport resync after client lag.
    #[serde(rename = "viewport_full")]
    ViewportFull {
        seq: u64,
        lines: Vec<String>,
        cursor: CursorState,
        scrollback_len: usize,
        ai_primitives: Vec<serde_json::Value>,
    },
    /// Response to a draw command.
    #[serde(rename = "draw_result")]
    DrawResult {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        id: u32,
        #[serde(rename = "primitive_type")]
        ptype: String,
    },
    /// Error response.
    #[serde(rename = "error")]
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        message: String,
    },
    /// Terminal resized.
    #[serde(rename = "resize")]
    Resize { cols: usize, rows: usize },
    /// Pong response.
    #[serde(rename = "pong")]
    Pong,
    /// Full team state snapshot (from subscribe_team or on change).
    #[serde(rename = "team_state")]
    TeamState {
        team_name: String,
        /// JSON-serialized immorterm_core::TeamState
        state_json: String,
    },
    /// Incremental team update event.
    #[serde(rename = "team_update")]
    TeamUpdate {
        team_name: String,
        event: TeamEvent,
    },
    /// Channel server registered acknowledgement.
    #[serde(rename = "channel_registered")]
    ChannelRegistered {
        immorterm_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    /// Channel message from a paired session.
    #[serde(rename = "channel_message")]
    ChannelMsg {
        from_immorterm_id: String,
        from_name: String,
        message: String,
    },
    /// Interactive session pairing established.
    #[serde(rename = "session_paired")]
    SessionPaired {
        partner_id: String,
        partner_name: String,
    },
    /// Interactive session pairing ended.
    #[serde(rename = "session_unpaired")]
    SessionUnpaired,
    /// Full terminal snapshot for GPU clients (binary frame subscribers).
    #[serde(rename = "snapshot")]
    Snapshot {
        snapshot_json: String,
        session: String,
        theme: String,
        project: String,
        cols: usize,
        rows: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        claude: Option<serde_json::Value>,
        /// Total scrollback rows available on the daemon (for on-demand fetch).
        /// The snapshot itself may contain 0 rows (viewport-only mode).
        #[serde(default, skip_serializing_if = "is_zero")]
        scrollback_total: usize,
    },
    /// Initial state for control subscribers.
    #[serde(rename = "control_hello")]
    ControlHello {
        session_name: String,
        window_id: String,
        display_name: String,
        claude: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_user_prompt: Option<String>,
    },
    /// Control event pushed to subscribers.
    #[serde(rename = "control_event")]
    ControlEvent {
        event: String,
        claude: Option<serde_json::Value>,
    },
    /// On-demand scrollback rows (response to ScrollRequest).
    #[serde(rename = "scrollback_rows")]
    ScrollbackRows {
        offset: usize,
        rows_json: String,
        total: usize,
    },
    /// Reply to clipboard_check_image. `file_url` is set when the clipboard
    /// holds a Finder file copy (any file type — PDF, image, doc); the
    /// webview should prefer typing this path over the empty-bracketed-paste
    /// flow (otherwise Claude Code would treat the file's preview thumbnail
    /// as a standalone image). `has_image` reflects raw image bytes (e.g.
    /// screenshots) and is meaningful only when `file_url` is None.
    #[serde(rename = "clipboard_image_presence")]
    ClipboardImagePresence {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        has_image: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_url: Option<String>,
    },
    /// Reply to clipboard_save_image — daemon wrote the clipboard image to a
    /// temp file and returns the path. Used by the Cmd+Option+V "paste as
    /// path" flow that avoids inlining the image into the conversation
    /// (so the many-image dimension limit doesn't trigger for big images).
    #[serde(rename = "clipboard_image_saved")]
    ClipboardImageSaved {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// Reply to write_share_item — daemon wrote the attachment/share JSON into
    /// THIS host's `~/.immorterm/pending-share/<window_id>/` queue. For a
    /// remote-bound tab the daemon runs on the remote host, so this is how a
    /// local attach reaches the remote session's UserPromptSubmit hook.
    #[serde(rename = "share_item_written")]
    ShareItemWritten {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        ok: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct CursorState {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ViewportRow {
    pub idx: usize,
    pub text: String,
}

/// Team state change events for incremental updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamEvent {
    /// A new member joined the team.
    MemberJoined { name: String },
    /// A member left or was shut down.
    MemberLeft { name: String },
    /// A task changed status or owner.
    TaskChanged { task_id: String, status: String, owner: Option<String> },
    /// A new message was received in an inbox.
    MessageReceived { from: String, to: String, summary: String },
    /// The team config was updated (full resync recommended).
    ConfigChanged,
    /// The team lifecycle changed (e.g., Active → Done).
    LifecycleChanged {
        old: immorterm_core::team::TeamLifecycle,
        new: immorterm_core::team::TeamLifecycle,
    },
}

// ─── Client → Server messages ────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum WsClientMsg {
    /// UTF-8 text → PTY (\n = Enter).
    #[serde(rename = "input")]
    Input {
        data: String,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Base64 raw bytes → PTY (escape sequences).
    #[serde(rename = "input_raw")]
    InputRaw {
        data: String,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Human input on the self-driven browser panel → queued for the MCP
    /// screencast pump to dispatch to the live browser (page CSS px). `kind`
    /// is click/key/scroll; the payload fields are flattened alongside it.
    #[serde(rename = "browser_input")]
    BrowserInput {
        kind: String,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        dy: Option<f64>,
    },
    /// Panel pause/continue toggle → queued as a Control browser-input event.
    #[serde(rename = "browser_control")]
    BrowserControl {
        action: String,
    },
    /// Ask the daemon whether the system clipboard currently holds image
    /// bytes. The webview's Async Clipboard API only exposes `image/png`,
    /// not JPEG/TIFF, so it falls back to this RPC for non-PNG images.
    #[serde(rename = "clipboard_check_image")]
    ClipboardCheckImage {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Persist the clipboard image to a temp PNG and return its path.
    /// Backs Cmd+Option+V — the path-style paste that avoids inlining the
    /// image into the conversation (Claude Code reads it lazily via Read).
    #[serde(rename = "clipboard_save_image")]
    ClipboardSaveImage {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Persist image bytes sent from the webview as a temp PNG and return
    /// its path. Used when the daemon's machine has no clipboard access
    /// (e.g. headless remote container) but the webview can read the
    /// user's local clipboard — the bytes cross the SSH tunnel and land
    /// on the daemon's filesystem at `~/.immorterm/paste/<window>/<n>.png`.
    /// Backs Cmd+V image + Cmd+Option+V on remote-bound tabs.
    #[serde(rename = "clipboard_save_image_bytes")]
    ClipboardSaveImageBytes {
        #[serde(default)]
        request_id: Option<String>,
        /// PNG bytes encoded as standard base64. Webview produces this
        /// from `navigator.clipboard.read()` → `blob.arrayBuffer()` →
        /// btoa over the byte array.
        png_base64: String,
    },
    /// Write an attachment/share item into THIS host's per-terminal
    /// `~/.immorterm/pending-share/<window_id>/<id>.json` queue, so the host's
    /// UserPromptSubmit hook injects it on the next prompt. Mirrors
    /// clipboard_save_image_bytes: on a remote-bound tab the daemon runs on the
    /// remote host, so a local file-drop / session-share lands in the REMOTE
    /// queue where the remote session's hook can consume it.
    #[serde(rename = "write_share_item")]
    WriteShareItem {
        #[serde(default)]
        request_id: Option<String>,
        /// Target session's window id (== its IMMORTERM_ID on this host).
        window_id: String,
        /// Full share payload: { id, kind, file_path?, rel_path?,
        /// source_immorterm_id?, source_name?, task_id?, ... }.
        item: serde_json::Value,
    },
    /// Draw a filled rectangle.
    #[serde(rename = "draw_rect")]
    DrawRect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: [f32; 4],
        #[serde(default)]
        border_color: Option<[f32; 4]>,
        #[serde(default)]
        border_width: Option<f32>,
        #[serde(default)]
        anchor: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Draw text.
    #[serde(rename = "draw_text")]
    DrawText {
        text: String,
        x: f32,
        y: f32,
        color: [f32; 4],
        #[serde(default)]
        font_size_scale: Option<f32>,
        #[serde(default)]
        anchor: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Draw a clickable button.
    #[serde(rename = "draw_button")]
    DrawButton {
        text: String,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        bg_color: [f32; 4],
        text_color: [f32; 4],
        #[serde(default)]
        anchor: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Draw a line.
    #[serde(rename = "draw_line")]
    DrawLine {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color: [f32; 4],
        #[serde(default)]
        thickness: Option<f32>,
        #[serde(default)]
        anchor: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Remove a primitive by ID.
    #[serde(rename = "remove_primitive")]
    RemovePrimitive {
        id: u32,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Clear all AI canvas content.
    #[serde(rename = "clear_ai_layer")]
    ClearAiLayer {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Animate a primitive property.
    #[serde(rename = "animate")]
    Animate {
        primitive_id: u32,
        property: String,
        from: f32,
        to: f32,
        duration_ms: u32,
        #[serde(default)]
        easing: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Resize terminal.
    #[serde(rename = "resize")]
    Resize {
        cols: u16,
        rows: u16,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Manually rebuild grid+scrollback from the PTY byte history at the
    /// current column width. User-triggered (Cmd+Shift+R) escape hatch for
    /// the case where auto-replay-on-resize didn't fire (panel was hidden
    /// while output streamed, no resize event) or didn't fully recover.
    #[serde(rename = "rerender_backlog")]
    RerenderBacklog {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Right-click "Reconnect to AI". Daemon spams Ctrl+C into the PTY to
    /// exit the running TUI AI (never `/exit\r` — a queued draft would get
    /// submitted by the `\r`), polls for the AI child to die (max 3 s,
    /// one retry burst), then stuffs `immorterm-ai recall\r` so the
    /// existing 4-tier cascade (resolve UUID → `claude --resume <uuid>` →
    /// /immorterm:recall skill → plain claude) brings the AI back with
    /// restored context. Daemon-side so it works for AI-tab sessions
    /// that aren't wrapped in `screen`.
    #[serde(rename = "reconnect_ai")]
    ReconnectAi {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Frontend's visual bell auto-clear (active session received PTY data)
    /// mirroring itself to the registry, so the dismissal survives reload.
    /// Daemon-side has no auto-clear anymore; this is the only way the registry
    /// flag gets reset back to false outside of `notify working`.
    #[serde(rename = "dismiss_attention")]
    DismissAttention {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Subscribe to raw PTY bytes (for GPU/WASM clients).
    #[serde(rename = "subscribe_raw")]
    SubscribeRaw {
        #[serde(default)]
        request_id: Option<String>,
        /// When true, request a full snapshot (with scrollback) instead of viewport-only.
        #[serde(default)]
        full_snapshot: bool,
    },
    /// Subscribe to lightweight control events.
    #[serde(rename = "subscribe_control")]
    SubscribeControl {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Subscribe to team state updates for a specific team.
    #[serde(rename = "subscribe_team")]
    SubscribeTeam {
        team_name: String,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Send a message to a team member.
    #[serde(rename = "send_team_message")]
    SendTeamMessage {
        team_name: String,
        recipient: String,
        content: String,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Request a range of scrollback rows (on-demand, for scroll-up).
    #[serde(rename = "scroll_request")]
    ScrollRequest {
        offset: usize,
        count: usize,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Register as a channel server for this session.
    #[serde(rename = "register_channel")]
    RegisterChannel {
        immorterm_id: String,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Send a message to a paired session via channel.
    #[serde(rename = "channel_message")]
    ChannelMessage {
        to_immorterm_id: String,
        message: String,
        #[serde(default)]
        from_name: Option<String>,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Request to pair two sessions for interactive sharing.
    #[serde(rename = "pair_sessions")]
    PairSessions {
        source_id: String,
        target_id: String,
        source_name: String,
        target_name: String,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Unpair the current interactive session.
    #[serde(rename = "unpair_sessions")]
    UnpairSessions {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Close a workshop initiated from the webview (e.g. user clicked the
    /// per-tab X button). Mirrors the MCP `close_workshop` IPC: removes from
    /// session state, deletes the persisted HTML file, broadcasts a close
    /// envelope to all WS subscribers so every webview drops the DOM.
    /// Idempotent: closing a non-existent workshop is Ok.
    #[serde(rename = "close_workshop")]
    CloseWorkshop {
        name: String,
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Keep-alive ping.
    #[serde(rename = "ping")]
    Ping,
}

// ─── Commands sent from WS handlers to the event loop ────────────────

/// Routed to the event loop via mpsc — never touches SessionState directly.
pub enum WsCommand {
    /// Close a workshop by name (initiated from the webview — e.g. user clicked
    /// the per-tab X button or chose Close from the sidebar context menu).
    /// Handled inline in the session event loop because it needs both
    /// `&mut state.workshops` and `workshop_tx` for the broadcast.
    CloseWorkshop {
        name: String,
    },
    /// Write UTF-8 text to PTY.
    Input(Vec<u8>),
    /// Queue a human browser-panel input event for the MCP screencast pump
    /// (`PollBrowserInput`). Pushed onto `state.browser_input_queue`.
    BrowserInput(crate::ipc::BrowserInputEvent),
    /// Draw a rect and reply with the primitive ID.
    DrawRect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: [f32; 4],
        border_color: Option<[f32; 4]>,
        border_width: Option<f32>,
        anchor: Option<String>,
        reply: oneshot::Sender<WsDrawReply>,
    },
    DrawText {
        text: String,
        x: f32,
        y: f32,
        color: [f32; 4],
        font_size_scale: Option<f32>,
        anchor: Option<String>,
        reply: oneshot::Sender<WsDrawReply>,
    },
    DrawButton {
        text: String,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        bg_color: [f32; 4],
        text_color: [f32; 4],
        anchor: Option<String>,
        reply: oneshot::Sender<WsDrawReply>,
    },
    DrawLine {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color: [f32; 4],
        thickness: Option<f32>,
        anchor: Option<String>,
        reply: oneshot::Sender<WsDrawReply>,
    },
    RemovePrimitive {
        id: u32,
        reply: oneshot::Sender<Result<(), String>>,
    },
    ClearAiLayer,
    Animate {
        primitive_id: u32,
        property: String,
        from: f32,
        to: f32,
        duration_ms: u32,
        easing: Option<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Rebuild grid+scrollback from PTY byte history at current cols.
    RerenderBacklog,
    /// Force-quit the running AI process and re-invoke `immorterm-ai recall`.
    ReconnectAi,
    /// Frontend dismissed the bell visually — clear `needs_attention` in registry
    /// so a subsequent VS Code reload doesn't resurrect it.
    DismissAttention,
    /// Request full viewport state (for hello message or lag recovery).
    GetInitialState(oneshot::Sender<InitialState>),
    /// Request terminal snapshot + raw PTY byte stream (for GPU clients).
    SubscribeRaw {
        reply: oneshot::Sender<SubscribeRawReply>,
        /// When true, force a full snapshot (with scrollback).
        full_snapshot: bool,
    },
    /// Request current control state (for control_hello message).
    GetControlState(oneshot::Sender<ControlStateReply>),
    /// Request a range of scrollback rows for on-demand scroll.
    ScrollRequest {
        offset: usize,
        count: usize,
        reply: oneshot::Sender<ScrollbackReply>,
    },
    /// Register a channel server for cross-session messaging.
    RegisterChannel {
        immorterm_id: String,
        /// Sender to push messages (JSON strings) to this WS client.
        channel_tx: mpsc::Sender<String>,
    },
    /// Pair two sessions for interactive sharing.
    PairSessions {
        source_id: String,
        target_id: String,
        source_name: String,
        target_name: String,
    },
    /// Unpair interactive sessions.
    UnpairSessions,
}

/// Reply for SubscribeRaw — snapshot JSON + broadcast receiver of raw PTY bytes.
pub struct SubscribeRawReply {
    pub snapshot_json: String,
    pub session: String,
    pub theme: String,
    pub project: String,
    pub cols: usize,
    pub rows: usize,
    pub claude: Option<serde_json::Value>,
    pub pty_rx: broadcast::Receiver<Vec<u8>>,
    /// Total scrollback rows available on the daemon (viewport-only snapshot has 0 in JSON).
    pub scrollback_total: usize,
}

/// Reply for ScrollRequest — serialized rows + total scrollback length.
pub struct ScrollbackReply {
    pub rows_json: String,
    pub offset: usize,
    pub total: usize,
}

/// Reply from event loop after a draw command.
pub struct WsDrawReply {
    pub id: u32,
    pub ptype: String,
}

/// Reply with full control state for hello message.
pub struct ControlStateReply {
    pub session_name: String,
    pub window_id: String,
    pub display_name: String,
    pub claude: Option<serde_json::Value>,
    pub last_user_prompt: Option<String>,
}

/// Full viewport snapshot for hello/resync messages.
pub struct InitialState {
    pub session: String,
    pub cols: usize,
    pub rows: usize,
    pub title: String,
    pub project: String,
    pub theme: String,
    pub lines: Vec<String>,
    pub cursor: CursorState,
    pub scrollback_len: usize,
    pub ai_primitives: Vec<serde_json::Value>,
}

// ─── Control event helpers ───────────────────────────────────────────

/// Build Claude state JSON from tracker.
pub fn build_control_state(claude: &crate::claude::ClaudeTracker) -> Option<serde_json::Value> {
    if claude.claude_pid.is_none() && claude.api_stats.model.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "active": claude.claude_pid.is_some(),
        "pid": claude.claude_pid,
        "session_id": claude.session_id,
        "rss_kb": claude.rss_kb,
        "cpu_percent": claude.cpu_percent,
        "runtime_secs": claude.runtime_secs(),
        "model": if claude.api_stats.model.is_empty() { None } else { Some(&claude.api_stats.model) },
        "cost_usd": if claude.api_stats.cost_usd > 0.0 { Some(claude.api_stats.cost_usd) } else { None },
        "context_pct": if claude.api_stats.context_pct > 0.0 { Some(claude.api_stats.context_pct) } else { None },
        "transcript_path": if claude.api_stats.transcript_path.is_empty() { None } else { Some(&claude.api_stats.transcript_path) },
        "permission_mode": claude.permission_mode,
        // Active vendor identifier — lowercase ("claude", "codex", ...) so
        // the gpu-terminal.html status bar can prefix the AI stats line.
        // None on legacy entries that haven't been re-detected since the
        // multi-vendor classify_ai_process landed in #28.
        "tool": claude.detected_tool.map(|t| t.name()),
    }))
}

/// Build a JSON control event string from current Claude state.
pub fn build_control_event(event_type: &str, claude: &crate::claude::ClaudeTracker) -> String {
    serde_json::json!({
        "type": "control_event",
        "event": event_type,
        "claude": build_control_state(claude),
    }).to_string()
}

/// Build a control event with an optional `branch` field, used to push
/// the session's current git branch (from `state.terminal.cwd`'s `.git/HEAD`)
/// alongside Claude state. The webview tracks `branch` per session and
/// renders it in the status-bar projectName label for the active tab.
///
/// Always include the field even when `branch` is None — the webview
/// dedupes on no-op transitions itself. Top-level (not nested in `claude`)
/// because branch is a property of the session's cwd, not of Claude.
pub fn build_control_event_with_branch(
    event_type: &str,
    claude: &crate::claude::ClaudeTracker,
    branch: Option<&str>,
) -> String {
    serde_json::json!({
        "type": "control_event",
        "event": event_type,
        "claude": build_control_state(claude),
        "branch": branch,
    }).to_string()
}

/// Build a JSON control event with an optional last_user_prompt field.
pub fn build_control_event_with_prompt(
    event_type: &str,
    claude: &crate::claude::ClaudeTracker,
    last_user_prompt: Option<&str>,
) -> String {
    serde_json::json!({
        "type": "control_event",
        "event": event_type,
        "claude": build_control_state(claude),
        "last_user_prompt": last_user_prompt,
    }).to_string()
}

/// Bundled channels for a WebSocket connection (avoids clippy::too_many_arguments).
struct WsChannels {
    viewport_rx: broadcast::Receiver<Arc<String>>,
    cmd_tx: mpsc::Sender<WsCommand>,
    control_rx: broadcast::Receiver<Arc<String>>,
    ai_layer_rx: broadcast::Receiver<Arc<Vec<u8>>>,
    ai_event_tx: broadcast::Sender<immorterm_core::ai_layer::AiEvent>,
    /// Targeted JS-eval messages bound for a specific primitive's Shadow DOM.
    /// Carries pre-serialized JSON `{"primitive_id":N,"js":"..."}` (envelope
    /// added in the WS forward step).
    ai_eval_rx: broadcast::Receiver<Arc<String>>,
    /// Workshop lifecycle events (open/update/eval/close). Carries
    /// pre-serialized JSON `{"event":"open"|...,"name":"...",...}`. Wrapped
    /// as `{"type":"workshop_event","data":<this>}` on the wire.
    workshop_rx: broadcast::Receiver<Arc<String>>,
    /// Live count of clients currently in raw_mode for this session. Owned
    /// by `SessionState`; the WS loop increments on subscribe_raw and
    /// decrements on subscribe_control / disconnect.
    raw_subscriber_count: Arc<std::sync::atomic::AtomicUsize>,
}

// ─── Server lifecycle ────────────────────────────────────────────────

/// Start the WebSocket server. Returns the assigned port, or 0 on failure.
///
/// **Bind host** is read from `IMMORTERM_WS_LISTEN_HOST` (default `127.0.0.1`).
/// Set to `0.0.0.0` for containerized / remote deployments where clients
/// need to reach the WS through Docker port mapping or a TLS reverse proxy.
///
/// **Port selection**:
/// - If `IMMORTERM_WS_PORT_BASE` is set (e.g. `9000`), iterate upward from
///   there until a port binds — gives the host a predictable range that
///   Docker can `-p 9000-9050:9000-9050` map cleanly.
/// - Otherwise bind port `0` (ephemeral) — the historical localhost-only
///   behaviour.
#[allow(clippy::too_many_arguments)]
pub async fn start_websocket_server(
    session_name: String,
    viewport_tx: broadcast::Sender<Arc<String>>,
    cmd_tx: mpsc::Sender<WsCommand>,
    control_tx: broadcast::Sender<Arc<String>>,
    ai_layer_tx: broadcast::Sender<Arc<Vec<u8>>>,
    ai_event_tx: broadcast::Sender<immorterm_core::ai_layer::AiEvent>,
    ai_eval_tx: broadcast::Sender<Arc<String>>,
    workshop_tx: broadcast::Sender<Arc<String>>,
    raw_subscriber_count: Arc<std::sync::atomic::AtomicUsize>,
) -> Result<u16, std::io::Error> {
    let host = std::env::var("IMMORTERM_WS_LISTEN_HOST")
        .unwrap_or_else(|_| "127.0.0.1".to_string());

    let listener = if let Ok(base_str) = std::env::var("IMMORTERM_WS_PORT_BASE") {
        let base: u16 = base_str.parse().unwrap_or(0);
        let span: u16 = std::env::var("IMMORTERM_WS_PORT_SPAN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(50);
        let mut bound = None;
        for offset in 0..span {
            let port = base.saturating_add(offset);
            match TcpListener::bind(format!("{host}:{port}")).await {
                Ok(l) => { bound = Some(l); break; }
                Err(_) => continue,
            }
        }
        bound.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("no free port in range {base}..{}", base.saturating_add(span)),
            )
        })?
    } else {
        TcpListener::bind(format!("{host}:0")).await?
    };
    let port = listener.local_addr()?.port();

    tokio::spawn(accept_loop(listener, session_name, viewport_tx, cmd_tx, control_tx, ai_layer_tx, ai_event_tx, ai_eval_tx, workshop_tx, raw_subscriber_count));

    Ok(port)
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: TcpListener,
    session_name: String,
    viewport_tx: broadcast::Sender<Arc<String>>,
    cmd_tx: mpsc::Sender<WsCommand>,
    control_tx: broadcast::Sender<Arc<String>>,
    ai_layer_tx: broadcast::Sender<Arc<Vec<u8>>>,
    ai_event_tx: broadcast::Sender<immorterm_core::ai_layer::AiEvent>,
    ai_eval_tx: broadcast::Sender<Arc<String>>,
    workshop_tx: broadcast::Sender<Arc<String>>,
    raw_subscriber_count: Arc<std::sync::atomic::AtomicUsize>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("WebSocket client connected from {}", addr);
                let channels = WsChannels {
                    viewport_rx: viewport_tx.subscribe(),
                    cmd_tx: cmd_tx.clone(),
                    control_rx: control_tx.subscribe(),
                    ai_layer_rx: ai_layer_tx.subscribe(),
                    ai_event_tx: ai_event_tx.clone(),
                    ai_eval_rx: ai_eval_tx.subscribe(),
                    workshop_rx: workshop_tx.subscribe(),
                    raw_subscriber_count: raw_subscriber_count.clone(),
                };
                let name = session_name.clone();
                tokio::spawn(handle_ws_connection(stream, name, channels, false));
            }
            Err(e) => {
                error!("WebSocket accept error: {}", e);
            }
        }
    }
}

async fn handle_ws_connection(
    stream: tokio::net::TcpStream,
    _session_name: String,
    channels: WsChannels,
    mut raw_mode: bool,
) {
    let WsChannels {
        mut viewport_rx,
        cmd_tx,
        mut control_rx,
        mut ai_layer_rx,
        ai_event_tx,
        mut ai_eval_rx,
        mut workshop_rx,
        raw_subscriber_count,
    } = channels;
    // Upgrade TCP → WebSocket
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            error!("WebSocket handshake failed: {}", e);
            return;
        }
    };

    let (mut ws_sink, ws_source) = ws_stream.split();

    // Spawn a dedicated reader task that intercepts click events from the WebSocket
    // stream BEFORE they enter the main select loop. This prevents a deadlock where
    // viewport resync (cmd_tx → main loop) blocks the select loop while WaitForAiEvent
    // is blocking the main loop waiting for exactly these click events.
    let (client_msg_tx, mut client_msg_rx) = mpsc::channel::<Message>(64);
    let reader_ai_tx = ai_event_tx.clone();
    tokio::spawn(async move {
        let mut ws_source = ws_source;
        while let Some(result) = ws_source.next().await {
            match result {
                Ok(msg) => {
                    // Intercept click protocol messages and broadcast immediately,
                    // independent of the main select loop's state.
                    if let Message::Text(text) = &msg
                        && let Ok(client_msg) = serde_json::from_str::<WsClientMsg>(text)
                        && let WsClientMsg::DrawButton { text, .. } = &client_msg
                        && let Some(rest) = text.strip_prefix("__click__:")
                    {
                        // Three accepted forms:
                        //   __click__:<id>                      -> ButtonClicked, no label
                        //   __click__:<id>:<data-click>         -> ButtonClicked, labeled
                        //   __click__:workshop:<name>:<label>   -> WorkshopClicked
                        if let Some(after) = rest.strip_prefix("workshop:") {
                            // Workshop click: parse "name:label" (label may contain ':' too;
                            // split only on the FIRST ':' so labels like "step:1" survive).
                            let (name, label) = match after.split_once(':') {
                                Some((n, l)) => (n.to_string(), Some(l.to_string())),
                                None => (after.to_string(), None),
                            };
                            let _ = reader_ai_tx.send(
                                immorterm_core::ai_layer::AiEvent::WorkshopClicked {
                                    name,
                                    data_click: label,
                                },
                            );
                        } else {
                            // Primitive click (legacy form).
                            let (id_part, data_click) = match rest.split_once(':') {
                                Some((id_p, dc)) => (id_p, Some(dc.to_string())),
                                None => (rest, None),
                            };
                            if let Ok(btn_id) = id_part.parse::<u32>() {
                                let _ = reader_ai_tx.send(
                                    immorterm_core::ai_layer::AiEvent::ButtonClicked {
                                        id: btn_id,
                                        data_click,
                                    },
                                );
                            }
                        }
                    }
                    if client_msg_tx.send(msg).await.is_err() {
                        break; // Main handler dropped — exit
                    }
                }
                Err(e) => {
                    warn!("WebSocket read error in reader task: {}", e);
                    break;
                }
            }
        }
    });

    // Request initial state from event loop
    let (state_tx, state_rx) = oneshot::channel();
    if cmd_tx.send(WsCommand::GetInitialState(state_tx)).await.is_err() {
        return;
    }
    let state = match state_rx.await {
        Ok(s) => s,
        Err(_) => return,
    };

    // Send hello message
    let hello = WsServerMsg::Hello {
        session: state.session,
        cols: state.cols,
        rows: state.rows,
        title: state.title,
        project: state.project,
        theme: state.theme,
        lines: state.lines,
        cursor: state.cursor,
        scrollback_len: state.scrollback_len,
        ai_primitives: state.ai_primitives,
        capabilities: vec![
            "viewport_stream".into(),
            "ai_canvas".into(),
            "input".into(),
            "images".into(),
            "annotations".into(),
            "charts".into(),
            "raw_pty".into(),
        ],
    };
    if let Ok(json) = serde_json::to_string(&hello)
        && ws_sink.send(Message::Text(json)).await.is_err() {
            return;
        }

    // Optional: raw PTY byte receiver (activated by subscribe_raw message)
    let mut pty_rx: Option<broadcast::Receiver<Vec<u8>>> = None;
    let mut control_mode = false;
    // Channel mode: receives messages to forward to channel server WS client
    let mut channel_rx: Option<mpsc::Receiver<String>> = None;

    // Bidirectional select loop
    loop {
        tokio::select! {
            // Viewport diffs from the 60fps frame timer (text-mode clients)
            diff = viewport_rx.recv(), if !raw_mode && !control_mode => {
                match diff {
                    Ok(json_arc) => {
                        if ws_sink.send(Message::Text((*json_arc).clone())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WebSocket client lagged by {} frames, sending full resync", n);
                        let (tx, rx) = oneshot::channel();
                        if cmd_tx.send(WsCommand::GetInitialState(tx)).await.is_err() {
                            break;
                        }
                        if let Ok(state) = rx.await {
                            let full = WsServerMsg::ViewportFull {
                                seq: 0,
                                lines: state.lines,
                                cursor: state.cursor,
                                scrollback_len: state.scrollback_len,
                                ai_primitives: state.ai_primitives,
                            };
                            if let Ok(json) = serde_json::to_string(&full)
                                && ws_sink.send(Message::Text(json)).await.is_err() {
                                    break;
                                }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Raw PTY bytes → binary WebSocket frames (GPU clients)
            data = async {
                match pty_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            }, if raw_mode => {
                match data {
                    Ok(bytes) => {
                        if ws_sink.send(Message::Binary(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Raw PTY subscriber lagged by {} frames", n);
                        // For raw mode, we can't easily resync — the WASM terminal
                        // processes bytes sequentially. Request a fresh snapshot.
                        let (tx, rx) = oneshot::channel();
                        if cmd_tx.send(WsCommand::SubscribeRaw { reply: tx, full_snapshot: false }).await.is_err() {
                            break;
                        }
                        if let Ok(reply) = rx.await {
                            pty_rx = Some(reply.pty_rx);
                            let snap = WsServerMsg::Snapshot {
                                snapshot_json: reply.snapshot_json,
                                session: reply.session,
                                theme: reply.theme,
                                project: reply.project,
                                cols: reply.cols,
                                rows: reply.rows,
                                claude: reply.claude,
                                scrollback_total: reply.scrollback_total,
                            };
                            if let Ok(json) = serde_json::to_string(&snap)
                                && ws_sink.send(Message::Text(json)).await.is_err() {
                                    break;
                                }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Control events (extension + raw-mode clients for AI stats)
            event = control_rx.recv(), if control_mode || raw_mode => {
                match event {
                    Ok(json_arc) => {
                        // In raw mode, only forward Claude stats + title changes (not all control events)
                        let should_forward = if raw_mode {
                            json_arc.contains("\"event\":\"claude_update\"")
                                || json_arc.contains("\"event\":\"claude_exited\"")
                                || json_arc.contains("\"event\":\"attention\"")
                                || json_arc.contains("\"event\":\"working\"")
                                || json_arc.contains("\"event\":\"idle\"")
                                || json_arc.contains("\"type\":\"title_changed\"")
                                || json_arc.contains("\"type\":\"expression_update\"")
                                || json_arc.contains("\"type\":\"browser_frame\"")
                                || json_arc.contains("\"type\":\"browser_state\"")
                                || json_arc.contains("\"type\":\"browser_human_request\"")
                                || json_arc.contains("\"type\":\"browser_cursor\"")
                                || json_arc.contains("\"type\":\"browser_narration\"")
                        } else {
                            true
                        };
                        if should_forward
                            && ws_sink.send(Message::Text((*json_arc).clone())).await.is_err() {
                                break;
                            }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Control events are lightweight — safe to skip missed ones
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // AI layer updates for raw-mode clients (buttons, overlays drawn via MCP)
            ai_data = ai_layer_rx.recv(), if raw_mode => {
                match ai_data {
                    Ok(json_bytes) => {
                        // json_bytes is {"sb_len":N,"primitives":[...]}.
                        // Wrap it into a message the webview's handleServerMessage can parse.
                        let msg = format!(
                            "{{\"type\":\"ai_layer_update\",\"data\":{}}}",
                            String::from_utf8_lossy(&json_bytes)
                        );
                        if ws_sink.send(Message::Text(msg)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("AI layer WS subscriber lagged by {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Targeted JS eval into a specific primitive's Shadow DOM
            // (eval_in_primitive MCP tool — fire-and-forget).
            ai_eval = ai_eval_rx.recv(), if raw_mode => {
                match ai_eval {
                    Ok(json_str) => {
                        let msg = format!(
                            "{{\"type\":\"ai_eval\",\"data\":{}}}",
                            json_str
                        );
                        if ws_sink.send(Message::Text(msg)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Eval messages are best-effort — skip missed ones
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Workshop lifecycle events (open/update/eval/close) for the
            // persistent webview pane. Forward to ALL subscribers (raw and
            // control mode) — control-mode clients don't render the workshop
            // card but DO need the event to update sidebar workshop counts
            // and badges for non-active sessions. The client-side visibility
            // filter (syncWorkshopVisibility) hides cards on inactive sessions.
            ws_event = workshop_rx.recv() => {
                match ws_event {
                    Ok(json_str) => {
                        let msg = format!(
                            "{{\"type\":\"workshop_event\",\"data\":{}}}",
                            json_str
                        );
                        if ws_sink.send(Message::Text(msg)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Lagged subscribers may miss an update; the next
                        // open/update will re-establish state. Don't crash.
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Channel messages to forward to channel server client
            ch_msg = async {
                match channel_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match ch_msg {
                    Some(json) => {
                        if ws_sink.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        // Sender dropped — channel unregistered
                        channel_rx = None;
                    }
                }
            }
            // Client messages (via reader task — click events already broadcast)
            msg = client_msg_rx.recv() => {
                match msg {
                    Some(Message::Text(text)) => {
                        // Check for subscribe_raw before normal handling
                        if let Ok(WsClientMsg::SubscribeRaw { full_snapshot, .. }) = serde_json::from_str::<WsClientMsg>(&text) {
                            // Transition to raw mode
                            let (tx, rx) = oneshot::channel();
                            if cmd_tx.send(WsCommand::SubscribeRaw { reply: tx, full_snapshot }).await.is_err() {
                                break;
                            }
                            if let Ok(reply) = rx.await {
                                info!("WebSocket client switched to raw PTY mode");
                                if !raw_mode {
                                    raw_subscriber_count
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                                raw_mode = true;
                                control_mode = false; // Upgrade from control to raw
                                pty_rx = Some(reply.pty_rx);
                                // Send snapshot
                                let snap = WsServerMsg::Snapshot {
                                    snapshot_json: reply.snapshot_json,
                                    session: reply.session,
                                    theme: reply.theme,
                                    project: reply.project,
                                    cols: reply.cols,
                                    rows: reply.rows,
                                    claude: reply.claude,
                                    scrollback_total: reply.scrollback_total,
                                };
                                if let Ok(json) = serde_json::to_string(&snap)
                                    && ws_sink.send(Message::Text(json)).await.is_err() {
                                        break;
                                    }
                            }
                        } else if let Ok(WsClientMsg::SubscribeControl { .. }) = serde_json::from_str::<WsClientMsg>(&text) {
                            // Request current state from event loop
                            let (tx, rx) = oneshot::channel();
                            if cmd_tx.send(WsCommand::GetControlState(tx)).await.is_err() {
                                break;
                            }
                            if let Ok(state) = rx.await {
                                info!("WebSocket client switched to control mode");
                                control_mode = true;
                                // MEMORY FIX: Downgrade from raw mode — stop receiving
                                // binary PTY data to prevent unbounded memory in the client.
                                if raw_mode {
                                    raw_subscriber_count
                                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                                }
                                raw_mode = false;
                                pty_rx = None;
                                // Send hello with initial state
                                let hello = WsServerMsg::ControlHello {
                                    session_name: state.session_name,
                                    window_id: state.window_id,
                                    display_name: state.display_name,
                                    claude: state.claude,
                                    last_user_prompt: state.last_user_prompt,
                                };
                                if let Ok(json) = serde_json::to_string(&hello)
                                    && ws_sink.send(Message::Text(json)).await.is_err() {
                                        break;
                                    }
                            }
                        } else if let Ok(WsClientMsg::RegisterChannel { immorterm_id, request_id }) = serde_json::from_str::<WsClientMsg>(&text) {
                            // Set up channel forwarding for this connection
                            let (tx, rx) = mpsc::channel::<String>(32);
                            channel_rx = Some(rx);
                            let _ = cmd_tx.send(WsCommand::RegisterChannel {
                                immorterm_id: immorterm_id.clone(),
                                channel_tx: tx,
                            }).await;
                            // Respond with acknowledgment
                            let ack = WsServerMsg::ChannelRegistered {
                                immorterm_id,
                                request_id,
                            };
                            if let Ok(json) = serde_json::to_string(&ack)
                                && ws_sink.send(Message::Text(json)).await.is_err() {
                                    break;
                                }
                        } else {
                            handle_client_message(&text, &cmd_tx, &mut ws_sink).await;
                        }
                    }
                    Some(Message::Ping(data)) => {
                        let _ = ws_sink.send(Message::Pong(data)).await;
                    }
                    Some(Message::Close(_)) | None => break,
                    _ => {} // Binary, Pong — ignore
                }
            }
        }
    }

    if raw_mode {
        // Decrement on disconnect so wait_for_event guards stay accurate
        // even when a raw client drops without going through subscribe_control.
        raw_subscriber_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
    info!("WebSocket client disconnected");
}

async fn handle_client_message(
    text: &str,
    cmd_tx: &mpsc::Sender<WsCommand>,
    ws_sink: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message,
    >,
) {
    let msg: WsClientMsg = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            let err = WsServerMsg::Error {
                request_id: None,
                message: format!("Invalid JSON: {}", e),
            };
            if let Ok(json) = serde_json::to_string(&err) {
                let _ = ws_sink.send(Message::Text(json)).await;
            }
            return;
        }
    };

    match msg {
        WsClientMsg::Input { data, .. } => {
            let _ = cmd_tx.send(WsCommand::Input(data.into_bytes())).await;
        }
        WsClientMsg::InputRaw { data, .. } => {
            if let Ok(bytes) = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &data,
            ) {
                let _ = cmd_tx.send(WsCommand::Input(bytes)).await;
            }
        }
        WsClientMsg::BrowserInput { kind, x, y, key, dy } => {
            // Map the wire shape → the poll event the MCP pump drains. Silently
            // drop malformed events (missing coords/key) — best-effort input.
            use crate::ipc::BrowserInputEvent;
            let event = match kind.as_str() {
                "click" => match (x, y) {
                    (Some(x), Some(y)) => Some(BrowserInputEvent::Click { x, y }),
                    _ => None,
                },
                "key" => key.map(|key| BrowserInputEvent::Key { key }),
                "scroll" => dy.map(|dy| BrowserInputEvent::Scroll { dy }),
                _ => None,
            };
            if let Some(event) = event {
                let _ = cmd_tx.send(WsCommand::BrowserInput(event)).await;
            }
        }
        WsClientMsg::BrowserControl { action } => {
            let _ = cmd_tx
                .send(WsCommand::BrowserInput(
                    crate::ipc::BrowserInputEvent::Control { action },
                ))
                .await;
        }
        WsClientMsg::ClipboardCheckImage { request_id } => {
            // Read from the user's pasteboard via arboard. Browsers' Async
            // Clipboard API only sees image/png, so the webview asks us to
            // detect any image format (JPEG/TIFF/etc) including Finder copies.
            let has_image = arboard::Clipboard::new()
                .and_then(|mut c| c.get_image())
                .is_ok();
            // Finder Cmd+C of a file (PDF, image, doc) puts BOTH a preview
            // thumbnail AND a file URL on the pasteboard. Without this check,
            // a PDF copy would be treated as a standalone image.
            let file_url = if has_image { read_clipboard_file_url() } else { None };
            let resp = WsServerMsg::ClipboardImagePresence { request_id, has_image, file_url };
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = ws_sink.send(Message::Text(json)).await;
            }
        }
        WsClientMsg::ClipboardSaveImage { request_id } => {
            // Cmd+Option+V — write the clipboard image to a temp PNG and
            // return the path. Path-as-text paste lets Claude Code read the
            // image via its Read tool only when needed, dodging the
            // many-image dimension cap that the inline [Image #N] flow hits.
            let path = save_clipboard_image_to_temp();
            let resp = WsServerMsg::ClipboardImageSaved { request_id, path };
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = ws_sink.send(Message::Text(json)).await;
            }
        }
        WsClientMsg::ClipboardSaveImageBytes { request_id, png_base64 } => {
            // Same as ClipboardSaveImage but the webview provides the
            // bytes — the daemon doesn't need to read its own (possibly
            // empty) OS clipboard. The point of this path: on a
            // remote-bound tab, the daemon runs in a headless Linux
            // container with no clipboard access; the bytes come over
            // the SSH tunnel from the user's Mac.
            use base64::Engine;
            let path = match base64::engine::general_purpose::STANDARD.decode(&png_base64) {
                Ok(bytes) => save_image_bytes_to_temp(&bytes),
                Err(e) => {
                    warn!("[clipboard] decode base64 image: {e}");
                    None
                }
            };
            let resp = WsServerMsg::ClipboardImageSaved { request_id, path };
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = ws_sink.send(Message::Text(json)).await;
            }
        }
        WsClientMsg::WriteShareItem { request_id, window_id, item } => {
            // Land a dropped file / shared session / task in THIS host's
            // pending-share queue. On a remote-bound tab this daemon is the
            // remote host, so the remote session's hook will consume it.
            let ok = write_share_item_to_queue(&window_id, &item);
            let resp = WsServerMsg::ShareItemWritten { request_id, ok };
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = ws_sink.send(Message::Text(json)).await;
            }
        }
        WsClientMsg::DrawRect {
            x, y, width, height, color, border_color, border_width, anchor, request_id,
        } => {
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = cmd_tx.send(WsCommand::DrawRect {
                x, y, width, height, color, border_color, border_width, anchor,
                reply: reply_tx,
            }).await;
            if let Ok(reply) = reply_rx.await {
                let resp = WsServerMsg::DrawResult {
                    request_id,
                    id: reply.id,
                    ptype: reply.ptype,
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = ws_sink.send(Message::Text(json)).await;
                }
            }
        }
        WsClientMsg::DrawText {
            text, x, y, color, font_size_scale, anchor, request_id,
        } => {
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = cmd_tx.send(WsCommand::DrawText {
                text, x, y, color, font_size_scale, anchor,
                reply: reply_tx,
            }).await;
            if let Ok(reply) = reply_rx.await {
                let resp = WsServerMsg::DrawResult {
                    request_id,
                    id: reply.id,
                    ptype: reply.ptype,
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = ws_sink.send(Message::Text(json)).await;
                }
            }
        }
        WsClientMsg::DrawButton {
            text, x, y, width, height, bg_color, text_color, anchor, request_id,
        } => {
            // Click broadcasts are handled by the WS reader task (spawned in
            // handle_ws_connection) to avoid deadlock with the main event loop.
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = cmd_tx.send(WsCommand::DrawButton {
                text, x, y, width, height, bg_color, text_color, anchor,
                reply: reply_tx,
            }).await;
            if let Ok(reply) = reply_rx.await {
                let resp = WsServerMsg::DrawResult {
                    request_id,
                    id: reply.id,
                    ptype: reply.ptype,
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = ws_sink.send(Message::Text(json)).await;
                }
            }
        }
        WsClientMsg::DrawLine {
            x1, y1, x2, y2, color, thickness, anchor, request_id,
        } => {
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = cmd_tx.send(WsCommand::DrawLine {
                x1, y1, x2, y2, color, thickness, anchor,
                reply: reply_tx,
            }).await;
            if let Ok(reply) = reply_rx.await {
                let resp = WsServerMsg::DrawResult {
                    request_id,
                    id: reply.id,
                    ptype: reply.ptype,
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = ws_sink.send(Message::Text(json)).await;
                }
            }
        }
        WsClientMsg::RemovePrimitive { id, request_id } => {
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = cmd_tx.send(WsCommand::RemovePrimitive { id, reply: reply_tx }).await;
            if let Ok(Err(e)) = reply_rx.await {
                let err = WsServerMsg::Error {
                    request_id,
                    message: e,
                };
                if let Ok(json) = serde_json::to_string(&err) {
                    let _ = ws_sink.send(Message::Text(json)).await;
                }
            }
        }
        WsClientMsg::ClearAiLayer { .. } => {
            let _ = cmd_tx.send(WsCommand::ClearAiLayer).await;
        }
        WsClientMsg::CloseWorkshop { name, request_id: _ } => {
            let _ = cmd_tx.send(WsCommand::CloseWorkshop { name }).await;
        }
        WsClientMsg::Animate {
            primitive_id, property, from, to, duration_ms, easing, request_id,
        } => {
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = cmd_tx.send(WsCommand::Animate {
                primitive_id, property, from, to, duration_ms, easing,
                reply: reply_tx,
            }).await;
            if let Ok(Err(e)) = reply_rx.await {
                let err = WsServerMsg::Error {
                    request_id,
                    message: e,
                };
                if let Ok(json) = serde_json::to_string(&err) {
                    let _ = ws_sink.send(Message::Text(json)).await;
                }
            }
        }
        WsClientMsg::Resize { cols, rows, .. } => {
            let _ = cmd_tx.send(WsCommand::Resize { cols, rows }).await;
        }
        WsClientMsg::RerenderBacklog { .. } => {
            let _ = cmd_tx.send(WsCommand::RerenderBacklog).await;
        }
        WsClientMsg::ReconnectAi { .. } => {
            let _ = cmd_tx.send(WsCommand::ReconnectAi).await;
        }
        WsClientMsg::DismissAttention { .. } => {
            let _ = cmd_tx.send(WsCommand::DismissAttention).await;
        }
        WsClientMsg::SubscribeRaw { .. } => {
            // Handled inline in handle_ws_connection before reaching this function.
            // This arm exists only for exhaustive match.
        }
        WsClientMsg::SubscribeControl { .. } => {
            // Handled inline in handle_ws_connection before reaching this function.
        }
        WsClientMsg::SubscribeTeam { team_name: _, request_id } => {
            // Team subscriptions are handled at a higher level (team_hub).
            // Individual session WebSockets don't serve team state directly.
            let err = WsServerMsg::Error {
                request_id,
                message: "subscribe_team is only supported on team hub WebSocket".into(),
            };
            if let Ok(json) = serde_json::to_string(&err) {
                let _ = ws_sink.send(Message::Text(json)).await;
            }
        }
        WsClientMsg::SendTeamMessage { team_name, recipient, content, request_id } => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            match crate::team_watcher::send_team_message(&home, &team_name, &recipient, &content) {
                Ok(()) => {
                    let resp = WsServerMsg::DrawResult {
                        request_id,
                        id: 0,
                        ptype: "message_sent".into(),
                    };
                    if let Ok(json) = serde_json::to_string(&resp) {
                        let _ = ws_sink.send(Message::Text(json)).await;
                    }
                }
                Err(e) => {
                    let err = WsServerMsg::Error {
                        request_id,
                        message: format!("Failed to send: {}", e),
                    };
                    if let Ok(json) = serde_json::to_string(&err) {
                        let _ = ws_sink.send(Message::Text(json)).await;
                    }
                }
            }
        }
        WsClientMsg::ScrollRequest { offset, count, request_id: _ } => {
            let (tx, rx) = oneshot::channel();
            let _ = cmd_tx.send(WsCommand::ScrollRequest {
                offset,
                count: count.min(200), // cap to prevent abuse
                reply: tx,
            }).await;
            if let Ok(reply) = rx.await {
                let msg = WsServerMsg::ScrollbackRows {
                    offset: reply.offset,
                    rows_json: reply.rows_json,
                    total: reply.total,
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = ws_sink.send(Message::Text(json)).await;
                }
            }
        }
        WsClientMsg::Ping => {
            let pong = WsServerMsg::Pong;
            if let Ok(json) = serde_json::to_string(&pong) {
                let _ = ws_sink.send(Message::Text(json)).await;
            }
        }
        WsClientMsg::RegisterChannel { .. } => {
            // Handled inline in handle_ws_connection (like SubscribeRaw).
        }
        WsClientMsg::ChannelMessage { to_immorterm_id, message, from_name, request_id } => {
            // Write message to target's inbox file (like SendTeamMessage uses file IPC)
            let inbox_dir = {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                std::path::PathBuf::from(home).join(".immorterm").join("channel-inbox")
            };
            let msg = crate::channel_registry::ChannelMessage {
                from_immorterm_id: String::new(), // filled by daemon from session context
                from_name: from_name.unwrap_or_default(),
                message,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            };
            match crate::channel_registry::write_to_inbox(&inbox_dir, &to_immorterm_id, &msg) {
                Ok(()) => {
                    let resp = WsServerMsg::DrawResult {
                        request_id,
                        id: 0,
                        ptype: "channel_message_sent".into(),
                    };
                    if let Ok(json) = serde_json::to_string(&resp) {
                        let _ = ws_sink.send(Message::Text(json)).await;
                    }
                }
                Err(e) => {
                    let err = WsServerMsg::Error {
                        request_id,
                        message: format!("Channel send failed: {}", e),
                    };
                    if let Ok(json) = serde_json::to_string(&err) {
                        let _ = ws_sink.send(Message::Text(json)).await;
                    }
                }
            }
        }
        WsClientMsg::PairSessions { source_id, target_id, source_name, target_name, request_id } => {
            // Forward to event loop for pairing
            let _ = cmd_tx.send(WsCommand::PairSessions {
                source_id,
                target_id,
                source_name,
                target_name,
            }).await;
            let resp = WsServerMsg::DrawResult {
                request_id,
                id: 0,
                ptype: "pair_sessions_ok".into(),
            };
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = ws_sink.send(Message::Text(json)).await;
            }
        }
        WsClientMsg::UnpairSessions { request_id } => {
            let _ = cmd_tx.send(WsCommand::UnpairSessions).await;
            let resp = WsServerMsg::DrawResult {
                request_id,
                id: 0,
                ptype: "unpair_sessions_ok".into(),
            };
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = ws_sink.send(Message::Text(json)).await;
            }
        }
    }
}

#[cfg(test)]
mod browser_input_tests {
    use super::WsClientMsg;

    // The webview forwards these EXACT wire shapes (gpu-terminal-browser.js).
    // A rename/tag drift here silently drops human input, so pin the contract.
    #[test]
    fn parses_webview_browser_input_shapes() {
        let click = r#"{"type":"browser_input","kind":"click","x":120.5,"y":40}"#;
        match serde_json::from_str::<WsClientMsg>(click).unwrap() {
            WsClientMsg::BrowserInput { kind, x, y, .. } => {
                assert_eq!(kind, "click");
                assert_eq!(x, Some(120.5));
                assert_eq!(y, Some(40.0));
            }
            _ => panic!("click did not parse as BrowserInput"),
        }

        let key = r#"{"type":"browser_input","kind":"key","key":"Enter"}"#;
        match serde_json::from_str::<WsClientMsg>(key).unwrap() {
            WsClientMsg::BrowserInput { kind, key, .. } => {
                assert_eq!(kind, "key");
                assert_eq!(key.as_deref(), Some("Enter"));
            }
            _ => panic!("key did not parse as BrowserInput"),
        }

        let scroll = r#"{"type":"browser_input","kind":"scroll","dy":-90}"#;
        match serde_json::from_str::<WsClientMsg>(scroll).unwrap() {
            WsClientMsg::BrowserInput { kind, dy, .. } => {
                assert_eq!(kind, "scroll");
                assert_eq!(dy, Some(-90.0));
            }
            _ => panic!("scroll did not parse as BrowserInput"),
        }
    }

    #[test]
    fn parses_webview_browser_control() {
        for action in ["pause", "continue"] {
            let msg = format!(r#"{{"type":"browser_control","action":"{action}"}}"#);
            match serde_json::from_str::<WsClientMsg>(&msg).unwrap() {
                WsClientMsg::BrowserControl { action: a } => assert_eq!(a, action),
                _ => panic!("{action} did not parse as BrowserControl"),
            }
        }
    }
}
