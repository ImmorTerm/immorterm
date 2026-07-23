//! Terminal emulation — implements `vte::Perform` to process ANSI/VT sequences.
//!
//! This is the brain of the terminal. It receives parsed escape sequences from
//! the VTE parser and updates the grid, cursor, and attributes accordingly.

use smallvec::SmallVec;
use unicode_width::UnicodeWidthChar;

use crate::cell::{CellAttrs, Color};
use crate::cursor::Cursor;
use crate::expression::{ExpressionMeta, ExpressionState};
use crate::graphics::GraphicsState;
use crate::grid::{Grid, Row};
use crate::marker::{expression_from_attrs, MarkerEvent, MarkerParser};
use crate::scrollback::Scrollback;

/// Combining marks attached to base characters (Hebrew niqqud, Arabic diacritics, etc.).
///
/// Stored as a sparse side-table keyed by (row, col) to keep `Cell` compact.
/// Most cells have no combining marks, so this adds zero overhead for ASCII/CJK text.
pub type CombiningMarks = std::collections::HashMap<(usize, usize), SmallVec<[char; 4]>>;

/// Terminal modes (DECSET/DECRST).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Modes {
    /// DECCKM: Application cursor keys
    pub application_cursor_keys: bool,
    /// DECAWM: Auto-wrap mode
    pub auto_wrap: bool,
    /// DECOM: Origin mode (cursor relative to scroll region)
    pub origin_mode: bool,
    /// Bracketed paste mode
    pub bracketed_paste: bool,
    /// Focus reporting
    pub focus_reporting: bool,
    /// Mouse tracking modes
    pub mouse_tracking: MouseMode,
    /// Mouse encoding format
    pub mouse_format: MouseFormat,
    /// Alternate screen buffer active
    pub alternate_screen: bool,
    /// DECTCEM: Cursor visible
    pub cursor_visible: bool,
    /// LNM: Line feed / new line mode
    pub linefeed_mode: bool,
    /// IRM: Insert mode
    pub insert_mode: bool,
    /// BiDi implicit mode (FreeDesktop Terminal BiDi Spec, mode 2501).
    /// When true, the terminal handles BiDi reordering at render time.
    /// When false (explicit mode), the application handles BiDi itself.
    #[serde(default = "crate::serde_true")]
    pub bidi_implicit: bool,
    /// DECSET 2026: Synchronized Output Mode. When set, renderers should
    /// defer painting until the mode is reset. Claude Code and other TUIs
    /// use this to batch streaming updates and avoid mid-frame flicker.
    /// Ephemeral — not persisted across snapshots (watchdog is consumer's job).
    #[serde(default, skip_serializing)]
    pub synchronized_update: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            application_cursor_keys: false,
            auto_wrap: true,
            origin_mode: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_tracking: MouseMode::None,
            mouse_format: MouseFormat::Normal,
            alternate_screen: false,
            cursor_visible: true,
            linefeed_mode: false,
            insert_mode: false,
            bidi_implicit: true, // ImmorTerm handles BiDi by default
            synchronized_update: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MouseMode {
    None,
    Press,
    PressRelease,
    Motion,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MouseFormat {
    Normal,
    Sgr,
    Utf8,
    Urxvt,
}

/// Tab stop positions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TabStops {
    stops: Vec<bool>,
}

impl TabStops {
    pub fn new(cols: usize) -> Self {
        let mut stops = vec![false; cols];
        // Default: every 8 columns
        for i in (0..cols).step_by(8) {
            stops[i] = true;
        }
        Self { stops }
    }

    /// Next tab stop after `col`.
    pub fn next(&self, col: usize) -> usize {
        for i in (col + 1)..self.stops.len() {
            if self.stops[i] {
                return i;
            }
        }
        self.stops.len().saturating_sub(1)
    }

    /// Set a tab stop at `col`.
    pub fn set(&mut self, col: usize) {
        if col < self.stops.len() {
            self.stops[col] = true;
        }
    }

    /// Clear a tab stop at `col`.
    pub fn clear(&mut self, col: usize) {
        if col < self.stops.len() {
            self.stops[col] = false;
        }
    }

    /// Clear all tab stops.
    pub fn clear_all(&mut self) {
        self.stops.fill(false);
    }

    /// Resize to new column count, maintaining existing stops.
    pub fn resize(&mut self, cols: usize) {
        let old_len = self.stops.len();
        self.stops.resize(cols, false);
        // Set default stops for new columns
        for i in old_len..cols {
            if i % 8 == 0 {
                self.stops[i] = true;
            }
        }
    }
}

/// serde helper for `HashMap<(usize, usize), V>` fields: JSON object keys must
/// be strings, and serde_json rejects tuple keys ("key must be a string"). We
/// serialize these maps as a sequence of `(key, value)` entries instead.
/// ponytail: a few lines beats pulling in serde_with.
mod tuple_key_map {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<S, V>(
        map: &HashMap<(usize, usize), V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        V: Serialize,
    {
        let entries: Vec<(&(usize, usize), &V)> = map.iter().collect();
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D, V>(
        deserializer: D,
    ) -> Result<HashMap<(usize, usize), V>, D::Error>
    where
        D: Deserializer<'de>,
        V: Deserialize<'de>,
    {
        let entries: Vec<((usize, usize), V)> = Vec::deserialize(deserializer)?;
        Ok(entries.into_iter().collect())
    }
}

/// Serializable snapshot of terminal state for IPC transfer.
///
/// Contains everything the GPU renderer needs to produce a screenshot.
/// The VTE parser and APC state are omitted (not needed for rendering,
/// not serializable, and cheap to recreate).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TerminalSnapshot {
    pub grid: Grid,
    pub scrollback: Scrollback,
    pub cursor: crate::cursor::Cursor,
    pub modes: Modes,
    pub graphics: GraphicsState,
    pub overlays: crate::overlays::OverlayState,
    pub ai_layer: crate::ai_layer::AiLayerState,
    pub title: String,
    pub cwd: String,
    pub last_exit_code: Option<i32>,
    pub prompt_state: PromptState,
    pub cols: usize,
    pub rows: usize,
    /// Current AI expression state (for snapshot consumers to apply effects).
    #[serde(default)]
    pub expression: ExpressionState,
    /// Per-cell color overrides from expression (sparse map).
    #[serde(
        default,
        with = "tuple_key_map",
        skip_serializing_if = "std::collections::HashMap::is_empty"
    )]
    pub expression_colors: std::collections::HashMap<(usize, usize), [f32; 4]>,
    /// Combining marks for grapheme clusters (Hebrew niqqud, etc.).
    #[serde(
        default,
        with = "tuple_key_map",
        skip_serializing_if = "CombiningMarks::is_empty"
    )]
    pub combining_marks: CombiningMarks,
}

/// APC (Application Program Command) parser state for Kitty graphics.
///
/// VTE's parser discards APC sequences, so we intercept `ESC _ G ... ESC \`
/// in the raw byte stream before VTE sees them.
#[derive(Debug, Clone)]
enum ApcState {
    /// Normal processing — not inside an APC sequence
    Normal,
    /// Saw ESC — waiting for `_` (APC) or passing through to VTE
    Escape,
    /// Inside APC — saw `ESC _`, waiting for first char to identify type
    ApcStart,
    /// Inside a Kitty graphics APC (`ESC _ G ...`) — accumulating data
    KittyGraphics(Vec<u8>),
    /// Inside a Kitty graphics APC — saw ESC, waiting for `\` to terminate
    KittyEscape(Vec<u8>),
}

/// Semantic prompt state from OSC 133 shell integration markers.
///
/// Tracks where the terminal is in the prompt/input/output lifecycle.
/// This enables prompt-aware features like command navigation, prompt
/// region highlighting, and intelligent copy-paste boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PromptState {
    /// No prompt markers received yet, or between commands
    #[default]
    Unknown,
    /// Prompt is being drawn (after A, before B)
    Prompt,
    /// User is typing input (after B, before C)
    Input,
    /// Command output is being produced (after C, before D)
    Output,
}

/// The main terminal state.
pub struct Terminal {
    /// Primary screen grid
    pub grid: Grid,
    /// Alternate screen grid (for fullscreen apps like vim)
    alt_grid: Grid,
    /// Scrollback history
    pub scrollback: Scrollback,
    /// Cursor state
    pub cursor: Cursor,
    /// Saved cursor for alternate screen
    alt_cursor: Cursor,
    /// Terminal modes
    pub modes: Modes,
    /// Tab stops
    pub tabs: TabStops,
    /// Graphics state (Kitty protocol images)
    pub graphics: GraphicsState,
    /// Window title (OSC 0/2)
    pub title: String,
    /// Current working directory (OSC 7)
    pub cwd: String,
    /// Last command exit code (OSC 133;D)
    pub last_exit_code: Option<i32>,
    /// Semantic prompt state (OSC 133 A/B/C/D markers)
    pub prompt_state: PromptState,
    /// Number of columns
    cols: usize,
    /// Number of rows
    rows: usize,
    /// VTE parser instance
    parser: vte::Parser,
    /// APC sequence parser state (for Kitty graphics interception)
    apc_state: ApcState,
    /// Overlay state (annotations, charts) — set via IPC/MCP
    pub overlays: crate::overlays::OverlayState,
    /// AI canvas layer — persistent drawing primitives and animations.
    pub ai_layer: crate::ai_layer::AiLayerState,
    /// Whether terminal state has changed since last checked
    pub dirty: bool,
    /// Pending prompt events for structured logging (drained by daemon).
    pending_prompt_events: Vec<crate::log::PromptEvent>,
    /// Pending AI stats from OSC 1337;ImmorTerm (drained by daemon).
    pub pending_ai_stats_event: Option<AiStatsOscEvent>,
    /// Pending generic ImmorTerm OSC events (drained by daemon).
    pub pending_immorterm_events: Vec<ImmorTermOscEvent>,
    /// AI Expression Protocol — current "emotional state" set by AI agents.
    /// Applied to every cell written from PTY. Changed via MCP tool, auto-detection,
    /// or inline markers.
    pub expression: ExpressionState,
    /// Cached compact metadata from `expression` (recomputed on change).
    pub expression_meta: ExpressionMeta,
    /// Per-cell color overrides from expression (sparse — only cells with explicit color).
    /// Key: (absolute_row, col), Value: RGBA color.
    pub expression_colors: std::collections::HashMap<(usize, usize), [f32; 4]>,
    /// Inline marker parser for `<<express>>` styling tags + ```im-html
    /// fenced overlay blocks.
    marker_parser: MarkerParser,
    /// Expression state set by inline `<<express>>` markers (separate from MCP global).
    /// When Some, overrides the global expression_meta for cell stamping.
    marker_expression: Option<ExpressionState>,
    /// Pending HTML blocks from ```im-html fences, drained by the daemon.
    pending_html_blocks: Vec<HtmlBlock>,
    /// Stashed attrs from the most recent ```im-html opener line,
    /// consumed when the closing fence fires to populate `HtmlBlock.attrs`.
    pending_html_attrs: Option<std::collections::HashMap<String, String>>,
    /// Pending reply bytes to write back to the PTY (DA1, DA2, XTVERSION, etc.).
    /// Drained by the daemon after each `process()` call.
    pending_replies: Vec<Vec<u8>>,
    /// Bell fired since last drain (BEL 0x07). Drained by daemon to notify frontend.
    pending_bell: bool,
    /// Pending OSC 52 clipboard writes (base64-decoded UTF-8). Drained by the
    /// WASM layer each frame and written to `navigator.clipboard` in JS.
    pending_clipboard_writes: Vec<String>,
    /// Pending desktop notifications from OSC 9 / OSC 777;notify / OSC 99.
    /// Drained by the WASM layer and surfaced via VS Code's notification API.
    pending_notifications: Vec<TerminalNotification>,
    /// OSC 8 hyperlinks — map of `cell.hyperlink_id` → URI. Populated as
    /// `ESC ] 8 ; params ; uri ST` opens new links; cells written between
    /// open and close carry the id. Look up URIs via `hyperlink_uri(id)`.
    hyperlink_uris: std::collections::HashMap<u16, String>,
    /// Next hyperlink id to allocate (1..=u16::MAX; 0 means "no link").
    /// Wraps at overflow — older links then dangle but the map stays bounded.
    next_hyperlink_id: u16,
    /// Currently-open OSC 8 link id; stamped onto every cell written between
    /// the open OSC and its matching close.
    current_hyperlink_id: u16,
    /// Kitty keyboard protocol mode stack.
    /// Each entry is a bitmask: bit 0 = disambiguate, bit 1 = report events,
    /// bit 2 = report alternates, bit 3 = report all, bit 4 = report text.
    kitty_keyboard_stack: Vec<u16>,
    /// Combining marks for grapheme clusters (Hebrew niqqud, Arabic diacritics, etc.).
    /// Keyed by (grid_row, col). Stored separately from Cell to keep Cell compact.
    pub combining_marks: CombiningMarks,
}

/// AI stats pushed via OSC 1337;ImmorTerm escape sequence.
/// Drained by the daemon after PTY processing.
#[derive(Debug, Clone, Default)]
pub struct AiStatsOscEvent {
    pub session_id: String,
    pub model: String,
    pub cost_usd: f64,
    pub context_pct: f64,
    pub transcript_path: String,
    pub permission_mode: Option<String>,
}

/// Generic ImmorTerm OSC event (evt=<type> key-value pairs).
/// Drained by the daemon after PTY processing.
#[derive(Debug, Clone)]
pub struct ImmorTermOscEvent {
    pub event_type: String,
    pub params: std::collections::HashMap<String, String>,
}

/// Desktop notification emitted by OSC 9 (iTerm2), OSC 777;notify (urxvt/gnome),
/// or OSC 99 (kitty). The frontend routes these to `vscode.window.showInformationMessage`
/// / `showWarningMessage` based on urgency.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TerminalNotification {
    /// Notification summary; None means the terminal only sent a body (OSC 9 + OSC 99 without title).
    pub title: Option<String>,
    pub body: String,
    pub urgency: NotificationUrgency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationUrgency {
    Low,
    Normal,
    Critical,
}

/// An HTML block emitted by a fenced ```im-html overlay.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HtmlBlock {
    /// The HTML content between the opener fence line and the closer fence line.
    pub content: String,
    /// Absolute row where the opener fence appeared (scrollback + cursor row).
    pub anchor_row: usize,
    /// Scrollback length at the time the block was parsed — needed for scroll anchoring.
    pub scrollback_at_creation: usize,
    /// Attributes from the opening tag (anchor, position, id, etc.).
    pub attrs: std::collections::HashMap<String, String>,
}

impl Terminal {
    /// Create a new terminal with the given dimensions.
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            grid: Grid::new(cols, rows),
            alt_grid: Grid::new(cols, rows),
            scrollback: Scrollback::new(50_000),
            cursor: Cursor::default(),
            alt_cursor: Cursor::default(),
            modes: Modes::default(),
            tabs: TabStops::new(cols),
            graphics: GraphicsState::new(),
            title: String::new(),
            cwd: String::new(),
            last_exit_code: None,
            prompt_state: PromptState::Unknown,
            cols,
            rows,
            parser: vte::Parser::new(),
            apc_state: ApcState::Normal,
            dirty: true,
            overlays: crate::overlays::OverlayState::new(),
            ai_layer: crate::ai_layer::AiLayerState::new(),
            pending_prompt_events: Vec::new(),
            pending_ai_stats_event: None,
            pending_immorterm_events: Vec::new(),
            expression: ExpressionState::new(),
            expression_meta: ExpressionMeta::NONE,
            expression_colors: std::collections::HashMap::new(),
            marker_parser: MarkerParser::new(),
            marker_expression: None,
            pending_html_blocks: Vec::new(),
            pending_html_attrs: None,
            pending_replies: Vec::new(),
            pending_bell: false,
            pending_clipboard_writes: Vec::new(),
            pending_notifications: Vec::new(),
            hyperlink_uris: std::collections::HashMap::new(),
            next_hyperlink_id: 1,
            current_hyperlink_id: 0,
            kitty_keyboard_stack: Vec::new(),
            combining_marks: CombiningMarks::new(),
        }
    }

    /// Set the AI expression state (from MCP tool, auto-detection, or inline markers).
    /// Recomputes the cached compact metadata and marks all rows dirty so the
    /// renderer applies the new global expression to the entire visible screen.
    pub fn set_expression(&mut self, state: ExpressionState) {
        self.expression_meta = state.to_meta();
        self.expression = state;
        // Mark all grid rows dirty so the renderer re-renders with the new expression
        for row_idx in 0..self.grid.num_rows() {
            if let Some(row) = self.grid.row_mut(row_idx) {
                row.dirty = true;
            }
        }
        self.dirty = true;
    }

    /// Reset expression to defaults.
    pub fn reset_expression(&mut self) {
        self.expression.reset();
        self.expression_meta = ExpressionMeta::NONE;
        // Mark all grid rows dirty so the renderer clears expression effects
        for row_idx in 0..self.grid.num_rows() {
            if let Some(row) = self.grid.row_mut(row_idx) {
                row.dirty = true;
            }
        }
        self.dirty = true;
    }

    /// Create a serializable snapshot of the current terminal state.
    ///
    /// Contains everything needed for GPU rendering (grid, cursor, colors,
    /// overlays) but omits VTE parser state and APC state.
    pub fn snapshot(&self) -> TerminalSnapshot {
        TerminalSnapshot {
            grid: self.grid.clone(),
            scrollback: self.scrollback.clone(),
            cursor: self.cursor.clone(),
            modes: self.modes.clone(),
            graphics: self.graphics.clone(),
            overlays: self.overlays.clone(),
            ai_layer: self.ai_layer.clone(),
            title: self.title.clone(),
            cwd: self.cwd.clone(),
            last_exit_code: self.last_exit_code,
            prompt_state: self.prompt_state,
            cols: self.cols,
            rows: self.rows,
            expression: self.expression.clone(),
            expression_colors: self.expression_colors.clone(),
            combining_marks: self.combining_marks.clone(),
        }
    }

    /// Create a viewport-only snapshot — identical to `snapshot()` but with
    /// an empty scrollback buffer. This reduces the JSON payload from ~180MB
    /// (50K scrollback rows) to ~720KB (viewport grid only).
    ///
    /// Scrollback is fetched on-demand via `ScrollRequest` WebSocket messages
    /// when the user scrolls up past the local buffer.
    pub fn snapshot_viewport_only(&self) -> TerminalSnapshot {
        TerminalSnapshot {
            grid: self.grid.clone(),
            scrollback: Scrollback::new(self.scrollback.max_lines()),
            cursor: self.cursor.clone(),
            modes: self.modes.clone(),
            graphics: self.graphics.clone(),
            overlays: self.overlays.clone(),
            ai_layer: self.ai_layer.clone(),
            title: self.title.clone(),
            cwd: self.cwd.clone(),
            last_exit_code: self.last_exit_code,
            prompt_state: self.prompt_state,
            cols: self.cols,
            rows: self.rows,
            expression: self.expression.clone(),
            expression_colors: self.expression_colors.clone(),
            combining_marks: self.combining_marks.clone(),
        }
    }

    /// Restore a Terminal from a snapshot (for client-side rendering).
    ///
    /// The parser and APC state are initialized fresh — this Terminal
    /// should only be used for rendering, not for processing new bytes.
    pub fn from_snapshot(snap: TerminalSnapshot) -> Self {
        Self {
            grid: snap.grid,
            alt_grid: Grid::new(snap.cols, snap.rows),
            scrollback: snap.scrollback,
            cursor: snap.cursor,
            alt_cursor: Cursor::default(),
            modes: snap.modes,
            tabs: TabStops::new(snap.cols),
            graphics: snap.graphics,
            title: snap.title,
            cwd: snap.cwd,
            last_exit_code: snap.last_exit_code,
            prompt_state: snap.prompt_state,
            cols: snap.cols,
            rows: snap.rows,
            parser: vte::Parser::new(),
            apc_state: ApcState::Normal,
            dirty: false,
            overlays: snap.overlays,
            ai_layer: snap.ai_layer,
            pending_prompt_events: Vec::new(),
            pending_ai_stats_event: None,
            pending_immorterm_events: Vec::new(),
            expression_meta: snap.expression.to_meta(),
            expression: snap.expression,
            expression_colors: snap.expression_colors,
            marker_parser: MarkerParser::new(),
            marker_expression: None,
            pending_html_blocks: Vec::new(),
            pending_html_attrs: None,
            pending_replies: Vec::new(),
            pending_bell: false,
            pending_clipboard_writes: Vec::new(),
            pending_notifications: Vec::new(),
            hyperlink_uris: std::collections::HashMap::new(),
            next_hyperlink_id: 1,
            current_hyperlink_id: 0,
            kitty_keyboard_stack: Vec::new(),
            combining_marks: snap.combining_marks,
        }
    }

    /// Process raw bytes from the PTY.
    ///
    /// Pipeline: PTY bytes → MarkerParser (strips `<<express>>` + ```im-html fences)
    ///         → APC interceptor (catches Kitty graphics) → VTE parser (renders text).
    ///
    /// Marker bytes are consumed; only `PassThrough` bytes reach the APC/VTE pipeline.
    pub fn process(&mut self, bytes: &[u8]) {
        let mut vte_parser = std::mem::take(&mut self.parser);
        let mut apc_state = std::mem::replace(&mut self.apc_state, ApcState::Normal);

        for &byte in bytes {
            // Step 1: Feed through inline marker parser
            let events = self.marker_parser.feed(byte);

            for event in events {
                match event {
                    MarkerEvent::PassThrough(b) => {
                        // Step 2: APC interceptor (Kitty graphics)
                        apc_state = Self::process_apc_byte(
                            self,
                            &mut vte_parser,
                            apc_state,
                            b,
                        );
                    }
                    MarkerEvent::ExpressStart(attrs) => {
                        let state = expression_from_attrs(&attrs);
                        let meta = state.to_meta();
                        self.marker_expression = Some(state);
                        self.expression_meta = meta;
                        self.dirty = true;
                    }
                    MarkerEvent::ExpressEnd => {
                        self.marker_expression = None;
                        // Restore global expression (from MCP tool)
                        self.expression_meta = self.expression.to_meta();
                        self.dirty = true;
                    }
                    MarkerEvent::ExpressReset => {
                        self.marker_expression = None;
                        self.expression.reset();
                        self.expression_meta = ExpressionMeta::NONE;
                        self.expression_colors.clear();
                        // Mark all rows dirty
                        for row_idx in 0..self.grid.num_rows() {
                            if let Some(row) = self.grid.row_mut(row_idx) {
                                row.dirty = true;
                            }
                        }
                        self.dirty = true;
                    }
                    MarkerEvent::HtmlStart(attrs) => {
                        // Stash attrs until HtmlEnd fires — the parser emits
                        // start and end as separate events.
                        self.pending_html_attrs = Some(attrs);
                    }
                    MarkerEvent::HtmlEnd(content) => {
                        let sb_len = self.scrollback.len();
                        self.pending_html_blocks.push(HtmlBlock {
                            content,
                            anchor_row: sb_len + self.cursor.row,
                            scrollback_at_creation: sb_len,
                            attrs: self.pending_html_attrs.take().unwrap_or_default(),
                        });
                        self.dirty = true;
                    }
                }
            }
        }

        self.apc_state = apc_state;
        self.parser = vte_parser;
    }

    /// Process a single byte through the APC interceptor.
    /// Returns the new APC state.
    fn process_apc_byte(
        term: &mut Terminal,
        vte_parser: &mut vte::Parser,
        apc_state: ApcState,
        byte: u8,
    ) -> ApcState {
        match apc_state {
            ApcState::Normal => {
                if byte == 0x1b {
                    ApcState::Escape
                } else {
                    vte_parser.advance(term, byte);
                    ApcState::Normal
                }
            }
            ApcState::Escape => {
                if byte == b'_' {
                    ApcState::ApcStart
                } else {
                    vte_parser.advance(term, 0x1b);
                    vte_parser.advance(term, byte);
                    ApcState::Normal
                }
            }
            ApcState::ApcStart => {
                if byte == b'G' {
                    ApcState::KittyGraphics(Vec::with_capacity(4096))
                } else {
                    vte_parser.advance(term, 0x1b);
                    vte_parser.advance(term, b'_');
                    vte_parser.advance(term, byte);
                    ApcState::Normal
                }
            }
            ApcState::KittyGraphics(mut buf) => {
                if byte == 0x1b {
                    ApcState::KittyEscape(buf)
                } else {
                    buf.push(byte);
                    ApcState::KittyGraphics(buf)
                }
            }
            ApcState::KittyEscape(buf) => {
                if byte == b'\\' {
                    let row = term.scrollback.len() + term.cursor.row;
                    let col = term.cursor.col;
                    term.graphics.process_command(&buf, row, col);
                    term.dirty = true;
                    ApcState::Normal
                } else {
                    let mut buf = buf;
                    buf.push(0x1b);
                    buf.push(byte);
                    ApcState::KittyGraphics(buf)
                }
            }
        }
    }

    /// Resize the terminal with content-aware reflow.
    ///
    /// When columns change, logical lines (connected by the `wrapped` flag) are
    /// re-wrapped at the new width — preserving content that would otherwise be
    /// truncated. Row count changes push/pull rows to/from scrollback.
    ///
    /// The alternate screen grid always uses simple resize (fullscreen apps
    /// like vim manage their own content).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let old_cols = self.cols;

        // Update dimension tracking
        self.cols = cols;
        self.rows = rows;

        // Alt grid: always simple resize (fullscreen apps handle their own layout)
        self.alt_grid.resize(cols, rows);

        // Alternate screen mode: alt-screen grid keeps its layout (TUI owns
        // it), but scrollback is primary-screen content captured before
        // alt-screen entry and must track current cols. Without this reflow,
        // scrolling up while a TUI is active reveals scrollback at the old
        // column width — visibly misaligned with the current viewport.
        if self.modes.alternate_screen {
            self.grid.resize(cols, rows);
            self.tabs.resize(cols);
            self.cursor.clamp(cols, rows);
            if cols != old_cols && !self.scrollback.is_empty() {
                let sb_rows = self.scrollback.take_all();
                let reflowed = reflow_scrollback_rows(sb_rows, cols);
                for row in reflowed {
                    self.scrollback.push(row);
                }
            }
            self.dirty = true;
            return;
        }

        // ── Column reflow ──
        //
        // Both scrollback AND grid reflow as one unified stream. Wrapped chains
        // are joined and re-wrapped at the new column width. Aligns with
        // Alacritty / xterm.js / WezTerm / Ghostty — all reflow scrollback on
        // column change. Logical lines are capped at MAX_LOGICAL_LINE_CAP cells
        // inside `reflow_grid_rows` to bound worst-case reflow time on
        // pathological wrapped chains (e.g. a 1MB JSON line).
        if cols != old_cols {
            let scrollback_rows = self.scrollback.take_all();
            let scrollback_count = scrollback_rows.len();
            let grid_rows = self.grid.take_rows();

            // Combine into one top-to-bottom stream. Cursor row is in grid
            // coords; shift into combined-stream coords for the reflow pass.
            let cursor_in_combined = scrollback_count + self.cursor.row;
            let mut combined = scrollback_rows;
            combined.extend(grid_rows);

            let (new_rows, new_cursor_row, new_cursor_col) = reflow_grid_rows(
                combined,
                old_cols,
                cols,
                cursor_in_combined,
                self.cursor.col,
            );

            // Split back: last `rows` rows go to grid, rest to scrollback.
            let total = new_rows.len();
            if total <= rows {
                // Rare: tiny scrollback + grid combined fits entirely in grid
                self.grid.replace_rows(new_rows, cols);
                self.cursor.row = new_cursor_row;
                self.cursor.col = new_cursor_col;
            } else {
                let mut split = total - rows;
                // Ensure cursor stays in the grid portion.
                if new_cursor_row < split {
                    split = new_cursor_row;
                }
                // Don't split in the middle of a wrapped logical line — walk
                // back to a line boundary (preceding row not wrapped).
                while split > 0 && new_rows[split - 1].wrapped {
                    split -= 1;
                }

                let mut iter = new_rows.into_iter();
                for _ in 0..split {
                    if let Some(row) = iter.next() {
                        self.scrollback.push(row);
                    }
                }
                let grid_rows_vec: Vec<Row> = iter.collect();
                self.grid.replace_rows(grid_rows_vec, cols);
                self.cursor.row = new_cursor_row - split;
                self.cursor.col = new_cursor_col;
            }
        }

        // ── Row adjustment (when only row count changed) ──
        let current = self.grid.row_count();
        if current > rows {
            let excess = current - rows;

            // First: trim empty rows below the cursor (they're unused padding)
            let mut trimmed = 0;
            while trimmed < excess && self.grid.row_count() > rows {
                let last_idx = self.grid.row_count() - 1;
                if last_idx <= self.cursor.row {
                    break; // Don't remove rows at or above cursor
                }
                let is_empty = self.grid.row(last_idx)
                    .is_some_and(|r| !r.wrapped && r.cells.iter().all(|c| c.is_default()));
                if !is_empty {
                    break;
                }
                self.grid.pop_last();
                trimmed += 1;
            }

            // Then: push remaining excess from top to scrollback
            let remaining = excess - trimmed;
            if remaining > 0 {
                for _ in 0..remaining {
                    let row = self.grid.remove_first();
                    self.scrollback.push(row);
                }
                self.cursor.row = self.cursor.row.saturating_sub(remaining);
            }
        } else if current < rows {
            // Pull from scrollback into top, or add empty rows at bottom
            let deficit = rows - current;
            let mut pulled = 0;
            let needs_reflow = cols != old_cols;
            for _ in 0..deficit {
                if let Some(mut row) = self.scrollback.pop_back() {
                    // Don't resize yet if we'll re-reflow — padding with defaults
                    // would corrupt the join (e.g. "HEL"+defaults+"LO" ≠ "HELLO")
                    if !needs_reflow {
                        row.resize(cols);
                    }
                    self.grid.insert_first(row);
                    pulled += 1;
                } else {
                    self.grid.push_row(Row::new(cols));
                }
            }
            // Complete any broken wrapped chain: if the new top of scrollback
            // has wrapped=true, it continues into the row we just pulled.
            // Pull the entire chain so the logical line isn't split.
            while !self.scrollback.is_empty() {
                let top_wrapped = self.scrollback
                    .get(self.scrollback.len() - 1)
                    .is_some_and(|r| r.wrapped);
                if !top_wrapped {
                    break;
                }
                if let Some(mut row) = self.scrollback.pop_back() {
                    if !needs_reflow {
                        row.resize(cols);
                    }
                    self.grid.insert_first(row);
                    pulled += 1;
                }
            }
            if pulled > 0 {
                self.cursor.row += pulled;

                // Pulled rows still have their original wrapped flags and widths.
                // Re-reflow the grid so wrapped chains (e.g. "HEL"(w)+"LO") merge
                // back into complete logical lines (e.g. "HELLO") at the current
                // column width.
                if needs_reflow {
                    let grid_rows = self.grid.take_rows();
                    let (new_rows, new_cr, new_cc) = reflow_grid_rows(
                        grid_rows,
                        old_cols,
                        cols,
                        self.cursor.row,
                        self.cursor.col,
                    );
                    self.grid.replace_rows(new_rows, cols);
                    self.cursor.row = new_cr;
                    self.cursor.col = new_cc;
                }
            }
        }

        // Finalize grid dimensions and scroll region
        self.grid.finalize(cols, rows);
        self.tabs.resize(cols);
        self.cursor.clamp(cols, rows);
        self.dirty = true;
    }

    /// Get the number of columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Get the number of rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Drain pending prompt events (called by the daemon after each `process()`).
    ///
    /// Returns all events since the last drain. The daemon uses these to trigger
    /// grid snapshots (on prompt detection) and other structured logging actions.
    pub fn drain_prompt_events(&mut self) -> Vec<crate::log::PromptEvent> {
        std::mem::take(&mut self.pending_prompt_events)
    }

    /// Drain pending HTML blocks from ```im-html fences.
    ///
    /// Called by the daemon in its render loop to forward HTML blocks to the
    /// webview via WebSocket.
    pub fn drain_html_blocks(&mut self) -> Vec<HtmlBlock> {
        std::mem::take(&mut self.pending_html_blocks)
    }

    /// Drain pending PTY reply bytes (DA1, DA2, XTVERSION, Kitty keyboard query).
    ///
    /// Called by the daemon after each `process()` to write responses back to the
    /// PTY. Programs (Claude Code, neovim, fish) send capability queries and expect
    /// the terminal to respond — without these replies they may fall back to
    /// degraded rendering or hang waiting for a response.
    pub fn drain_replies(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_replies)
    }

    /// Drain pending OSC 52 clipboard writes (base64-decoded UTF-8 strings).
    ///
    /// Called by the WASM layer each frame. JS then writes each entry to
    /// `navigator.clipboard.writeText`. Only the most recent write matters in
    /// practice — later writes overwrite earlier ones at the OS level.
    pub fn drain_clipboard_writes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_clipboard_writes)
    }

    /// Drain pending desktop notifications from OSC 9 / 777;notify / 99.
    ///
    /// Called by the WASM layer each frame. The frontend routes each entry
    /// to VS Code's notification API, falling back to the sidebar bell badge
    /// when the session is already focused.
    pub fn drain_notifications(&mut self) -> Vec<TerminalNotification> {
        std::mem::take(&mut self.pending_notifications)
    }

    /// Look up the URI for an OSC 8 hyperlink id (as stored on a cell).
    ///
    /// Returns `None` if `id == 0` (cell has no link) or if the id refers
    /// to a link that has been evicted by wrap-around.
    pub fn hyperlink_uri(&self, id: u16) -> Option<&str> {
        if id == 0 {
            return None;
        }
        self.hyperlink_uris.get(&id).map(|s| s.as_str())
    }

    /// Drain the pending bell flag (BEL 0x07).
    ///
    /// Returns true if a bell was received since the last drain. The daemon
    /// broadcasts this to WebSocket clients so the sidebar can show a 🔔 badge
    /// on inactive sessions that need attention.
    pub fn take_bell(&mut self) -> bool {
        std::mem::replace(&mut self.pending_bell, false)
    }

    /// Current Kitty keyboard mode flags (top of stack, or 0).
    pub fn kitty_keyboard_mode(&self) -> u16 {
        self.kitty_keyboard_stack.last().copied().unwrap_or(0)
    }

    // ── CSI > (xterm extensions) ──────────────────────────────────────────
    //
    // CSI > Ps c  — DA2 (Secondary Device Attributes): terminal identity.
    // CSI > Ps m  — modifyOtherKeys: xterm key modifier reporting (no-op, we use Kitty).
    // CSI > Ps q  — XTVERSION: terminal name/version query.
    // CSI > Ps u  — Kitty keyboard: push mode onto stack.
    //
    fn csi_dispatch_gt(&mut self, action: char, first_param: usize) {
        match action {
            // DA2 — Secondary Device Attributes
            'c' => {
                // Respond as VT100 (type 0), version from Cargo, no ROM cartridge.
                let version: u32 = {
                    let parts: Vec<&str> = env!("CARGO_PKG_VERSION").split('.').collect();
                    let major = parts.first().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                    let minor = parts.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                    let patch = parts.get(2).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
                    major * 10000 + minor * 100 + patch
                };
                let reply = format!("\x1b[>0;{};0c", version);
                self.pending_replies.push(reply.into_bytes());
            }
            // modifyOtherKeys — no-op (we support Kitty keyboard instead, like Alacritty)
            'm' => {}
            // XTVERSION — respond with terminal name and version
            'q' => {
                let reply = format!("\x1bP>|ImmorTerm {}\x1b\\", env!("CARGO_PKG_VERSION"));
                self.pending_replies.push(reply.into_bytes());
            }
            // Kitty keyboard: push mode onto stack
            'u' => {
                let flags = first_param as u16;
                // Cap stack depth at 16 (same as Alacritty)
                if self.kitty_keyboard_stack.len() >= 16 {
                    self.kitty_keyboard_stack.remove(0);
                }
                self.kitty_keyboard_stack.push(flags);
            }
            _ => {}
        }
    }

    // ── CSI < (kitty keyboard pop) ──────────────────────────────────────
    fn csi_dispatch_lt(&mut self, action: char, first_param: usize) {
        if action == 'u' {
            let new_len = self.kitty_keyboard_stack.len().saturating_sub(first_param);
            self.kitty_keyboard_stack.truncate(new_len);
        }
    }

    // ── CSI = (kitty keyboard set / DA3) ────────────────────────────────
    fn csi_dispatch_eq(&mut self, action: char, first_param: usize) {
        match action {
            // Kitty keyboard: set mode (replace top of stack, don't push)
            'u' => {
                let flags = first_param as u16;
                if let Some(top) = self.kitty_keyboard_stack.last_mut() {
                    *top = flags;
                } else {
                    self.kitty_keyboard_stack.push(flags);
                }
            }
            // DA3 — Tertiary Device Attributes (all-zeros unit ID)
            'c' => {
                self.pending_replies.push(b"\x1bP!|00000000\x1b\\".to_vec());
            }
            _ => {}
        }
    }

    /// Enable inline marker parsing (`<<express>>` + ```im-html fences).
    /// Off by default. Only the daemon should enable this —
    /// the WASM frontend leaves it disabled to avoid parsing
    /// literal marker text that appears in terminal output.
    pub fn enable_marker_parsing(&mut self) {
        self.marker_parser.enable();
    }

    /// Set the scrollback max.
    pub fn set_scrollback(&mut self, max: usize) {
        self.scrollback.set_max_lines(max);
    }

    /// Remove combining marks for a specific grid row.
    fn drain_combining_marks_row(&mut self, row: usize) {
        self.combining_marks.retain(|&(r, _), _| r != row);
    }

    /// Remove combining marks for cells in a range within a row.
    fn drain_combining_marks_range(&mut self, row: usize, col_from: usize, col_to: usize) {
        self.combining_marks
            .retain(|&(r, c), _| !(r == row && c >= col_from && c <= col_to));
    }

    /// Remove all combining marks (used on full grid clear).
    fn drain_all_combining_marks(&mut self) {
        self.combining_marks.clear();
    }

    /// Shift combining marks row indices when rows move (scroll).
    /// Decrements row indices in [from..=to] by 1, removing row `from`.
    fn shift_combining_marks_up(&mut self, from: usize, to: usize) {
        let mut shifted = CombiningMarks::new();
        for ((r, c), marks) in self.combining_marks.drain() {
            if r == from {
                // Row being evicted — drop its marks (or they went to scrollback)
                continue;
            }
            if r > from && r <= to {
                shifted.insert((r - 1, c), marks);
            } else {
                shifted.insert((r, c), marks);
            }
        }
        self.combining_marks = shifted;
    }

    /// Scroll up by one line, pushing evicted row to scrollback.
    fn scroll_up(&mut self) {
        let evicted = self.grid.scroll_up();
        // Shift combining marks: row indices in scroll region move up by 1
        self.shift_combining_marks_up(self.grid.scroll_top, self.grid.scroll_bottom);
        if !self.modes.alternate_screen {
            self.scrollback.push(evicted);
        }
        self.dirty = true;
    }

    /// Scroll down by one line.
    fn scroll_down(&mut self) {
        self.grid.scroll_down();
        // Bottom row is evicted, all rows in region shift down by 1
        self.drain_combining_marks_row(self.grid.scroll_bottom);
        let mut shifted = CombiningMarks::new();
        for ((r, c), marks) in self.combining_marks.drain() {
            if r >= self.grid.scroll_top && r < self.grid.scroll_bottom {
                shifted.insert((r + 1, c), marks);
            } else {
                shifted.insert((r, c), marks);
            }
        }
        self.combining_marks = shifted;
        self.dirty = true;
    }

    /// Write a character at the cursor position with current attributes.
    fn write_char(&mut self, c: char) {
        let char_width = UnicodeWidthChar::width(c).unwrap_or(0);

        if char_width == 0 {
            // Combining marks (Hebrew niqqud, Arabic diacritics, accents, etc.)
            // Attach to the previous base character's cell via the side-table.
            if self.cursor.col > 0 || self.cursor.pending_wrap {
                // pending_wrap: the base char was written at the LAST column and
                // the cursor did NOT advance — the base is at cursor.col itself,
                // not col-1. Getting this wrong scatters niqqud onto the
                // previous letter whenever a marked word straddles the wrap.
                let mut target_col = if self.cursor.pending_wrap {
                    self.cursor.col
                } else {
                    self.cursor.col - 1
                };
                // Wide-char continuation cell (width 0, blank) → step back to
                // the base cell so the mark joins the real grapheme.
                if target_col > 0
                    && self
                        .grid
                        .row(self.cursor.row)
                        .and_then(|r| r.cells.get(target_col))
                        .is_some_and(|cell| cell.width == 0)
                {
                    target_col -= 1;
                }
                self.combining_marks
                    .entry((self.cursor.row, target_col))
                    .or_default()
                    .push(c);
                // Emoji keycap (1️⃣ = digit + VS16 + U+20E3): brand the base
                // cell so renderers can route it to the emoji overlay even
                // after the row scrolls into scrollback (where the side-table
                // marks are dropped).
                if c == '\u{20E3}'
                    && let Some(row) = self.grid.row_mut(self.cursor.row)
                    && let Some(cell) = row.cells.get_mut(target_col)
                    && matches!(cell.grapheme, '0'..='9' | '#' | '*')
                {
                    cell.attrs.insert(CellAttrs::KEYCAP);
                }
                // Mark row dirty so renderer picks up the new combining mark
                if let Some(row) = self.grid.row_mut(self.cursor.row) {
                    row.dirty = true;
                }
                self.dirty = true;
            }
            return;
        }

        // Handle pending wrap
        if self.cursor.pending_wrap {
            if self.modes.auto_wrap {
                // Mark current row as soft-wrapped (line continues on next row)
                if let Some(row) = self.grid.row_mut(self.cursor.row) {
                    row.wrapped = true;
                }
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
                if self.cursor.row == self.grid.scroll_bottom {
                    self.scroll_up();
                } else if self.cursor.row + 1 < self.rows {
                    self.cursor.row += 1;
                }
            } else {
                // No auto-wrap: overwrite last column
                self.cursor.pending_wrap = false;
                self.cursor.col = self.cols.saturating_sub(1);
            }
        }

        // Insert mode: shift existing chars right
        if self.modes.insert_mode {
            self.grid.insert_chars(self.cursor.row, self.cursor.col, char_width);
        }

        // Wide character handling: need enough space
        if char_width == 2 && self.cursor.col + 1 >= self.cols {
            // Wide char doesn't fit — wrap to next line
            if self.modes.auto_wrap {
                // Mark current row as soft-wrapped
                if let Some(row) = self.grid.row_mut(self.cursor.row) {
                    row.wrapped = true;
                }
                // Fill current position with space
                if let Some(cell) = self.grid.cell_mut(self.cursor.row, self.cursor.col) {
                    cell.reset();
                }
                self.cursor.col = 0;
                if self.cursor.row == self.grid.scroll_bottom {
                    self.scroll_up();
                } else if self.cursor.row + 1 < self.rows {
                    self.cursor.row += 1;
                }
            }
        }

        // Write the character
        if let Some(cell) = self.grid.cell_mut(self.cursor.row, self.cursor.col) {
            cell.grapheme = c;
            cell.attrs = self.cursor.attrs;
            cell.fg = self.cursor.fg;
            cell.bg = self.cursor.bg;
            cell.underline_color = self.cursor.underline_color;
            cell.width = char_width as u8;
            // Stamp AI expression metadata (from MCP tool, auto-detection, or inline markers)
            cell.expression = self.expression_meta;
            // Stamp OSC 8 hyperlink id (0 = no link)
            cell.hyperlink_id = self.current_hyperlink_id;
            // Store explicit color override in sparse map (if expression has one)
            if let Some(color) = self.expression.color_override {
                let abs_row = self.scrollback.len() + self.cursor.row;
                self.expression_colors
                    .insert((abs_row, self.cursor.col), color);
            }
        }

        // Wide character: set continuation cell
        if char_width == 2 {
            let next_col = self.cursor.col + 1;
            if let Some(cell) = self.grid.cell_mut(self.cursor.row, next_col) {
                cell.grapheme = ' ';
                cell.width = 0; // continuation
                cell.attrs = self.cursor.attrs;
                cell.fg = self.cursor.fg;
                cell.bg = self.cursor.bg;
                cell.expression = self.expression_meta;
                cell.hyperlink_id = self.current_hyperlink_id;
            }
        }

        // Mark row dirty + track content end (non-space chars only).
        // Ink fills entire rows with styled spaces, so tracking all chars
        // would set content_end_col = terminal width on every rendered row.
        if let Some(row) = self.grid.row_mut(self.cursor.row) {
            row.dirty = true;
            if c != ' ' {
                let end = self.cursor.col + char_width;
                if end > row.content_end_col {
                    row.content_end_col = end;
                }
            }
        }

        // Advance cursor
        let new_col = self.cursor.col + char_width;
        if new_col >= self.cols {
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col = new_col;
        }

        self.dirty = true;
    }

    /// Process SGR (Select Graphic Rendition) parameters.
    fn process_sgr(&mut self, params: &vte::Params) {
        let mut iter = params.iter();

        while let Some(param) = iter.next() {
            match param {
                [0] | [] => {
                    // Reset
                    self.cursor.attrs = CellAttrs::empty();
                    self.cursor.fg = Color::Default;
                    self.cursor.bg = Color::Default;
                    self.cursor.underline_color = Color::Default;
                }
                [1] => self.cursor.attrs.insert(CellAttrs::BOLD),
                [2] => self.cursor.attrs.insert(CellAttrs::DIM),
                [3] => self.cursor.attrs.insert(CellAttrs::ITALIC),
                [4] => self.cursor.attrs.insert(CellAttrs::UNDERLINE),
                [4, 0] => {
                    self.cursor.attrs.remove(
                        CellAttrs::UNDERLINE
                            | CellAttrs::DOUBLE_UNDERLINE
                            | CellAttrs::CURLY_UNDERLINE
                            | CellAttrs::DOTTED_UNDERLINE
                            | CellAttrs::DASHED_UNDERLINE,
                    );
                }
                [4, 2] => {
                    self.cursor.attrs.remove(CellAttrs::UNDERLINE);
                    self.cursor.attrs.insert(CellAttrs::DOUBLE_UNDERLINE);
                }
                [4, 3] => {
                    self.cursor.attrs.remove(CellAttrs::UNDERLINE);
                    self.cursor.attrs.insert(CellAttrs::CURLY_UNDERLINE);
                }
                [4, 4] => {
                    self.cursor.attrs.remove(CellAttrs::UNDERLINE);
                    self.cursor.attrs.insert(CellAttrs::DOTTED_UNDERLINE);
                }
                [4, 5] => {
                    self.cursor.attrs.remove(CellAttrs::UNDERLINE);
                    self.cursor.attrs.insert(CellAttrs::DASHED_UNDERLINE);
                }
                [5] => self.cursor.attrs.insert(CellAttrs::BLINK),
                [7] => self.cursor.attrs.insert(CellAttrs::INVERSE),
                [8] => self.cursor.attrs.insert(CellAttrs::HIDDEN),
                [9] => self.cursor.attrs.insert(CellAttrs::STRIKETHROUGH),
                [21] => {
                    self.cursor.attrs.remove(CellAttrs::UNDERLINE);
                    self.cursor.attrs.insert(CellAttrs::DOUBLE_UNDERLINE);
                }
                [22] => self
                    .cursor
                    .attrs
                    .remove(CellAttrs::BOLD | CellAttrs::DIM),
                [23] => self.cursor.attrs.remove(CellAttrs::ITALIC),
                [24] => self.cursor.attrs.remove(
                    CellAttrs::UNDERLINE
                        | CellAttrs::DOUBLE_UNDERLINE
                        | CellAttrs::CURLY_UNDERLINE
                        | CellAttrs::DOTTED_UNDERLINE
                        | CellAttrs::DASHED_UNDERLINE,
                ),
                [25] => self.cursor.attrs.remove(CellAttrs::BLINK),
                [27] => self.cursor.attrs.remove(CellAttrs::INVERSE),
                [28] => self.cursor.attrs.remove(CellAttrs::HIDDEN),
                [29] => self.cursor.attrs.remove(CellAttrs::STRIKETHROUGH),
                // Standard foreground colors (30-37)
                [n @ 30..=37] => self.cursor.fg = Color::Indexed((n - 30) as u8),
                // Extended foreground
                [38] => {
                    if let Some(color) = self.parse_extended_color(&mut iter) {
                        self.cursor.fg = color;
                    }
                }
                [39] => self.cursor.fg = Color::Default,
                // Standard background colors (40-47)
                [n @ 40..=47] => self.cursor.bg = Color::Indexed((n - 40) as u8),
                // Extended background
                [48] => {
                    if let Some(color) = self.parse_extended_color(&mut iter) {
                        self.cursor.bg = color;
                    }
                }
                [49] => self.cursor.bg = Color::Default,
                // Bright foreground colors (90-97)
                [n @ 90..=97] => self.cursor.fg = Color::Indexed((n - 90 + 8) as u8),
                // Bright background colors (100-107)
                [n @ 100..=107] => self.cursor.bg = Color::Indexed((n - 100 + 8) as u8),
                // Underline color
                [58] => {
                    if let Some(color) = self.parse_extended_color(&mut iter) {
                        self.cursor.underline_color = color;
                    }
                }
                [59] => self.cursor.underline_color = Color::Default,
                _ => {} // Ignore unknown
            }
        }
    }

    /// Parse extended color (256-color or true color) from SGR parameter iterator.
    fn parse_extended_color<'a>(
        &self,
        iter: &mut impl Iterator<Item = &'a [u16]>,
    ) -> Option<Color> {
        match iter.next()? {
            [2] => {
                // True color: 38;2;R;G;B
                let r = iter.next()?.first().copied()? as u8;
                let g = iter.next()?.first().copied()? as u8;
                let b = iter.next()?.first().copied()? as u8;
                Some(Color::Rgb(r, g, b))
            }
            [5] => {
                // 256-color: 38;5;N
                let idx = iter.next()?.first().copied()? as u8;
                Some(Color::Indexed(idx))
            }
            _ => None,
        }
    }

    /// Process DECSET (private mode set).
    fn decset(&mut self, mode: u16) {
        match mode {
            1 => self.modes.application_cursor_keys = true,
            6 => self.modes.origin_mode = true,
            7 => self.modes.auto_wrap = true,
            12 => {} // Blinking cursor — visual only
            25 => {
                self.modes.cursor_visible = true;
                self.cursor.visible = true;
            }
            47 => self.switch_to_alt_screen(),
            1000 => self.modes.mouse_tracking = MouseMode::Press,
            1002 => self.modes.mouse_tracking = MouseMode::Motion,
            1003 => self.modes.mouse_tracking = MouseMode::Any,
            1004 => self.modes.focus_reporting = true,
            1005 => self.modes.mouse_format = MouseFormat::Utf8,
            1006 => self.modes.mouse_format = MouseFormat::Sgr,
            1015 => self.modes.mouse_format = MouseFormat::Urxvt,
            1049 => {
                // Save cursor + switch to alt screen
                self.cursor.save(self.modes.origin_mode);
                self.switch_to_alt_screen();
            }
            2004 => self.modes.bracketed_paste = true,
            // Synchronized output — defer paints until RST (see DECRST 2026).
            2026 => self.modes.synchronized_update = true,
            // FreeDesktop Terminal BiDi: implicit mode (terminal handles reordering)
            2501 => self.modes.bidi_implicit = true,
            _ => {} // Ignore unknown
        }
    }

    /// Process DECRST (private mode reset).
    fn decrst(&mut self, mode: u16) {
        match mode {
            1 => self.modes.application_cursor_keys = false,
            6 => self.modes.origin_mode = false,
            7 => self.modes.auto_wrap = false,
            12 => {} // Steady cursor
            25 => {
                self.modes.cursor_visible = false;
                self.cursor.visible = false;
            }
            47 => self.switch_to_primary_screen(),
            1000 | 1002 | 1003 => self.modes.mouse_tracking = MouseMode::None,
            1004 => self.modes.focus_reporting = false,
            1005 | 1006 | 1015 => self.modes.mouse_format = MouseFormat::Normal,
            1049 => {
                self.switch_to_primary_screen();
                self.cursor.restore();
            }
            2004 => self.modes.bracketed_paste = false,
            // Synchronized output end — flush accumulated changes by marking dirty.
            2026 => {
                self.modes.synchronized_update = false;
                self.dirty = true;
            }
            // FreeDesktop Terminal BiDi: explicit mode (app handles reordering)
            2501 => self.modes.bidi_implicit = false,
            _ => {}
        }
    }

    /// Switch to alternate screen buffer.
    fn switch_to_alt_screen(&mut self) {
        if !self.modes.alternate_screen {
            self.modes.alternate_screen = true;
            std::mem::swap(&mut self.grid, &mut self.alt_grid);
            std::mem::swap(&mut self.cursor, &mut self.alt_cursor);
            self.grid.clear();
            self.drain_all_combining_marks();
            self.dirty = true;
        }
    }

    /// Switch back to primary screen buffer.
    fn switch_to_primary_screen(&mut self) {
        if self.modes.alternate_screen {
            self.modes.alternate_screen = false;
            std::mem::swap(&mut self.grid, &mut self.alt_grid);
            std::mem::swap(&mut self.cursor, &mut self.alt_cursor);
            self.dirty = true;
        }
    }
}

impl vte::Perform for Terminal {
    /// Print a character to the terminal.
    fn print(&mut self, c: char) {
        self.cursor.cr_near_edge = false;
        self.write_char(c);
    }

    /// Execute a C0 control character.
    fn execute(&mut self, byte: u8) {
        match byte {
            // BEL
            0x07 => {
                self.pending_bell = true;
                self.dirty = true;
            }
            // BS (Backspace)
            0x08 => {
                self.cursor.pending_wrap = false;
                if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                }
            }
            // HT (Horizontal Tab)
            0x09 => {
                self.cursor.pending_wrap = false;
                self.cursor.col = self.tabs.next(self.cursor.col);
            }
            // LF, VT, FF (Line Feed, Vertical Tab, Form Feed)
            0x0A..=0x0C => {
                if let Some(row) = self.grid.row_mut(self.cursor.row) {
                    // Not auto-wrapped
                    row.wrapped = false;
                    // If CR just arrived from a near-full line, this is app word-wrap
                    row.soft_wrapped = self.cursor.cr_near_edge;
                }
                self.cursor.cr_near_edge = false;
                if self.cursor.row == self.grid.scroll_bottom {
                    self.scroll_up();
                } else if self.cursor.row + 1 < self.rows {
                    self.cursor.row += 1;
                }
                if self.modes.linefeed_mode {
                    self.cursor.col = 0;
                }
                self.cursor.pending_wrap = false;
                self.dirty = true;
            }
            // CR (Carriage Return)
            0x0D => {
                // Track whether CR arrived near the line edge (≥75% of width).
                // If LF follows, the row is marked soft_wrapped (app word-wrap).
                // A no-op CR (cursor already at col 0) preserves the flag —
                // Ink sometimes sends \r\r\n where the second \r is redundant.
                if self.cursor.col > 0 || self.cursor.pending_wrap {
                    let threshold = self.cols * 3 / 4;
                    self.cursor.cr_near_edge = self.cursor.col >= threshold
                        || self.cursor.pending_wrap;
                }
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            // SO (Shift Out) / SI (Shift In) — charset switching, ignored
            0x0E | 0x0F => {}
            _ => {}
        }
    }

    /// A CSI (Control Sequence Introducer) dispatch.
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let first_param = |default: usize| -> usize {
            params
                .iter()
                .next()
                .and_then(|p| p.first().copied())
                .map(|v| if v == 0 { default } else { v as usize })
                .unwrap_or(default)
        };

        let is_private = intermediates.first() == Some(&b'?');

        // Clear cr_near_edge on CSI sequences that aren't part of the
        // Ink/React word-wrap pattern: CR → [optional CUF indent] → CUD(1)
        // Keep cr_near_edge alive through CUD ('B') and CUF ('C').
        if !(action == 'B' || action == 'C') || is_private {
            self.cursor.cr_near_edge = false;
        }

        // ── CSI sequences with '>', '<', '=' intermediates ──────────────────
        // These are xterm/kitty extensions that must NOT fall through to the
        // standard handler (e.g., CSI > 4 m is NOT SGR 4/underline).
        match intermediates.first() {
            Some(b'>') => {
                self.csi_dispatch_gt(action, first_param(0));
                return;
            }
            Some(b'<') => {
                self.csi_dispatch_lt(action, first_param(1));
                return;
            }
            Some(b'=') => {
                self.csi_dispatch_eq(action, first_param(0));
                return;
            }
            _ => {} // '?' handled below, empty handled below
        }

        match (action, is_private) {
            // CUU — Cursor Up
            ('A', false) => {
                let n = first_param(1);
                let top = if self.modes.origin_mode {
                    self.grid.scroll_top
                } else {
                    0
                };
                self.cursor.row = self.cursor.row.saturating_sub(n).max(top);
                self.cursor.pending_wrap = false;
            }
            // CUD — Cursor Down
            ('B', false) => {
                let n = first_param(1);
                // Detect app word-wrap: CR near edge → CUD 1 (used by Ink/React CLI)
                if n == 1 && self.cursor.cr_near_edge
                    && let Some(row) = self.grid.row_mut(self.cursor.row)
                {
                    row.soft_wrapped = true;
                }
                self.cursor.cr_near_edge = false;
                let bottom = if self.modes.origin_mode {
                    self.grid.scroll_bottom
                } else {
                    self.rows - 1
                };
                self.cursor.row = (self.cursor.row + n).min(bottom);
                self.cursor.pending_wrap = false;
            }
            // CUF — Cursor Forward
            ('C', false) => {
                let n = first_param(1);
                self.cursor.col = (self.cursor.col + n).min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            // CUB — Cursor Backward
            ('D', false) => {
                let n = first_param(1);
                self.cursor.col = self.cursor.col.saturating_sub(n);
                self.cursor.pending_wrap = false;
            }
            // CNL — Cursor Next Line
            ('E', false) => {
                let n = first_param(1);
                self.cursor.col = 0;
                self.cursor.row = (self.cursor.row + n).min(self.rows - 1);
                self.cursor.pending_wrap = false;
            }
            // CPL — Cursor Previous Line
            ('F', false) => {
                let n = first_param(1);
                self.cursor.col = 0;
                self.cursor.row = self.cursor.row.saturating_sub(n);
                self.cursor.pending_wrap = false;
            }
            // CHA — Cursor Character Absolute (column)
            ('G', false) => {
                let col = first_param(1).saturating_sub(1);
                self.cursor.col = col.min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            // CUP / HVP — Cursor Position
            ('H', false) | ('f', false) => {
                let mut piter = params.iter();
                let row = piter
                    .next()
                    .and_then(|p| p.first().copied())
                    .map(|v| if v == 0 { 1 } else { v as usize })
                    .unwrap_or(1)
                    .saturating_sub(1);
                let col = piter
                    .next()
                    .and_then(|p| p.first().copied())
                    .map(|v| if v == 0 { 1 } else { v as usize })
                    .unwrap_or(1)
                    .saturating_sub(1);

                let (row_offset, max_row) = if self.modes.origin_mode {
                    (self.grid.scroll_top, self.grid.scroll_bottom)
                } else {
                    (0, self.rows - 1)
                };
                self.cursor.row = (row + row_offset).min(max_row);
                self.cursor.col = col.min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            // ED — Erase in Display
            ('J', false) => {
                match first_param(0) {
                    0 => {
                        self.grid.erase_below(self.cursor.row, self.cursor.col);
                        // Clear combining marks from cursor to end of screen
                        self.drain_combining_marks_range(self.cursor.row, self.cursor.col, self.cols);
                        for r in (self.cursor.row + 1)..self.rows {
                            self.drain_combining_marks_row(r);
                        }
                    }
                    1 => {
                        self.grid.erase_above(self.cursor.row, self.cursor.col);
                        for r in 0..self.cursor.row {
                            self.drain_combining_marks_row(r);
                        }
                        self.drain_combining_marks_range(self.cursor.row, 0, self.cursor.col);
                    }
                    2 | 3 => {
                        self.grid.clear();
                        self.drain_all_combining_marks();
                    }
                    _ => {}
                }
                self.dirty = true;
            }
            // EL — Erase in Line
            ('K', false) => {
                match first_param(0) {
                    0 => {
                        self.grid.erase_line_right(self.cursor.row, self.cursor.col);
                        self.drain_combining_marks_range(self.cursor.row, self.cursor.col, self.cols);
                    }
                    1 => {
                        self.grid.erase_line_left(self.cursor.row, self.cursor.col);
                        self.drain_combining_marks_range(self.cursor.row, 0, self.cursor.col);
                    }
                    2 => {
                        self.grid.erase_line(self.cursor.row);
                        self.drain_combining_marks_row(self.cursor.row);
                    }
                    _ => {}
                }
                self.dirty = true;
            }
            // IL — Insert Lines
            ('L', false) => {
                let n = first_param(1);
                if self.cursor.row >= self.grid.scroll_top
                    && self.cursor.row <= self.grid.scroll_bottom
                {
                    self.grid.insert_lines(self.cursor.row, n);
                    self.dirty = true;
                }
            }
            // DL — Delete Lines
            ('M', false) => {
                let n = first_param(1);
                if self.cursor.row >= self.grid.scroll_top
                    && self.cursor.row <= self.grid.scroll_bottom
                {
                    self.grid.delete_lines(self.cursor.row, n);
                    self.dirty = true;
                }
            }
            // DCH — Delete Characters
            ('P', false) => {
                let n = first_param(1);
                self.grid.delete_chars(self.cursor.row, self.cursor.col, n);
                self.dirty = true;
            }
            // SU — Scroll Up
            ('S', false) => {
                let n = first_param(1);
                for _ in 0..n {
                    self.scroll_up();
                }
            }
            // SD — Scroll Down
            ('T', false) => {
                let n = first_param(1);
                for _ in 0..n {
                    self.scroll_down();
                }
            }
            // ECH — Erase Characters
            ('X', false) => {
                let n = first_param(1);
                for i in 0..n {
                    let col = self.cursor.col + i;
                    if col < self.cols
                        && let Some(cell) = self.grid.cell_mut(self.cursor.row, col) {
                            cell.reset();
                        }
                }
                self.dirty = true;
            }
            // ICH — Insert Characters
            ('@', false) => {
                let n = first_param(1);
                self.grid
                    .insert_chars(self.cursor.row, self.cursor.col, n);
                self.dirty = true;
            }
            // VPA — Vertical Position Absolute
            ('d', false) => {
                let row = first_param(1).saturating_sub(1).min(self.rows - 1);
                self.cursor.row = row;
                self.cursor.pending_wrap = false;
            }
            // SGR — Select Graphic Rendition
            ('m', false) => {
                self.process_sgr(params);
            }
            // DA1 — Primary Device Attributes
            ('c', false) => {
                // Respond as VT220 with ANSI color, sixel (like Alacritty).
                // Applications (vim, tmux, Claude Code) use this to detect capabilities.
                self.pending_replies
                    .push(b"\x1b[?62;22c".to_vec());
            }
            // DECSTBM — Set Scrolling Region
            ('r', false) if !is_private => {
                let mut piter = params.iter();
                let top = piter
                    .next()
                    .and_then(|p| p.first().copied())
                    .map(|v| if v == 0 { 1 } else { v as usize })
                    .unwrap_or(1)
                    .saturating_sub(1);
                let bottom = piter
                    .next()
                    .and_then(|p| p.first().copied())
                    .map(|v| v as usize)
                    .unwrap_or(self.rows)
                    .saturating_sub(1)
                    .min(self.rows - 1);

                if top < bottom {
                    self.grid.scroll_top = top;
                    self.grid.scroll_bottom = bottom;
                    // Cursor moves to home position
                    self.cursor.col = 0;
                    self.cursor.row = if self.modes.origin_mode { top } else { 0 };
                    self.cursor.pending_wrap = false;
                }
            }
            // DECSET — Private mode set
            ('h', true) => {
                for param in params.iter() {
                    if let Some(&mode) = param.first() {
                        self.decset(mode);
                    }
                }
            }
            // SM — Set Mode
            ('h', false) => {
                for param in params.iter() {
                    if let Some(&mode) = param.first() {
                        match mode {
                            4 => self.modes.insert_mode = true,
                            20 => self.modes.linefeed_mode = true,
                            _ => {}
                        }
                    }
                }
            }
            // DECRST — Private mode reset
            ('l', true) => {
                for param in params.iter() {
                    if let Some(&mode) = param.first() {
                        self.decrst(mode);
                    }
                }
            }
            // RM — Reset Mode
            ('l', false) => {
                for param in params.iter() {
                    if let Some(&mode) = param.first() {
                        match mode {
                            4 => self.modes.insert_mode = false,
                            20 => self.modes.linefeed_mode = false,
                            _ => {}
                        }
                    }
                }
            }
            // DECSCUSR — Set Cursor Shape
            ('q', false) if intermediates.first() == Some(&b' ') => {
                match first_param(0) {
                    0 | 1 => self.cursor.shape = crate::cursor::CursorShape::Block,
                    2 => self.cursor.shape = crate::cursor::CursorShape::Block,
                    3 | 4 => self.cursor.shape = crate::cursor::CursorShape::Underline,
                    5 | 6 => self.cursor.shape = crate::cursor::CursorShape::Bar,
                    _ => {}
                }
            }
            // TBC — Tab Clear
            ('g', false) => {
                match first_param(0) {
                    0 => self.tabs.clear(self.cursor.col),
                    3 => self.tabs.clear_all(),
                    _ => {}
                }
            }
            // DSR — Device Status Report
            ('n', false) => {
                match first_param(0) {
                    // Status report → "OK"
                    5 => self.pending_replies.push(b"\x1b[0n".to_vec()),
                    // Cursor position report
                    6 => {
                        let reply = format!(
                            "\x1b[{};{}R",
                            self.cursor.row + 1,
                            self.cursor.col + 1
                        );
                        self.pending_replies.push(reply.into_bytes());
                    }
                    _ => {}
                }
            }
            // Kitty keyboard query: CSI ? u — respond with current mode flags
            ('u', true) => {
                let flags = self.kitty_keyboard_mode();
                let reply = format!("\x1b[?{}u", flags);
                self.pending_replies.push(reply.into_bytes());
            }
            _ => {
                // Unknown CSI sequence — ignore
            }
        }
    }

    /// OSC (Operating System Command) dispatch.
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }

        let cmd = params[0];
        match cmd {
            // OSC 0 or 2 — Set window title
            b"0" | b"2" => {
                if let Some(title_bytes) = params.get(1)
                    && let Ok(title) = std::str::from_utf8(title_bytes) {
                        self.title = title.to_string();
                        self.dirty = true;
                    }
            }
            // OSC 7 — Current working directory
            // Format: file://hostname/path/to/dir
            b"7" => {
                if let Some(uri_bytes) = params.get(1)
                    && let Ok(uri) = std::str::from_utf8(uri_bytes) {
                        // Parse file:// URI → extract path
                        if let Some(path) = uri.strip_prefix("file://") {
                            // Skip hostname: file://hostname/path → /path
                            if let Some(slash) = path.find('/') {
                                self.cwd = path[slash..].to_string();
                            } else {
                                self.cwd = path.to_string();
                            }
                        } else {
                            // Bare path (some shells send this)
                            self.cwd = uri.to_string();
                        }
                        self.dirty = true;
                    }
            }
            // OSC 8 — Hyperlinks
            // Format: OSC 8 ; params ; uri ST
            // `params` is a colon-separated key=value list (we only read `id=`,
            // but ignore its value — our internal id space is independent).
            // Empty `uri` (or no uri param) closes the currently-open link.
            b"8" => {
                const OSC8_MAX_URI: usize = 2048;
                let uri_bytes = params.get(2).copied().unwrap_or(b"");
                if uri_bytes.is_empty() {
                    self.current_hyperlink_id = 0;
                } else if uri_bytes.len() <= OSC8_MAX_URI
                    && let Ok(uri) = std::str::from_utf8(uri_bytes)
                {
                    let id = self.next_hyperlink_id;
                    self.next_hyperlink_id = self.next_hyperlink_id.checked_add(1).unwrap_or(1);
                    self.hyperlink_uris.insert(id, uri.to_string());
                    self.current_hyperlink_id = id;
                }
            }
            // OSC 9 — iTerm2 notification
            // Format: OSC 9 ; <message> BEL
            // Note: OSC 9 ; 4 ; <state> ; <percent> is iTerm2 progress reporting (unrelated).
            b"9" => {
                const OSC_NOTIFY_MAX: usize = 4096;
                // Skip the progress-reporting sub-form: OSC 9 ; 4 ; ...
                let is_progress_subform = params.get(1).is_some_and(|p| p == b"4") && params.len() > 2;
                if !is_progress_subform
                    && let Some(msg_slice) = params.get(1)
                    && !msg_slice.is_empty()
                    && msg_slice.len() <= OSC_NOTIFY_MAX
                    && let Ok(message) = std::str::from_utf8(msg_slice)
                {
                    self.pending_notifications.push(TerminalNotification {
                        title: None,
                        body: message.to_string(),
                        urgency: NotificationUrgency::Normal,
                    });
                }
            }
            // OSC 99 — kitty-style structured notification
            // Format: OSC 99 ; <metadata> ; <body> ST
            // <metadata>: colon-separated key=value pairs (i=id, u=urgency, d=dir, etc.)
            // We extract urgency (0=low, 1=normal, 2=critical) and use body verbatim.
            b"99" => {
                const OSC_NOTIFY_MAX: usize = 4096;
                if let Some(body_slice) = params.get(2)
                    && !body_slice.is_empty()
                    && body_slice.len() <= OSC_NOTIFY_MAX
                    && let Ok(body) = std::str::from_utf8(body_slice)
                {
                    let mut urgency = NotificationUrgency::Normal;
                    if let Some(meta_slice) = params.get(1)
                        && let Ok(meta) = std::str::from_utf8(meta_slice)
                    {
                        for kv in meta.split(':') {
                            if let Some(u) = kv.strip_prefix("u=") {
                                urgency = match u {
                                    "0" => NotificationUrgency::Low,
                                    "2" => NotificationUrgency::Critical,
                                    _ => NotificationUrgency::Normal,
                                };
                            }
                        }
                    }
                    self.pending_notifications.push(TerminalNotification {
                        title: None,
                        body: body.to_string(),
                        urgency,
                    });
                }
            }
            // OSC 777 — urxvt / gnome-terminal notification
            // Format: OSC 777 ; notify ; <summary> ; <body> ST
            b"777" => {
                const OSC_NOTIFY_MAX: usize = 4096;
                if let Some(action) = params.get(1)
                    && action == b"notify"
                    && let Some(summary_slice) = params.get(2)
                    && !summary_slice.is_empty()
                    && summary_slice.len() <= OSC_NOTIFY_MAX
                    && let Ok(summary) = std::str::from_utf8(summary_slice)
                {
                    let body = params
                        .get(3)
                        .filter(|b| !b.is_empty() && b.len() <= OSC_NOTIFY_MAX)
                        .and_then(|b| std::str::from_utf8(b).ok())
                        .unwrap_or("");
                    self.pending_notifications.push(TerminalNotification {
                        title: Some(summary.to_string()),
                        body: body.to_string(),
                        urgency: NotificationUrgency::Normal,
                    });
                }
            }
            // OSC 52 — Clipboard
            // Format: OSC 52 ; <selection-chars> ; <base64-payload> ST
            // <selection-chars>: empty or any of c/p/q/s/0-7 (we treat all as system clipboard).
            // `?` as payload is a read request — unsupported for security (clipboard exfiltration).
            b"52" => {
                use base64::Engine;
                const OSC52_MAX_DECODED: usize = 100 * 1024;
                if let Some(payload_slice) = params.get(2) {
                    let payload: &[u8] = payload_slice;
                    if !payload.is_empty()
                        && payload != b"?"
                        && payload.len() <= OSC52_MAX_DECODED * 2
                        && let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(payload)
                        && decoded.len() <= OSC52_MAX_DECODED
                        && let Ok(text) = String::from_utf8(decoded)
                    {
                        self.pending_clipboard_writes.push(text);
                    }
                }
            }
            // OSC 133 — Semantic prompts (shell integration)
            b"133" => {
                // Shell integration: A=prompt start, B=input start, C=output start, D=command done
                if let Some(sub) = params.get(1)
                    && let Ok(s) = std::str::from_utf8(sub) {
                        if s == "A" {
                            // OSC 133;A — prompt start (shell is about to draw the prompt)
                            self.prompt_state = PromptState::Prompt;
                            self.pending_prompt_events
                                .push(crate::log::PromptEvent::PromptStart);
                            // Safety net: reset any lingering inline marker expression.
                            // When the shell prompt appears, AI output is done — clear
                            // any <<express>> that wasn't properly closed.
                            if self.marker_parser.in_express() {
                                self.marker_parser.reset();
                                self.marker_expression = None;
                                self.expression_meta = self.expression.to_meta();
                            }
                            self.dirty = true;
                        } else if s == "B" {
                            // OSC 133;B — input start (prompt drawn, user can type)
                            self.prompt_state = PromptState::Input;
                            self.pending_prompt_events
                                .push(crate::log::PromptEvent::InputStart);
                            self.dirty = true;
                        } else if s == "C" {
                            // OSC 133;C — command output start (user pressed Enter)
                            self.prompt_state = PromptState::Output;
                            self.pending_prompt_events
                                .push(crate::log::PromptEvent::OutputStart);
                            self.dirty = true;
                        } else if let Some(rest) = s.strip_prefix("D;") {
                            // OSC 133;D;<exit_code> — command finished
                            if let Ok(code) = rest.parse::<i32>() {
                                self.last_exit_code = Some(code);
                                self.prompt_state = PromptState::Unknown;
                                self.pending_prompt_events
                                    .push(crate::log::PromptEvent::CommandDone {
                                        exit_code: code,
                                    });
                                self.dirty = true;
                            }
                        } else if s == "D" {
                            // OSC 133;D with no code — command finished, exit 0 assumed
                            self.last_exit_code = Some(0);
                            self.prompt_state = PromptState::Unknown;
                            self.pending_prompt_events
                                .push(crate::log::PromptEvent::CommandDone { exit_code: 0 });
                            self.dirty = true;
                        }
                    }
            }
            // OSC 1337;ImmorTerm — AI stats pushed by Claude Code's statusline.sh.
            // Format: \e]1337;ImmorTerm;sid=<id>;m=<model>;c=<cost>;ctx=<pct>;tp=<path>;pm=<mode>\a
            b"1337" if params.get(1) == Some(&&b"ImmorTerm"[..]) => {
                // Parse all key=value pairs into a HashMap first
                let mut kv = std::collections::HashMap::new();
                for param in &params[2..] {
                    if let Ok(s) = std::str::from_utf8(param)
                        && let Some((k, v)) = s.split_once('=') {
                            kv.insert(k.to_string(), v.to_string());
                        }
                }

                if let Some(evt_type) = kv.remove("evt") {
                    // Generic ImmorTerm event (e.g. evt=share_consumed)
                    self.pending_immorterm_events.push(ImmorTermOscEvent {
                        event_type: evt_type,
                        params: kv,
                    });
                } else if kv.contains_key("sid") {
                    // AI stats event (legacy format: sid=...,m=...,c=...,ctx=...)
                    let event = AiStatsOscEvent {
                        session_id: kv.remove("sid").unwrap_or_default(),
                        model: kv.remove("m").unwrap_or_default(),
                        cost_usd: kv.get("c").and_then(|v| v.parse().ok()).unwrap_or(0.0),
                        context_pct: kv.get("ctx").and_then(|v| v.parse().ok()).unwrap_or(0.0),
                        transcript_path: kv.remove("tp").unwrap_or_default(),
                        permission_mode: kv.remove("pm"),
                    };
                    self.pending_ai_stats_event = Some(event);
                }
            }
            _ => {}
        }
    }

    /// ESC dispatch.
    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (byte, intermediates) {
            // DECSC — Save Cursor (ESC 7)
            (b'7', []) => {
                self.cursor.save(self.modes.origin_mode);
            }
            // DECRC — Restore Cursor (ESC 8)
            (b'8', []) => {
                if let Some(origin) = self.cursor.restore() {
                    self.modes.origin_mode = origin;
                }
            }
            // IND — Index (ESC D)
            (b'D', []) => {
                if self.cursor.row == self.grid.scroll_bottom {
                    self.scroll_up();
                } else if self.cursor.row + 1 < self.rows {
                    self.cursor.row += 1;
                }
            }
            // NEL — Next Line (ESC E)
            (b'E', []) => {
                self.cursor.col = 0;
                if self.cursor.row == self.grid.scroll_bottom {
                    self.scroll_up();
                } else if self.cursor.row + 1 < self.rows {
                    self.cursor.row += 1;
                }
            }
            // HTS — Horizontal Tab Set (ESC H)
            (b'H', []) => {
                self.tabs.set(self.cursor.col);
            }
            // RI — Reverse Index (ESC M)
            (b'M', []) => {
                if self.cursor.row == self.grid.scroll_top {
                    self.scroll_down();
                } else if self.cursor.row > 0 {
                    self.cursor.row -= 1;
                }
            }
            // RIS — Full Reset (ESC c)
            (b'c', []) => {
                let cols = self.cols;
                let rows = self.rows;
                *self = Terminal::new(cols, rows);
            }
            _ => {}
        }
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {
        // DCS hooks — Sixel, etc. Placeholder.
    }

    fn put(&mut self, _byte: u8) {
        // DCS put — receive data. Placeholder.
    }

    fn unhook(&mut self) {
        // DCS unhook — finalize. Placeholder.
    }
}

/// Reflow grid rows from `old_cols` to `new_cols` width.
///
/// Extracts logical lines (sequences of rows connected by `wrapped = true`),
/// then re-wraps each logical line at the new column width. Returns the new
/// rows and updated cursor position.
/// Reflow scrollback rows to a new column width (no cursor tracking).
/// Used when on-demand scrollback rows arrive from the daemon at a different
/// width than the current terminal.
pub fn reflow_scrollback_rows(rows: Vec<Row>, new_cols: usize) -> Vec<Row> {
    if rows.is_empty() {
        return rows;
    }
    let old_cols = rows[0].cells.len();
    if old_cols == new_cols {
        return rows;
    }
    let (reflowed, _, _) = reflow_grid_rows(rows, old_cols, new_cols, 0, 0);
    reflowed
}

fn reflow_grid_rows(
    old_rows: Vec<Row>,
    _old_cols: usize,
    new_cols: usize,
    cursor_row: usize,
    cursor_col: usize,
) -> (Vec<Row>, usize, usize) {
    use crate::cell::Cell;

    // Cap on cells per logical line during reflow. Above this, a wrapped
    // chain is force-broken (`trailing_wrap=true`) so the output preserves
    // the wrap continuation flag without holding a single giant Vec. Protects
    // against pathological cases — e.g. a 1.5MB JSON line wrapped over
    // thousands of rows would otherwise allocate a single huge buffer and
    // block resize for tens of ms. WezTerm uses a similar guard
    // (MAX_LOGICAL_LINE_LEN, screen.rs:1004).
    const MAX_LOGICAL_LINE_CAP: usize = 4096;

    let default_cell = Cell::default();

    // ── Step 1: Extract logical lines ──
    // A logical line is a chain of physical rows where all but the last have
    // `wrapped = true`. We concatenate their cells and record which logical
    // line the cursor belongs to. The bool tracks whether the chain was
    // force-broken (cap hit or trailing wrap at end-of-input) — propagated
    // to the last output row's `wrapped` flag in step 2.
    let mut logical_lines: Vec<(Vec<Cell>, bool)> = Vec::new();
    let mut current_cells: Vec<Cell> = Vec::new();
    let mut cursor_logical_line: usize = 0;
    let mut cursor_char_offset: usize = 0;
    let mut logical_line_idx: usize = 0;

    for (row_idx, row) in old_rows.into_iter().enumerate() {
        // Cap protection: if appending this row would exceed the cap, finalize
        // the current logical line as broken (trailing_wrap=true) so the chain
        // continues correctly into the next output row group.
        if !current_cells.is_empty()
            && current_cells.len() + row.cells.len() > MAX_LOGICAL_LINE_CAP
        {
            logical_lines.push((std::mem::take(&mut current_cells), true));
            logical_line_idx += 1;
        }

        let row_start = current_cells.len();
        current_cells.extend(row.cells);

        if row_idx == cursor_row {
            cursor_logical_line = logical_line_idx;
            cursor_char_offset = row_start + cursor_col;
        }

        if !row.wrapped {
            logical_lines.push((current_cells, false));
            current_cells = Vec::new();
            logical_line_idx += 1;
        }
    }
    // Trailing wrapped rows (input ended mid-chain): preserve the wrap flag.
    if !current_cells.is_empty() {
        logical_lines.push((current_cells, true));
    }

    // ── Step 2: Re-wrap each logical line at new_cols ──
    let mut new_rows: Vec<Row> = Vec::new();
    let mut new_cursor_row: usize = 0;
    let mut new_cursor_col: usize = 0;

    for (line_idx, (cells, trailing_wrap)) in logical_lines.into_iter().enumerate() {
        // Find content width (last non-default cell index + 1)
        let content_len = cells
            .iter()
            .rposition(|c| *c != default_cell)
            .map(|i| i + 1)
            .unwrap_or(0);

        // For cursor tracking: if cursor is on this line beyond content,
        // include cursor position in the effective length
        let effective_len = if line_idx == cursor_logical_line {
            content_len.max(cursor_char_offset + 1)
        } else {
            content_len
        };

        if effective_len == 0 {
            // Empty logical line → single empty row (carry trailing_wrap flag
            // so a force-broken empty doesn't lose its continuation marker).
            if line_idx == cursor_logical_line {
                new_cursor_row = new_rows.len();
                new_cursor_col = 0;
            }
            let mut new_row = Row::new(new_cols);
            new_row.wrapped = trailing_wrap;
            new_rows.push(new_row);
            continue;
        }

        // Number of physical rows needed
        let num_phys = effective_len.div_ceil(new_cols);

        for phys in 0..num_phys {
            let start = phys * new_cols;
            let mut new_row = Row::new(new_cols);

            for j in 0..new_cols {
                let src_idx = start + j;
                if src_idx < cells.len() {
                    new_row.cells[j] = cells[src_idx].clone();
                }
            }

            // Wrap continues if this isn't the last physical row of the chunk,
            // OR if the chunk itself was force-broken (cap hit / trailing wrap
            // at input boundary) — then the last physical row keeps wrapped=true.
            let is_last_phys = phys == num_phys - 1;
            new_row.wrapped = !is_last_phys || trailing_wrap;
            new_row.dirty = true;
            // Recompute content_end_col for the new row
            new_row.content_end_col = new_row
                .cells
                .iter()
                .rposition(|c| c.grapheme != ' ' && c.width > 0)
                .map(|p| p + 1)
                .unwrap_or(0);

            // Track cursor
            if line_idx == cursor_logical_line
                && cursor_char_offset >= start
                && cursor_char_offset < start + new_cols
            {
                new_cursor_row = new_rows.len();
                new_cursor_col = cursor_char_offset - start;
            }

            new_rows.push(new_row);
        }
    }

    (new_rows, new_cursor_row, new_cursor_col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term() -> Terminal {
        Terminal::new(80, 24)
    }

    /// Terminal with marker parsing enabled (simulates daemon context).
    fn term_with_markers() -> Terminal {
        let mut t = Terminal::new(80, 24);
        t.enable_marker_parsing();
        t
    }

    #[test]
    fn print_character() {
        let mut t = term();
        t.process(b"A");
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, 'A');
        assert_eq!(t.cursor.col, 1);
    }

    #[test]
    fn print_wraps_at_end() {
        let mut t = Terminal::new(5, 3);
        t.process(b"12345");
        // After printing 5 chars in 5-col terminal, cursor is at pending_wrap
        assert!(t.cursor.pending_wrap);
        // Next char wraps
        t.process(b"6");
        assert_eq!(t.cursor.row, 1);
        assert_eq!(t.cursor.col, 1);
        assert_eq!(t.grid.cell(1, 0).unwrap().grapheme, '6');
    }

    #[test]
    fn linefeed_scrolls_at_bottom() {
        let mut t = Terminal::new(80, 3);
        t.cursor.row = 2; // Bottom row
        t.grid.cell_mut(0, 0).unwrap().grapheme = 'A';
        t.process(b"\n"); // LF at bottom triggers scroll
        // Row 'A' should have scrolled into scrollback
        assert_eq!(t.scrollback.len(), 1);
        assert_eq!(t.scrollback.get(0).unwrap().cells[0].grapheme, 'A');
    }

    #[test]
    fn autowrap_sets_wrapped_flag() {
        // 10-column terminal: write 15 chars without any \n
        let mut t = Terminal::new(10, 5);
        t.process(b"ABCDEFGHIJKLMNO"); // 15 chars, should wrap at col 10
        // Row 0 should be wrapped (soft-wrap at col 10)
        assert!(t.grid.row(0).unwrap().wrapped, "row 0 should be wrapped (auto-wrap)");
        // Row 1 should NOT be wrapped (content ends mid-row)
        assert!(!t.grid.row(1).unwrap().wrapped, "row 1 should not be wrapped");
        // Row 0 content: ABCDEFGHIJ, Row 1 content: KLMNO
        assert_eq!(t.grid.cell(0, 9).unwrap().grapheme, 'J');
        assert_eq!(t.grid.cell(1, 0).unwrap().grapheme, 'K');
    }

    #[test]
    fn autowrap_then_newline_clears_wrapped() {
        // If text fills exactly to width and then \n arrives, wrapped should be FALSE
        let mut t = Terminal::new(10, 5);
        t.process(b"ABCDEFGHIJ\n"); // exactly 10 chars + newline
        // Row 0 should NOT be wrapped — the \n is a hard break
        assert!(!t.grid.row(0).unwrap().wrapped, "row 0 should NOT be wrapped (hard \\n)");
    }

    #[test]
    fn autowrap_text_then_more_text_then_newline() {
        // Text wraps, continues on next row, then \n
        let mut t = Terminal::new(10, 5);
        t.process(b"ABCDEFGHIJKLMnop\n"); // 16 chars + newline
        // Row 0: wrapped (auto-wrap at col 10)
        assert!(t.grid.row(0).unwrap().wrapped, "row 0 should be wrapped");
        // Row 1: NOT wrapped (hard \n after)
        assert!(!t.grid.row(1).unwrap().wrapped, "row 1 should NOT be wrapped");
    }

    /// Replicate the exact extraction logic from WasmTerminal::get_selected_text
    /// to verify that `!row.wrapped` correctly suppresses newlines.
    #[test]
    fn selection_extract_respects_wrapped_flag() {
        let mut t = Terminal::new(10, 5);
        // "ABCDEFGHIJKLMNO" → 15 chars, wraps at col 10:
        //   Row 0: ABCDEFGHIJ (wrapped=true)
        //   Row 1: KLMNO      (wrapped=false)
        t.process(b"ABCDEFGHIJKLMNO");

        // Simulate selecting all text: rows 0..1, cols 0..9 on first, 0..4 on last
        let sb_len = t.scrollback.len(); // should be 0
        let sr = sb_len; // start row (content idx)
        let er = sb_len + 1; // end row (content idx)
        let sc = 0usize;
        let ec = 4usize; // 'O' is at col 4

        let mut text = String::new();
        for content_idx in sr..=er {
            let row = if content_idx < sb_len {
                t.scrollback.get(content_idx)
            } else {
                t.grid.row(content_idx - sb_len)
            };
            if let Some(row) = row {
                let start_col = if content_idx == sr { sc } else { 0 };
                let end_col = if content_idx == er {
                    ec.min(row.cells.len().saturating_sub(1))
                } else {
                    row.cells.len().saturating_sub(1)
                };
                for col in start_col..=end_col {
                    if let Some(cell) = row.cells.get(col) {
                        if cell.width > 0 {
                            text.push(cell.grapheme);
                        }
                    }
                }
                // Exact same logic as lib.rs get_selected_text
                if content_idx < er && !row.wrapped {
                    text.push('\n');
                }
            }
        }

        // Soft-wrapped line should NOT have a newline inserted
        assert_eq!(text, "ABCDEFGHIJKLMNO", "wrapped rows should not get \\n inserted");
    }

    /// Hard newline (\r\n) should produce \n in extracted text.
    #[test]
    fn selection_extract_inserts_newline_for_hard_break() {
        let mut t = Terminal::new(10, 5);
        // \r\n is what terminals actually send (CR+LF)
        t.process(b"HELLO\r\nWORLD");

        let sb_len = t.scrollback.len();
        let sr = sb_len;
        let er = sb_len + 1;
        let sc = 0usize;
        let ec = 4usize;

        let mut text = String::new();
        for content_idx in sr..=er {
            let row = if content_idx < sb_len {
                t.scrollback.get(content_idx)
            } else {
                t.grid.row(content_idx - sb_len)
            };
            if let Some(row) = row {
                let start_col = if content_idx == sr { sc } else { 0 };
                let end_col = if content_idx == er {
                    ec.min(row.cells.len().saturating_sub(1))
                } else {
                    row.cells.len().saturating_sub(1)
                };
                for col in start_col..=end_col {
                    if let Some(cell) = row.cells.get(col) {
                        if cell.width > 0 && cell.grapheme != ' ' {
                            text.push(cell.grapheme);
                        }
                    }
                }
                if content_idx < er && !row.wrapped {
                    text.push('\n');
                }
            }
        }

        // Row 0: "HELLO" (hard break, wrapped=false) → extracts "HELLO\n"
        // Row 1: "WORLD" → extracts "WORLD"
        assert!(text.contains('\n'), "hard break should produce \\n");
        assert!(!t.grid.row(0).unwrap().wrapped, "row 0 should NOT be wrapped (hard break)");
    }

    /// KEY DIAGNOSTIC: When an app sends \r\n at the exact column boundary
    /// (like Claude Code / Ink does), the terminal sees it as a hard break —
    /// NOT an auto-wrap. This is the root cause of the copy issue.
    #[test]
    fn app_wrapped_vs_auto_wrapped() {
        // Case 1: App pre-wraps with \r\n at column boundary
        let mut t1 = Terminal::new(10, 5);
        t1.process(b"ABCDEFGHIJ\r\nKLMNO"); // App sends 10 chars + \r\n
        assert!(!t1.grid.row(0).unwrap().wrapped,
            "app-wrapped row should NOT have wrapped=true (it used \\r\\n)");

        // Case 2: True auto-wrap (no \r\n, cursor hits edge)
        let mut t2 = Terminal::new(10, 5);
        t2.process(b"ABCDEFGHIJKLMNO"); // 15 chars, terminal auto-wraps at col 10
        assert!(t2.grid.row(0).unwrap().wrapped,
            "auto-wrapped row SHOULD have wrapped=true");

        // Both produce identical visual output, but wrapped flag differs!
        // This is why the `!row.wrapped` check doesn't help for app-wrapped text.
    }

    /// App-wrapped text (CR+LF near line edge) sets soft_wrapped=true.
    #[test]
    fn soft_wrapped_detected_on_app_wrap() {
        let mut t = Terminal::new(10, 5);
        // 10 chars fills the row (col 0-9), then \r\n = app wrap at boundary
        t.process(b"ABCDEFGHIJ\r\nKLMNO");
        let row0 = t.grid.row(0).unwrap();
        assert!(!row0.wrapped, "should NOT be auto-wrapped");
        assert!(row0.soft_wrapped, "should be soft_wrapped (CR at col 10 pending_wrap, then LF)");
    }

    /// CR+LF early in the line (before 75% threshold) is a hard break.
    #[test]
    fn hard_break_not_soft_wrapped() {
        let mut t = Terminal::new(10, 5);
        // Only 5 chars (50% of width), then \r\n — this is a hard newline
        t.process(b"ABCDE\r\nFGHIJ");
        let row0 = t.grid.row(0).unwrap();
        assert!(!row0.wrapped);
        assert!(!row0.soft_wrapped, "short line CR+LF should NOT be soft_wrapped");
    }

    /// Printable char between CR and LF clears cr_near_edge.
    #[test]
    fn print_between_cr_lf_clears_soft_wrap() {
        let mut t = Terminal::new(10, 5);
        // Fill to near-edge, CR, then a printable char, then LF
        t.process(b"ABCDEFGH\rX\n");
        let row0 = t.grid.row(0).unwrap();
        assert!(!row0.soft_wrapped, "print between CR and LF should prevent soft_wrapped");
    }

    /// CR at exactly 75% threshold still triggers soft_wrapped.
    #[test]
    fn soft_wrapped_at_threshold() {
        let mut t = Terminal::new(20, 5); // 75% = col 15
        // Write 15 chars (col 0-14 → cursor at col 15 = 75% threshold)
        t.process(b"ABCDEFGHIJKLMNO\r\n");
        let row0 = t.grid.row(0).unwrap();
        assert!(row0.soft_wrapped, "CR at 75% threshold should trigger soft_wrapped");
    }

    /// CR + CSI CUD(1) (Ink/React CLI pattern) sets soft_wrapped.
    #[test]
    fn soft_wrapped_via_csi_cud() {
        let mut t = Terminal::new(10, 5);
        // 10 chars fills the row, then \r + CSI B (cursor down 1) — Ink pattern
        t.process(b"ABCDEFGHIJ\r\x1b[1BKLMNO");
        let row0 = t.grid.row(0).unwrap();
        assert!(!row0.wrapped, "should NOT be auto-wrapped");
        assert!(row0.soft_wrapped, "CR near edge + CUD(1) should set soft_wrapped");
    }

    /// CR + CUF (indent) + CUD(1) pattern — Ink with indentation.
    #[test]
    fn soft_wrapped_via_cr_cuf_cud() {
        let mut t = Terminal::new(10, 5);
        // 10 chars, then CR + CUF(3) indent + CUD(1) — Ink indent pattern
        t.process(b"ABCDEFGHIJ\r\x1b[3C\x1b[1BKLMNO");
        let row0 = t.grid.row(0).unwrap();
        assert!(row0.soft_wrapped, "CR + CUF + CUD(1) should set soft_wrapped");
    }

    /// Double CR + LF (\r\r\n) after near-edge line is still soft wrap.
    /// Ink sends \r\r\n where the second \r is a no-op (cursor already at col 0).
    #[test]
    fn double_cr_lf_near_edge_is_soft_wrap() {
        let mut t = Terminal::new(10, 5);
        t.process(b"ABCDEFGHIJ\r\r\n");
        let row0 = t.grid.row(0).unwrap();
        assert!(row0.soft_wrapped, "\\r\\r\\n after near-edge should be soft_wrapped (second \\r is no-op)");
    }

    /// Double CR + LF after a short line is still a hard break.
    #[test]
    fn double_cr_lf_short_line_is_hard_break() {
        let mut t = Terminal::new(10, 5);
        t.process(b"ABC\r\r\n");
        let row0 = t.grid.row(0).unwrap();
        assert!(!row0.soft_wrapped, "short line \\r\\r\\n should NOT be soft_wrapped");
    }

    /// Test extraction when wrapped rows have scrolled into scrollback.
    #[test]
    fn selection_extract_wrapped_in_scrollback() {
        let mut t = Terminal::new(10, 3); // only 3 visible rows
        // Write enough to push rows into scrollback:
        // 30 chars = 3 wrapped rows, then a newline, then more text
        t.process(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcd\nNEWLINE");
        // Row layout:
        //   scrollback[0]: ABCDEFGHIJ (wrapped=true)
        //   scrollback[n]: ... more wrapped rows pushed by scroll
        //   grid: last visible rows

        // Just check that scrollback rows preserve wrapped flag
        let sb_len = t.scrollback.len();
        assert!(sb_len > 0, "should have scrollback");

        // First scrollback row should be wrapped (it was auto-wrapped)
        let first_sb = t.scrollback.get(0).unwrap();
        assert!(first_sb.wrapped, "first scrollback row should be wrapped (auto-wrap)");
    }

    #[test]
    fn carriage_return() {
        let mut t = term();
        t.cursor.col = 40;
        t.process(b"\r");
        assert_eq!(t.cursor.col, 0);
    }

    #[test]
    fn backspace() {
        let mut t = term();
        t.process(b"AB\x08");
        assert_eq!(t.cursor.col, 1);
    }

    #[test]
    fn tab_stops() {
        let mut t = term();
        t.process(b"\t");
        assert_eq!(t.cursor.col, 8);
        t.process(b"\t");
        assert_eq!(t.cursor.col, 16);
    }

    #[test]
    fn cursor_position_csi() {
        let mut t = term();
        // CSI 5;10H — move cursor to row 5, col 10
        t.process(b"\x1b[5;10H");
        assert_eq!(t.cursor.row, 4); // 0-indexed
        assert_eq!(t.cursor.col, 9);
    }

    #[test]
    fn erase_display_below() {
        let mut t = Terminal::new(10, 3);
        for r in 0..3 {
            for c in 0..10 {
                t.grid.cell_mut(r, c).unwrap().grapheme = 'X';
            }
        }
        t.cursor.row = 1;
        t.cursor.col = 5;
        // CSI 0J — erase below
        t.process(b"\x1b[J");
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, 'X');
        assert_eq!(t.grid.cell(1, 4).unwrap().grapheme, 'X');
        assert_eq!(t.grid.cell(1, 5).unwrap().grapheme, ' ');
        assert_eq!(t.grid.cell(2, 0).unwrap().grapheme, ' ');
    }

    #[test]
    fn sgr_bold_and_color() {
        let mut t = term();
        // ESC[1;31m — bold + red foreground
        t.process(b"\x1b[1;31m");
        assert!(t.cursor.attrs.contains(CellAttrs::BOLD));
        assert_eq!(t.cursor.fg, Color::Indexed(1));
        // ESC[0m — reset
        t.process(b"\x1b[0m");
        assert!(t.cursor.attrs.is_empty());
        assert_eq!(t.cursor.fg, Color::Default);
    }

    #[test]
    fn sgr_true_color() {
        let mut t = term();
        // ESC[38;2;255;128;0m — true color foreground
        t.process(b"\x1b[38;2;255;128;0m");
        assert_eq!(t.cursor.fg, Color::Rgb(255, 128, 0));
    }

    #[test]
    fn scroll_region() {
        let mut t = Terminal::new(80, 10);
        // CSI 3;7r — scroll region rows 3-7
        t.process(b"\x1b[3;7r");
        assert_eq!(t.grid.scroll_top, 2); // 0-indexed
        assert_eq!(t.grid.scroll_bottom, 6);
    }

    #[test]
    fn osc_title() {
        let mut t = term();
        // OSC 0 ; Hello World BEL
        t.process(b"\x1b]0;Hello World\x07");
        assert_eq!(t.title, "Hello World");
    }

    #[test]
    fn alternate_screen() {
        let mut t = term();
        t.grid.cell_mut(0, 0).unwrap().grapheme = 'P'; // Primary content
        // DECSET 1049 — switch to alt screen
        t.process(b"\x1b[?1049h");
        assert!(t.modes.alternate_screen);
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, ' '); // Alt screen is blank
        t.grid.cell_mut(0, 0).unwrap().grapheme = 'A'; // Alt content
        // DECRST 1049 — switch back to primary
        t.process(b"\x1b[?1049l");
        assert!(!t.modes.alternate_screen);
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, 'P'); // Primary preserved
    }

    #[test]
    fn save_restore_cursor() {
        let mut t = term();
        t.cursor.col = 10;
        t.cursor.row = 5;
        t.cursor.fg = Color::Indexed(1);
        // ESC 7 — save cursor
        t.process(b"\x1b7");
        t.cursor.col = 0;
        t.cursor.row = 0;
        t.cursor.fg = Color::Default;
        // ESC 8 — restore cursor
        t.process(b"\x1b8");
        assert_eq!(t.cursor.col, 10);
        assert_eq!(t.cursor.row, 5);
        assert_eq!(t.cursor.fg, Color::Indexed(1));
    }

    #[test]
    fn insert_delete_chars() {
        let mut t = Terminal::new(10, 1);
        t.process(b"ABCDE");
        t.cursor.col = 2;
        // CSI 2@ — insert 2 blanks at col 2
        t.process(b"\x1b[2@");
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, 'A');
        assert_eq!(t.grid.cell(0, 1).unwrap().grapheme, 'B');
        assert_eq!(t.grid.cell(0, 2).unwrap().grapheme, ' ');
        assert_eq!(t.grid.cell(0, 3).unwrap().grapheme, ' ');
        assert_eq!(t.grid.cell(0, 4).unwrap().grapheme, 'C');
    }

    #[test]
    fn full_reset() {
        let mut t = term();
        t.process(b"Hello");
        t.cursor.attrs = CellAttrs::BOLD;
        // ESC c — full reset
        t.process(b"\x1bc");
        assert_eq!(t.cursor.col, 0);
        assert_eq!(t.cursor.row, 0);
        assert!(t.cursor.attrs.is_empty());
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, ' ');
    }

    #[test]
    fn kitty_graphics_apc_interception() {
        use base64::Engine;
        let mut t = term();
        // Position cursor at row 5, col 10
        t.cursor.row = 5;
        t.cursor.col = 10;

        // Send a Kitty graphics APC with a 1x1 RGBA image
        let rgba = [255u8, 0, 0, 255]; // Red pixel
        let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
        let apc = format!("\x1b_Ga=T,f=32,s=1,v=1,i=42;{}\x1b\\", b64);
        t.process(apc.as_bytes());

        // The image should be in graphics state
        let img = t.graphics.get(42).expect("image should be stored");
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 1);
        // Row is absolute: scrollback.len() (0) + cursor.row (5) = 5
        assert_eq!(img.row, 5);
        assert_eq!(img.col, 10);
        assert_eq!(img.data, rgba);
    }

    #[test]
    fn kitty_graphics_split_across_process_calls() {
        use base64::Engine;
        let mut t = term();

        // The APC might be split across multiple process() calls
        let rgba = [0u8, 255, 0, 255]; // Green pixel
        let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
        let apc = format!("\x1b_Ga=T,f=32,s=1,v=1,i=99;{}\x1b\\", b64);
        let bytes = apc.as_bytes();

        // Split in the middle of the payload
        let mid = bytes.len() / 2;
        t.process(&bytes[..mid]);
        assert!(t.graphics.get(99).is_none()); // Not complete yet
        t.process(&bytes[mid..]);
        assert!(t.graphics.get(99).is_some()); // Now complete
    }

    #[test]
    fn non_kitty_apc_passes_through() {
        let mut t = term();
        // ESC _ X (non-G APC) should not crash or interfere
        t.process(b"\x1b_Xsome data\x1b\\");
        // Normal text should still work after
        t.process(b"OK");
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, 'O');
        assert_eq!(t.grid.cell(0, 1).unwrap().grapheme, 'K');
    }

    #[test]
    fn inline_express_marker_stamps_cells() {
        let mut t = term_with_markers();
        // Text before marker has no expression
        t.process(b"plain");
        assert!(t.grid.cell(0, 0).unwrap().expression.is_none());

        // Text inside marker gets expression metadata stamped
        t.process(b"<<express mood=creative>>styled<</express>>");
        // "styled" starts at col 5 (after "plain")
        let cell = t.grid.cell(0, 5).unwrap();
        assert_eq!(
            cell.expression.mood(),
            crate::expression::Mood::Creative
        );
        // Marker bytes should NOT appear as visible text
        // "plain" = 5 chars, "styled" = 6 chars, total visible = 11
        assert_eq!(t.grid.cell(0, 11).unwrap().grapheme, ' '); // blank after

        // Text after closing marker reverts to no expression
        t.process(b"after");
        let cell = t.grid.cell(0, 11).unwrap();
        assert!(cell.expression.is_none());
    }

    #[test]
    fn inline_express_marker_stripped_from_output() {
        let mut t = Terminal::new(40, 3);
        t.enable_marker_parsing();
        t.process(b"<<express mood=success>>hello<</express>> world");
        // Only "hello world" should be visible (markers stripped)
        let mut text = String::new();
        for col in 0..11 {
            text.push(t.grid.cell(0, col).unwrap().grapheme);
        }
        assert_eq!(text, "hello world");
    }

    #[test]
    fn inline_express_reset_marker() {
        let mut t = term_with_markers();
        // Set global expression via set_expression (simulating MCP tool)
        let mut state = crate::expression::ExpressionState::new();
        state.mood = crate::expression::Mood::Error;
        t.set_expression(state);
        assert_eq!(t.expression_meta.mood(), crate::expression::Mood::Error);

        // <<express reset>> should clear everything
        t.process(b"<<express reset>>");
        assert!(t.expression_meta.is_none());
        assert!(t.expression.is_default());
    }

    #[test]
    #[ignore = "im-html in-prose path disabled — use MCP draw_html"]
    fn inline_html_block_collected() {
        let mut t = term_with_markers();
        // Opener and closer must be at start-of-line. \r\n mirrors what
        // a TUI emits (LF alone doesn't carriage-return the cursor).
        t.process(b"before\r\n<<im-html>>\n<div>hi</div>\n<</im-html>>\r\nafter");
        let blocks = t.drain_html_blocks();
        assert_eq!(blocks.len(), 1, "got {:?}", blocks);
        assert_eq!(blocks[0].content, "<div>hi</div>\n");
        // "before" lands on row 0, "after" on row 1 col 0 after \r\n.
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, 'b');
        assert_eq!(t.grid.cell(1, 0).unwrap().grapheme, 'a');
    }

    #[test]
    fn inline_marker_split_across_process_calls() {
        // Bytes arrive in chunks — markers may span multiple process() calls.
        let mut t = term_with_markers();
        t.process(b"<<express mood=cre");
        t.process(b"ative>>styled<</ex");
        t.process(b"press>>normal");
        // "styled" at col 0 should have Creative mood
        let cell = t.grid.cell(0, 0).unwrap();
        assert_eq!(cell.expression.mood(), crate::expression::Mood::Creative);
        assert_eq!(cell.grapheme, 's');
        // "normal" should have no expression
        let cell = t.grid.cell(0, 6).unwrap();
        assert!(cell.expression.is_none());
        assert_eq!(cell.grapheme, 'n');
    }

    #[test]
    fn inline_marker_with_ansi_escapes() {
        // Markers interleave with ANSI color codes (common in AI output)
        let mut t = term_with_markers();
        // Bold red text, then marker, then more text
        t.process(b"\x1b[1;31mred ");
        t.process(b"<<express mood=creative>>styled<</express>>");
        t.process(b" more");
        // "red " = 4 chars, "styled" = 6 chars, " more" = 5 chars
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, 'r');
        assert_eq!(
            t.grid.cell(0, 4).unwrap().expression.mood(),
            crate::expression::Mood::Creative
        );
        // Text after close should be NONE expression (but still bold/red from ANSI)
        assert!(t.grid.cell(0, 10).unwrap().expression.is_none());
    }

    #[test]
    fn inline_marker_preserves_mcp_global_expression() {
        let mut t = term_with_markers();
        // Set global expression (simulating MCP tool call)
        let mut state = crate::expression::ExpressionState::new();
        state.mood = crate::expression::Mood::Confident;
        t.set_expression(state);

        // Text before marker should have global expression
        t.process(b"global ");
        assert_eq!(
            t.grid.cell(0, 0).unwrap().expression.mood(),
            crate::expression::Mood::Confident
        );

        // Marker overrides temporarily
        t.process(b"<<express mood=error>>err<</express>>");
        assert_eq!(
            t.grid.cell(0, 7).unwrap().expression.mood(),
            crate::expression::Mood::Error
        );

        // After close, should RESTORE global (Confident), not reset to NONE
        t.process(b"back");
        assert_eq!(
            t.grid.cell(0, 10).unwrap().expression.mood(),
            crate::expression::Mood::Confident
        );
    }

    #[test]
    fn inline_marker_osc133_safety_reset() {
        let mut t = term_with_markers();
        // Open express but DON'T close it (simulating interrupted AI output)
        t.process(b"<<express mood=creative>>unclosed text");
        assert_eq!(
            t.grid.cell(0, 0).unwrap().expression.mood(),
            crate::expression::Mood::Creative
        );

        // OSC 133;A (prompt start) should force-reset the marker
        // OSC 133;A = ESC ] 133 ; A ESC \  (or BEL terminated)
        t.process(b"\x1b]133;A\x07");

        // Text after prompt should have no expression
        t.process(b"prompt");
        assert!(t.grid.cell(1, 0).unwrap().expression.is_none()
            || t.grid.cell(0, 14).unwrap().expression.is_none());
    }

    #[test]
    fn inline_marker_auto_close_at_char_limit() {
        let mut t = Terminal::new(200, 10);
        t.enable_marker_parsing();
        // Open express, then exceed the auto-close limit
        t.process(b"<<express mood=error>>");
        let long_text = "x".repeat(501);
        t.process(long_text.as_bytes());
        // After 500 chars, should auto-close — char 501+ should be NONE
        // The auto-close happens at character 500, so characters at indices >= 500
        // should have NONE expression (the grid wraps at col 200)
        // Row 2, col 100 = character index 500 (200*2 + 100)
        let row = 500 / 200;
        let col = 500 % 200;
        let cell = t.grid.cell(row, col).unwrap();
        assert!(
            cell.expression.is_none(),
            "Expected NONE after auto-close at char 500, got {:?}",
            cell.expression
        );
    }

    #[test]
    #[ignore = "im-html in-prose path disabled — use MCP draw_html"]
    fn multiple_html_blocks_collected_in_order() {
        let mut t = term_with_markers();
        t.process(b"<<im-html>>\n<h1>Title</h1>\n<</im-html>>\ntext\n<<im-html>>\n<p>Body</p>\n<</im-html>>\n");
        let blocks = t.drain_html_blocks();
        assert_eq!(blocks.len(), 2, "got {:?}", blocks);
        assert_eq!(blocks[0].content, "<h1>Title</h1>\n");
        assert_eq!(blocks[1].content, "<p>Body</p>\n");
    }

    #[test]
    #[ignore = "im-html in-prose path disabled — use MCP draw_html"]
    fn html_block_anchor_row_tracked() {
        let mut t = Terminal::new(80, 24);
        t.enable_marker_parsing();
        // Put some text first to move the cursor
        t.process(b"line1\nline2\n");
        t.process(b"<<im-html>>\n<div>anchored</div>\n<</im-html>>\n");
        let blocks = t.drain_html_blocks();
        assert_eq!(blocks.len(), 1);
        // Anchor row should be row 2 (after two newlines)
        assert_eq!(blocks[0].anchor_row, 2);
    }

    #[test]
    #[ignore = "im-html in-prose path disabled — use MCP draw_html"]
    fn drain_html_blocks_clears_queue() {
        let mut t = term_with_markers();
        t.process(b"<<im-html>>\nfirst\n<</im-html>>\n");
        assert_eq!(t.drain_html_blocks().len(), 1);
        // Second drain should be empty
        assert_eq!(t.drain_html_blocks().len(), 0);
        // New block
        t.process(b"<<im-html>>\nsecond\n<</im-html>>\n");
        assert_eq!(t.drain_html_blocks().len(), 1);
    }

    #[test]
    #[ignore = "im-html in-prose path disabled — use MCP draw_html"]
    fn html_block_attrs_propagated_from_start_tag() {
        let mut t = term_with_markers();
        t.process(b"<<im-html anchor=fixed name=my-chart>>\n<canvas></canvas>\n<</im-html>>\n");
        let blocks = t.drain_html_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].content, "<canvas></canvas>\n");
        assert_eq!(blocks[0].attrs.get("anchor").map(|s| s.as_str()), Some("fixed"));
        assert_eq!(blocks[0].attrs.get("name").map(|s| s.as_str()), Some("my-chart"));
    }

    #[test]
    #[ignore = "im-html in-prose path disabled — use MCP draw_html"]
    fn html_block_scrollback_at_creation_tracked() {
        let mut t = Terminal::new(80, 5);
        t.enable_marker_parsing();
        // Fill scrollback by writing more lines than the terminal height
        for i in 0..10 {
            t.process(format!("line{}\n", i).as_bytes());
        }
        let sb_before = t.scrollback.len();
        assert!(sb_before > 0, "scrollback should have lines");
        t.process(b"<<im-html>>\n<div>deep</div>\n<</im-html>>\n");
        let blocks = t.drain_html_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].scrollback_at_creation, sb_before);
        assert_eq!(blocks[0].anchor_row, sb_before + t.cursor.row);
    }

    #[test]
    #[ignore = "im-html in-prose path disabled — use MCP draw_html"]
    fn html_block_attrs_default_to_empty_without_attrs() {
        let mut t = term_with_markers();
        t.process(b"<<im-html>>\n<p>plain</p>\n<</im-html>>\n");
        let blocks = t.drain_html_blocks();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].attrs.is_empty());
    }

    #[test]
    #[ignore = "im-html in-prose path disabled — use MCP draw_html"]
    fn html_block_attrs_isolated_between_blocks() {
        // Attrs from one block should not leak into the next
        let mut t = term_with_markers();
        t.process(b"<<im-html anchor=fixed>>\n<div>1</div>\n<</im-html>>\n<<im-html>>\n<div>2</div>\n<</im-html>>\n");
        let blocks = t.drain_html_blocks();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].attrs.get("anchor").map(|s| s.as_str()), Some("fixed"));
        assert!(blocks[1].attrs.is_empty(), "second block should not inherit first block's attrs");
    }

    // ── Reflow tests ──

    #[test]
    fn resize_shrink_cols_preserves_content() {
        // Scenario: 10-char line at 10 cols shrinks to 5 cols → wraps to 2 rows
        let mut t = Terminal::new(10, 5);
        t.process(b"ABCDEFGHIJ");
        // After writing 10 chars at 10 cols, cursor is at col 9 with pending_wrap
        assert_eq!(t.cursor.row, 0);
        assert_eq!(t.cursor.col, 9);

        t.resize(5, 5);

        // First row should have "ABCDE" and be marked as wrapped
        for (i, ch) in "ABCDE".chars().enumerate() {
            assert_eq!(t.grid.cell(0, i).unwrap().grapheme, ch, "row 0 col {i}");
        }
        assert!(t.grid.row(0).unwrap().wrapped, "row 0 should be wrapped");

        // Second row should have "FGHIJ"
        for (i, ch) in "FGHIJ".chars().enumerate() {
            assert_eq!(t.grid.cell(1, i).unwrap().grapheme, ch, "row 1 col {i}");
        }
        assert!(!t.grid.row(1).unwrap().wrapped, "row 1 should NOT be wrapped");
    }

    #[test]
    fn resize_grow_cols_merges_wrapped_rows() {
        // Scenario: shrink then grow back — content should merge into 1 row
        let mut t = Terminal::new(10, 5);
        t.process(b"ABCDEFGHIJ");

        // Shrink to 5 → wraps to 2 rows
        t.resize(5, 5);
        assert!(t.grid.row(0).unwrap().wrapped, "first row should be wrapped after shrink");

        // Grow back to 10 → should merge back to 1 row
        t.resize(10, 5);
        for (i, ch) in "ABCDEFGHIJ".chars().enumerate() {
            assert_eq!(t.grid.cell(0, i).unwrap().grapheme, ch, "col {i}");
        }
        assert!(!t.grid.row(0).unwrap().wrapped, "merged row should NOT be wrapped");
    }

    #[test]
    fn resize_grow_after_autowrap_merges() {
        // User's exact scenario: process text that auto-wraps, then grow wider
        let mut t = Terminal::new(10, 5);
        // "ls -la" style: long line auto-wraps at col 10
        t.process(b"ABCDEFGHIJKLMNO"); // 15 chars → wraps at col 10
        assert!(t.grid.row(0).unwrap().wrapped, "auto-wrapped row should have wrapped=true");
        assert_eq!(t.grid.cell(1, 0).unwrap().grapheme, 'K');

        // Grow to 20 cols → should merge back into 1 row
        t.resize(20, 5);
        assert!(!t.grid.row(0).unwrap().wrapped, "merged row should not be wrapped");
        for (i, ch) in "ABCDEFGHIJKLMNO".chars().enumerate() {
            assert_eq!(t.grid.cell(0, i).unwrap().grapheme, ch, "col {i}");
        }
        // Row 1 should now be empty (the old continuation was merged)
        assert!(t.grid.row(1).unwrap().cells.iter().all(|c| c.is_default()),
            "row 1 should be empty after merge");
    }

    #[test]
    fn resize_grow_autowrap_in_scrollback_reflows() {
        // Phase 1 reflow: scrollback IS reflowed on column change. A wrapped
        // chain that scrolled into scrollback at narrow width should merge
        // back into a single row when the terminal grows wide enough.
        let mut t = Terminal::new(10, 3);
        // 15 chars at 10 cols → wraps into 2 physical rows (first with wrapped=true).
        t.process(b"ABCDEFGHIJKLMNO\r\n");
        t.process(b"LINE2\r\n");
        t.process(b"LINE3\r\n");
        t.process(b"LINE4");

        let sb_len_before = t.scrollback.len();
        assert!(sb_len_before > 0, "should have scrollback");

        // Sanity: at 10 cols at least one scrollback row should be wrapped
        // (the first half of "ABCDEFGHIJKLMNO").
        let any_wrapped_before =
            (0..sb_len_before).any(|i| t.scrollback.get(i).unwrap().wrapped);
        assert!(
            any_wrapped_before,
            "expected a wrapped scrollback row at 10 cols before resize"
        );

        // Grow to 20 cols — wide enough to merge the 15-char wrapped chain.
        t.resize(20, 3);

        // After reflow, no scrollback row should still be soft-wrapped: the
        // 15-char chain fits in a single 20-col row.
        let sb_len_after = t.scrollback.len();
        let any_wrapped_after =
            (0..sb_len_after).any(|i| t.scrollback.get(i).unwrap().wrapped);
        assert!(
            !any_wrapped_after,
            "scrollback wrapped chains should be merged after grow reflow"
        );

        // Find ABCDEFGHIJKLMNO in scrollback or grid (it may sit at the
        // scrollback/grid boundary depending on cursor placement).
        let mut found_full_row = false;
        for i in 0..sb_len_after {
            let row = t.scrollback.get(i).unwrap();
            let s: String = row.cells.iter().take(15).map(|c| c.grapheme).collect();
            if s == "ABCDEFGHIJKLMNO" {
                found_full_row = true;
                break;
            }
        }
        if !found_full_row {
            for r in 0..t.rows {
                let row = t.grid.row(r).unwrap();
                let s: String = row.cells.iter().take(15).map(|c| c.grapheme).collect();
                if s == "ABCDEFGHIJKLMNO" {
                    found_full_row = true;
                    break;
                }
            }
        }
        assert!(
            found_full_row,
            "expected ABCDEFGHIJKLMNO to live on a single row after grow reflow"
        );
    }

    #[test]
    fn resize_shrink_reflows_scrollback_into_more_rows() {
        // Inverse direction: shrinking cols should split scrollback rows that
        // exceed the new width into wrapped chains.
        let mut t = Terminal::new(20, 3);
        t.process(b"ABCDEFGHIJKLMNOPQRST\r\n"); // exactly 20 chars, fills row at 20 cols
        t.process(b"X1\r\nX2\r\nX3\r\nX4"); // push the 20-char line into scrollback

        // Find the long line
        let mut long_idx = None;
        for i in 0..t.scrollback.len() {
            let row = t.scrollback.get(i).unwrap();
            let s: String = row.cells.iter().take(20).map(|c| c.grapheme).collect();
            if s == "ABCDEFGHIJKLMNOPQRST" {
                long_idx = Some(i);
                break;
            }
        }
        assert!(long_idx.is_some(), "should have long line in scrollback");

        // Shrink to 10 cols — long line must split into two wrapped rows.
        t.resize(10, 3);

        // Find the chain
        let mut found_first_half = None;
        for i in 0..t.scrollback.len() {
            let row = t.scrollback.get(i).unwrap();
            let s: String = row.cells.iter().take(10).map(|c| c.grapheme).collect();
            if s == "ABCDEFGHIJ" {
                found_first_half = Some(i);
                break;
            }
        }
        let i = found_first_half
            .expect("first half of split line should be in scrollback at 10 cols");
        assert!(
            t.scrollback.get(i).unwrap().wrapped,
            "first half of split line should have wrapped=true"
        );
        let next = t.scrollback.get(i + 1).unwrap();
        let s: String = next.cells.iter().take(10).map(|c| c.grapheme).collect();
        assert_eq!(s, "KLMNOPQRST", "second half should follow the first");
    }

    #[test]
    fn resize_reflow_perf_50k_rows_under_budget() {
        // Performance budget: reflowing 50K scrollback rows should complete
        // well under a single 60Hz frame (16ms). Alacritty handles 100K
        // rows in 5–15ms; we should be in the same ballpark.
        let mut t = Terminal::new(100, 30);
        // Push 50,000 rows of varied content into scrollback.
        for i in 0..50_000u32 {
            let mut row = crate::grid::Row::new(100);
            let s = format!("row {i:06} hello world some content");
            for (j, ch) in s.chars().enumerate() {
                if j < 100 {
                    row.cells[j].grapheme = ch;
                    row.cells[j].width = 1;
                }
            }
            t.scrollback.push(row);
        }

        let start = std::time::Instant::now();
        t.resize(80, 30); // shrink — forces reflow over scrollback
        let elapsed = start.elapsed();

        // Budget tiered by build profile:
        //   release: <100ms (real number is typically 30–60ms on M-class macs)
        //   debug:   <500ms (debug is ~5× slower; 302ms observed locally)
        // Smoke-tests that we don't regress catastrophically; for fine-grained
        // benchmarking use `cargo test --release` or `cargo bench`.
        #[cfg(not(debug_assertions))]
        let budget_ms = 100u128;
        // Debug budget needs headroom for parallel test execution alongside
        // concurrent cargo compilation (the build pipeline runs tests while
        // other crates compile) — 500ms flaked 4/6 full builds on 2026-06-06/07
        // despite 302ms when idle. Catastrophic regressions are 5–10×, still
        // caught.
        #[cfg(debug_assertions)]
        let budget_ms = 2_000u128;
        assert!(
            elapsed.as_millis() < budget_ms,
            "reflow of 50K rows took {elapsed:?} (>{budget_ms}ms budget)"
        );
    }

    #[test]
    fn resize_preserves_hard_line_breaks() {
        // Two separate lines should stay separate after shrink+grow.
        // \r\n = CR (col→0) + LF (row↓). Bare \n only moves down, not to col 0.
        let mut t = Terminal::new(10, 5);
        t.process(b"HELLO");
        t.process(b"\r\n"); // CR+LF: return to col 0, then move down
        t.process(b"WORLD");

        // Verify initial state
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, 'H');
        assert_eq!(t.grid.cell(1, 0).unwrap().grapheme, 'W');

        // Shrink then grow — hard breaks should survive
        t.resize(3, 5);
        t.resize(10, 5);

        // Find the two lines (they may have shifted due to scrollback pull)
        let mut hello_row = None;
        let mut world_row = None;
        for r in 0..5 {
            if t.grid.cell(r, 0).map_or(false, |c| c.grapheme == 'H') {
                hello_row = Some(r);
            }
            if t.grid.cell(r, 0).map_or(false, |c| c.grapheme == 'W') {
                world_row = Some(r);
            }
        }
        let hr = hello_row.expect("HELLO row should exist");
        let wr = world_row.expect("WORLD row should exist");

        for (i, ch) in "HELLO".chars().enumerate() {
            assert_eq!(t.grid.cell(hr, i).unwrap().grapheme, ch, "HELLO col {i}");
        }
        for (i, ch) in "WORLD".chars().enumerate() {
            assert_eq!(t.grid.cell(wr, i).unwrap().grapheme, ch, "WORLD col {i}");
        }
        assert!(wr > hr, "WORLD should be on a later row than HELLO");
        assert!(!t.grid.row(hr).unwrap().wrapped, "HELLO row should NOT be wrapped");
    }

    #[test]
    fn resize_excess_rows_go_to_scrollback() {
        // Shrinking cols creates more rows; excess goes to scrollback
        let mut t = Terminal::new(10, 3);
        // Fill all 3 rows with content
        t.process(b"AAAAAAAAAA"); // row 0, 10 chars
        t.process(b"\r\n");
        t.process(b"BBBBBBBBBB"); // row 1, 10 chars
        t.process(b"\r\n");
        t.process(b"CCCCCCCCCC"); // row 2, 10 chars

        // Shrink to 5 cols → each row wraps to 2, total 6 rows needed but grid is 3
        t.resize(5, 3);

        // Excess rows should have gone to scrollback
        assert!(t.scrollback.len() > 0, "excess rows should go to scrollback");
        // Grid should still have exactly 3 rows
        assert_eq!(t.grid.row_count(), 3);
    }

    #[test]
    fn resize_scrollback_pulled_on_grow() {
        let mut t = Terminal::new(10, 3);
        t.process(b"LINE1\r\nLINE2\r\nLINE3");

        // Shrink to 2 rows → pushes 1 row to scrollback
        t.resize(10, 2);
        let sb_after_shrink = t.scrollback.len();
        assert!(sb_after_shrink > 0, "should push to scrollback when shrinking rows");
        assert_eq!(t.grid.row_count(), 2);

        // Grow back to 3 rows → should pull from scrollback
        t.resize(10, 3);
        assert!(t.scrollback.len() < sb_after_shrink, "should pull from scrollback when growing");
        assert_eq!(t.grid.row_count(), 3);
    }

    #[test]
    fn viewport_only_snapshot_size_reduction() {
        use crate::cell::Cell;

        let mut t = term();
        // Fill 5000 scrollback rows
        for i in 0..5000u16 {
            let mut row = crate::grid::Row::new(120);
            row.cells[0] = Cell::with_char((b'A' + (i % 26) as u8) as char);
            t.scrollback.push(row);
        }

        let full = serde_json::to_string(&t.snapshot()).unwrap();
        let viewport = serde_json::to_string(&t.snapshot_viewport_only()).unwrap();

        eprintln!("Full snapshot:     {} bytes ({:.1} KB)", full.len(), full.len() as f64 / 1024.0);
        eprintln!("Viewport-only:     {} bytes ({:.1} KB)", viewport.len(), viewport.len() as f64 / 1024.0);
        eprintln!("Reduction:         {:.0}x", full.len() as f64 / viewport.len() as f64);

        // Viewport-only must round-trip correctly
        let snap: TerminalSnapshot = serde_json::from_str(&viewport).unwrap();
        assert_eq!(snap.scrollback.len(), 0);
        assert_eq!(snap.grid.row_count(), t.grid.row_count());

        // Compact serde: default cell should be tiny (only grapheme field)
        let default_cell = serde_json::to_string(&Cell::default()).unwrap();
        eprintln!("Default cell JSON: {} ({} bytes)", default_cell, default_cell.len());
        assert!(default_cell.len() < 30, "Default cell too large: {}", default_cell);

        // Must be >10x smaller (typically >100x for 5000 rows)
        assert!(full.len() > viewport.len() * 10,
            "viewport should be >10x smaller: full={} viewport={}", full.len(), viewport.len());
    }

    #[test]
    fn snapshot_serializes_with_tuple_keyed_maps() {
        // Regression: serde_json rejects HashMap<(usize,usize), _> as a JSON
        // object ("key must be a string"). expression_colors + combining_marks
        // must serialize (as entry sequences) and round-trip. This is the bug
        // that broke immorterm_screenshot once niqqud populated combining_marks.
        let t = term();
        let mut snap = t.snapshot();
        snap.expression_colors.insert((3, 7), [0.1, 0.2, 0.3, 1.0]);
        snap.combining_marks
            .insert((3, 7), smallvec::smallvec!['\u{05B4}']); // Hebrew hiriq

        let json = serde_json::to_string(&snap)
            .expect("snapshot with tuple-keyed maps must serialize");

        let back: TerminalSnapshot =
            serde_json::from_str(&json).expect("must round-trip");
        assert_eq!(back.expression_colors.get(&(3, 7)), Some(&[0.1, 0.2, 0.3, 1.0]));
        assert_eq!(back.combining_marks.get(&(3, 7)).map(|m| m.len()), Some(1));

        // Empty maps stay omitted (skip_serializing_if) — no format conflict.
        let empty = serde_json::to_string(&t.snapshot()).unwrap();
        assert!(!empty.contains("expression_colors"));
        assert!(!empty.contains("combining_marks"));
    }

    #[test]
    fn synchronized_output_mode_2026() {
        let mut t = term();
        assert!(!t.modes.synchronized_update);

        // DECSET 2026 — enter synchronized output mode
        t.process(b"\x1b[?2026h");
        assert!(t.modes.synchronized_update);
        // Writes during sync still mutate the grid (only the *paint* is deferred)
        t.process(b"X");
        assert_eq!(t.grid.cell(0, 0).unwrap().grapheme, 'X');

        // DECRST 2026 — exit; dirty must be set so consumer flushes accumulated state
        t.dirty = false;
        t.process(b"\x1b[?2026l");
        assert!(!t.modes.synchronized_update);
        assert!(t.dirty, "exiting sync mode must flag dirty to flush accumulated paints");
    }

    #[test]
    fn osc_52_decodes_clipboard_write() {
        let mut t = term();
        // "hello" base64-encoded = aGVsbG8=
        t.process(b"\x1b]52;c;aGVsbG8=\x1b\\");
        let writes = t.drain_clipboard_writes();
        assert_eq!(writes, vec!["hello".to_string()]);
        // Second drain returns nothing
        assert!(t.drain_clipboard_writes().is_empty());
    }

    #[test]
    fn osc_52_ignores_read_request() {
        let mut t = term();
        // `?` as payload is a read request — we intentionally drop for security.
        t.process(b"\x1b]52;c;?\x1b\\");
        assert!(t.drain_clipboard_writes().is_empty());
    }

    #[test]
    fn osc_52_rejects_invalid_base64() {
        let mut t = term();
        t.process(b"\x1b]52;c;not-valid-base64!!!\x1b\\");
        assert!(t.drain_clipboard_writes().is_empty());
    }

    #[test]
    fn osc_52_empty_selection_still_works() {
        let mut t = term();
        // Empty selection field (`OSC 52 ; ; <b64>`) is valid — default to system clipboard.
        t.process(b"\x1b]52;;aGVsbG8=\x1b\\");
        assert_eq!(t.drain_clipboard_writes(), vec!["hello".to_string()]);
    }

    #[test]
    fn osc_9_iterm_notification() {
        let mut t = term();
        t.process(b"\x1b]9;Build complete\x07");
        let notifs = t.drain_notifications();
        assert_eq!(notifs.len(), 1);
        assert_eq!(notifs[0].body, "Build complete");
        assert!(notifs[0].title.is_none());
        assert_eq!(notifs[0].urgency, NotificationUrgency::Normal);
    }

    #[test]
    fn osc_9_progress_sub_form_is_not_a_notification() {
        let mut t = term();
        // OSC 9 ; 4 ; <state> ; <percent> is iTerm2 progress — skip, not a notification.
        t.process(b"\x1b]9;4;1;50\x07");
        assert!(t.drain_notifications().is_empty());
    }

    #[test]
    fn osc_777_notify_with_summary_and_body() {
        let mut t = term();
        t.process(b"\x1b]777;notify;Tests passed;42/42\x1b\\");
        let notifs = t.drain_notifications();
        assert_eq!(notifs.len(), 1);
        assert_eq!(notifs[0].title.as_deref(), Some("Tests passed"));
        assert_eq!(notifs[0].body, "42/42");
    }

    #[test]
    fn osc_99_kitty_urgency_critical() {
        let mut t = term();
        t.process(b"\x1b]99;u=2;Something broke\x1b\\");
        let notifs = t.drain_notifications();
        assert_eq!(notifs.len(), 1);
        assert_eq!(notifs[0].body, "Something broke");
        assert_eq!(notifs[0].urgency, NotificationUrgency::Critical);
    }

    #[test]
    fn osc_99_default_urgency_when_missing() {
        let mut t = term();
        t.process(b"\x1b]99;i=abc;Plain message\x1b\\");
        assert_eq!(
            t.drain_notifications()[0].urgency,
            NotificationUrgency::Normal
        );
    }

    #[test]
    fn osc_8_stamps_hyperlink_id_on_cells() {
        let mut t = term();
        // Open a link, write "hi", close it, write " x" unlinked.
        t.process(b"\x1b]8;;https://example.com\x1b\\hi\x1b]8;;\x1b\\ x");
        let c0 = t.grid.cell(0, 0).unwrap();
        let c1 = t.grid.cell(0, 1).unwrap();
        let c2 = t.grid.cell(0, 2).unwrap();
        let c3 = t.grid.cell(0, 3).unwrap();
        assert_eq!(c0.grapheme, 'h');
        assert_eq!(c1.grapheme, 'i');
        assert_ne!(c0.hyperlink_id, 0, "h should have link id");
        assert_eq!(c0.hyperlink_id, c1.hyperlink_id, "both link cells share id");
        assert_eq!(c2.grapheme, ' ');
        assert_eq!(c3.grapheme, 'x');
        assert_eq!(c2.hyperlink_id, 0, "post-close cells unlinked");
        assert_eq!(c3.hyperlink_id, 0, "post-close cells unlinked");
        assert_eq!(t.hyperlink_uri(c0.hyperlink_id), Some("https://example.com"));
    }

    #[test]
    fn osc_8_file_uri_lookup_roundtrip() {
        let mut t = term();
        t.process(b"\x1b]8;;file:///Users/example/foo.rs\x1b\\x\x1b]8;;\x1b\\");
        let c0 = t.grid.cell(0, 0).unwrap();
        assert_eq!(
            t.hyperlink_uri(c0.hyperlink_id),
            Some("file:///Users/example/foo.rs")
        );
    }

    #[test]
    fn osc_8_missing_uri_closes_link() {
        let mut t = term();
        // Open, write 'a', explicit close via empty uri, write 'b'.
        t.process(b"\x1b]8;;https://a.b\x1b\\a\x1b]8;;\x1b\\b");
        let c0 = t.grid.cell(0, 0).unwrap();
        let c1 = t.grid.cell(0, 1).unwrap();
        assert_ne!(c0.hyperlink_id, 0);
        assert_eq!(c1.hyperlink_id, 0);
    }

    #[test]
    fn hyperlink_uri_returns_none_for_zero_id() {
        let t = term();
        assert_eq!(t.hyperlink_uri(0), None);
    }

    #[test]
    fn resize_compounding_narrow_then_widen_preserves_total() {
        // Regression for the "scrollback compounding on resize" bug. Resize
        // narrow → widen back to original should preserve total row count
        // (chains split then re-merge). If reflow ever leaks rows or
        // double-pushes, this test catches it.
        let mut t = Terminal::new(132, 46);
        for i in 0..50 {
            let line = format!(
                "line{i:03} this is content that may autowrap depending on column width {}",
                "x".repeat(40)
            );
            t.process(line.as_bytes());
            t.process(b"\r\n");
        }
        let initial_total = t.scrollback.len() + t.grid.row_count();

        t.resize(80, 46);
        let narrow_total = t.scrollback.len() + t.grid.row_count();
        t.resize(132, 46);
        let widen_total = t.scrollback.len() + t.grid.row_count();

        assert!(
            narrow_total >= initial_total,
            "narrow should not lose content (initial={initial_total}, narrow={narrow_total})"
        );
        assert!(
            narrow_total < initial_total * 3,
            "narrow should not triple content (initial={initial_total}, narrow={narrow_total})"
        );
        // Widen back to original: ±20% tolerance for content boundary shifts.
        assert!(
            widen_total >= initial_total.saturating_sub(initial_total / 5),
            "widen back to original lost content (initial={initial_total}, widen={widen_total})"
        );
        assert!(
            widen_total <= initial_total + initial_total / 5,
            "widen back to original GREW content (initial={initial_total}, widen={widen_total})"
        );
    }
}
