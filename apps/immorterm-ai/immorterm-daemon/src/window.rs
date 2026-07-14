//! Native GUI host — winit 0.30 window with GPU-rendered terminal.
//!
//! `immorterm-ai gui [session-name] [--shell /bin/bash]` connects to a daemon
//! session (spawning one if needed) and renders its output at 60fps via wgpu.
//! The GUI is a thin client — the daemon owns the PTY, Claude tracker, and session
//! state. Closing the window detaches; the session persists.

use std::io::{Read as IoRead, Write as IoWrite};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::TryRecvError;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use immorterm_core::ai_layer::AiPrimitive;
use immorterm_core::Terminal;
use immorterm_render::popup::{PopupRenderData, PopupRenderItem};
use immorterm_render::renderer::{RenderOptions, Selection};
use immorterm_render::statusbar::{
    self, AiStatsMode, StatusBarTarget, StatusBarTheme, THEME_PRESETS,
};
use immorterm_render::TerminalRenderer;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use crate::ipc::{Request, Response};

/// Input writer — sends raw keyboard bytes to the daemon's input stream.
type InputWriter = Arc<Mutex<UnixStream>>;

struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: TerminalRenderer,
}

/// Snapshot of Claude Code session stats, polled from the daemon every 5 seconds.
#[derive(Default, Clone)]
struct ClaudeStatsSnapshot {
    active: bool,
    model: String,
    cost_usd: f64,
    context_pct: f64,
    /// Active vendor (lowercase id from daemon: "claude" / "codex" / etc.).
    /// Empty when no tool detected. Display formatting (Title Case) is
    /// done at render time in `format_ai_stats`.
    tool: String,
}

/// Which popup menu is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopupKind {
    MainMenu,
    SessionPicker,
    ThemePicker,
}

/// Active popup menu state in the window.
struct ActivePopup {
    kind: PopupKind,
    items: Vec<PopupRenderItem>,
    selected: usize,
    anchor_col: usize,
    width: usize,
    /// Session names for session picker (parallel to items).
    session_names: Vec<String>,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    terminal: Terminal,

    // Daemon connection (replaces direct PTY ownership)
    output_rx: mpsc::Receiver<Vec<u8>>,
    ai_layer_rx: mpsc::Receiver<Vec<AiPrimitive>>,
    input_writer: InputWriter,
    socket_path: PathBuf,

    // Session info from daemon
    session_name: String,
    project: String,

    // Status bar (local animations + polled Claude stats)
    shell: String,
    created_at: Instant,
    last_io_time: Instant,
    claude_stats: Arc<Mutex<ClaudeStatsSnapshot>>,

    // Interactive status bar state
    status_bar_hover: StatusBarTarget,
    status_bar_theme: StatusBarTheme,
    ai_stats_mode: AiStatsMode,
    popup: Option<ActivePopup>,
    /// Cached status bar data for hit-testing between frames.
    last_status_bar_cols: usize,
    /// Accumulated time for title marquee animation (seconds).
    title_marquee_time: f32,
    /// Cached title overflow chars for stable marquee cycle duration.
    title_marquee_overflow: usize,

    // UI state (unchanged from standalone mode)
    selection: Selection,
    mouse_down: bool,
    mouse_pos: (f64, f64),
    scroll_pixels: f64,
    modifiers: ModifiersState,
    focused: bool,
}

impl App {
    fn init_gpu(&mut self, window: Arc<Window>) {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(window.clone())
            .expect("Failed to create surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("No suitable GPU adapter found");

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("immorterm_device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
            },
            None,
        ))
        .expect("Failed to create GPU device");

        // Log GPU errors instead of crashing
        device.on_uncaptured_error(Box::new(|error| {
            tracing::error!("wgpu error: {error}");
        }));

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .find(|f| !f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let scale_factor = window.scale_factor() as f32;
        let font_size = 14.0 * scale_factor;
        let mut renderer = TerminalRenderer::new(&device, &queue, format, None, font_size);
        renderer.status_bar_enabled = true;

        let (cols, rows) = renderer.cell_metrics();
        let term_cols = (size.width as f32 / cols).floor() as usize;
        let term_rows = (size.height as f32 / rows).floor() as usize;
        self.terminal.resize(term_cols.max(1), term_rows.max(1));

        // Resize daemon's PTY to match window
        send_control_request(&self.socket_path, Request::Resize {
            cols: term_cols.max(1) as u16,
            rows: term_rows.max(1) as u16,
        });

        self.gpu = Some(GpuState {
            surface,
            device,
            queue,
            config,
            renderer,
        });
    }

    /// Write raw bytes to the daemon's input stream.
    fn write_input(&self, data: &[u8]) {
        if let Ok(mut writer) = self.input_writer.lock() {
            let _ = writer.write_all(data);
            let _ = writer.flush();
        }
    }

    /// Compute xterm-style modifier parameter for CSI sequences.
    /// Encodes shift/alt/ctrl into a single number: 1 + shift(1) + alt(2) + ctrl(4).
    /// Returns None if no modifiers are held (use the plain sequence).
    fn xterm_modifier(shift: bool, alt: bool, ctrl: bool) -> Option<u8> {
        let m = (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2);
        if m > 0 { Some(1 + m) } else { None }
    }

    /// Format a CSI key sequence with optional xterm modifier.
    /// Plain: `ESC [ code`. Modified: `ESC [ 1 ; mod code`.
    fn csi_key(code: u8, modifier: Option<u8>) -> Vec<u8> {
        match modifier {
            Some(m) => format!("\x1b[1;{}{}", m, code as char).into_bytes(),
            None => vec![0x1b, b'[', code],
        }
    }

    /// Format an SS3 or CSI key depending on app-cursor mode + modifiers.
    /// App-cursor (no mods): `ESC O code`. Otherwise: CSI with modifier.
    fn cursor_key(code: u8, app_mode: bool, modifier: Option<u8>) -> Vec<u8> {
        if modifier.is_none() && app_mode {
            vec![0x1b, b'O', code]
        } else {
            Self::csi_key(code, modifier)
        }
    }

    /// Format AI stats string based on current display mode.
    ///
    /// Vendor prefix appears in every visible mode so a glance tells you
    /// "this terminal is on Codex" vs "this terminal is on Claude" — even
    /// when the model alone is ambiguous (Cursor wrapping Sonnet looks
    /// identical to Claude wrapping Sonnet otherwise).
    fn format_ai_stats(&self) -> String {
        let stats = self.claude_stats.lock().unwrap();
        if !stats.active {
            return String::new();
        }
        let vendor = title_case_vendor(&stats.tool);
        match self.ai_stats_mode {
            AiStatsMode::Full => {
                let model = if stats.model.is_empty() {
                    String::new()
                } else {
                    format!(" {}", stats.model)
                };
                format!(
                    "{}{} ${:.2} ctx:{:.0}%",
                    vendor, model, stats.cost_usd, stats.context_pct
                )
            }
            AiStatsMode::Compact => {
                if vendor.is_empty() {
                    format!("${:.2}", stats.cost_usd)
                } else {
                    format!("{} ${:.2}", vendor, stats.cost_usd)
                }
            }
            AiStatsMode::Hidden => String::new(),
        }
    }

    /// Cell height in pixels (for scroll calculations).
    fn cell_height(&self) -> f64 {
        self.gpu
            .as_ref()
            .map(|g| g.renderer.atlas.metrics.cell_height as f64)
            .unwrap_or(35.0)
    }

    /// Scroll by `px` pixels. Positive = into history, negative = toward live.
    fn scroll_by_px(&mut self, px: f64) {
        let max_lines = TerminalRenderer::max_scroll(&self.terminal);
        let max_px = max_lines as f64 * self.cell_height();
        self.scroll_pixels = (self.scroll_pixels + px).clamp(0.0, max_px);
    }

    /// Scroll by whole lines (for keyboard: PageUp/Down etc).
    fn scroll_by_lines(&mut self, lines: isize) {
        self.scroll_by_px(lines as f64 * self.cell_height());
    }

    /// Current scroll offset in whole lines.
    fn scroll_lines(&self) -> usize {
        let ch = self.cell_height();
        (self.scroll_pixels / ch).floor() as usize
    }

    /// Copy the current selection to clipboard.
    fn copy_selection(&self) {
        if !self.selection.is_active {
            return;
        }

        let ((sc, sr), (ec, er)) = self.selection.range();
        let mut text = String::new();

        let sb_len = self.terminal.scrollback.len();
        let grid = &self.terminal.grid;

        for row in sr..=er {
            // Selection rows are absolute content indices
            let cells = if row < sb_len {
                self.terminal.scrollback.get(row).map(|r| &r.cells)
            } else {
                grid.row(row - sb_len).map(|r| &r.cells)
            };

            if let Some(cells) = cells {
                let start_col = if row == sr { sc } else { 0 };
                let end_col = if row == er {
                    ec.min(cells.len().saturating_sub(1))
                } else {
                    cells.len().saturating_sub(1)
                };

                for col in start_col..=end_col {
                    if let Some(cell) = cells.get(col)
                        && cell.width > 0 {
                            text.push(cell.grapheme);
                        }
                }
                if row < er {
                    text.push('\n');
                }
            }
        }

        // Trim trailing whitespace per line
        let text: String = text
            .lines()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n");

        if !text.is_empty()
            && let Ok(mut clipboard) = arboard::Clipboard::new() {
                let _ = clipboard.set_text(&text);
            }
    }

    /// Paste clipboard content into daemon's PTY.
    ///
    /// Priority order:
    /// 1. Finder file URL — type the absolute path so Claude Code reads the
    ///    file via Read. Catches PDFs/docs/images-as-files where the
    ///    pasteboard also has a preview thumbnail we'd otherwise mistake
    ///    for a standalone image.
    /// 2. Image bytes (e.g. screenshot) — emit empty bracketed-paste markers
    ///    so Claude Code reads NSPasteboard itself and produces [Image #N].
    /// 3. Text fallback.
    fn paste_clipboard(&self) {
        let Ok(mut clipboard) = arboard::Clipboard::new() else { return };

        let has_image = clipboard.get_image().is_ok();
        if has_image
            && let Some(path) = crate::websocket::read_clipboard_file_url() {
                self.write_paste(path.as_bytes());
                return;
            }

        if has_image && self.terminal.modes.bracketed_paste {
            self.write_input(b"\x1b[200~\x1b[201~");
            return;
        }

        if let Ok(text) = clipboard.get_text() {
            self.write_paste(text.as_bytes());
        }
    }

    /// Write `bytes` to the PTY, wrapping in bracketed-paste markers when
    /// the terminal is in bracketed-paste mode.
    fn write_paste(&self, bytes: &[u8]) {
        if self.terminal.modes.bracketed_paste {
            self.write_input(b"\x1b[200~");
            self.write_input(bytes);
            self.write_input(b"\x1b[201~");
        } else {
            self.write_input(bytes);
        }
    }

    /// Get the character at an absolute content position (scrollback + grid).
    fn cell_char_at(&self, col: usize, abs_row: usize) -> char {
        let sb_len = self.terminal.scrollback.len();
        if abs_row < sb_len {
            self.terminal
                .scrollback
                .get(abs_row)
                .and_then(|r| r.cells.get(col))
                .map(|c| c.grapheme)
                .unwrap_or(' ')
        } else {
            self.terminal
                .grid
                .cell(abs_row - sb_len, col)
                .map(|c| c.grapheme)
                .unwrap_or(' ')
        }
    }

    /// Move selection active point by word in the given direction.
    /// dx: -1 = left, +1 = right. dy: -1 = up (word on prev line), +1 = down.
    fn move_selection_by_word(&mut self, dx: i32, dy: i32, cols: usize, max_row: usize) {
        let (mut col, mut row) = self.selection.active;

        if dy != 0 {
            // Shift+Option+Up/Down: move one row (same as plain Shift+Up/Down)
            if dy < 0 && row > 0 {
                row -= 1;
            } else if dy > 0 && row < max_row {
                row += 1;
            }
            self.selection.active = (col.min(cols.saturating_sub(1)), row);
            return;
        }

        // Horizontal word movement: skip whitespace, then skip word chars (or vice versa)
        let is_word_char = |ch: char| ch.is_alphanumeric() || ch == '_';
        let last_col = cols.saturating_sub(1);

        if dx < 0 {
            // Move left: skip one character, then skip backward over same-class chars
            if col > 0 {
                col -= 1;
            } else if row > 0 {
                row -= 1;
                col = last_col;
            }
            // Skip whitespace
            while col > 0 || row > 0 {
                let ch = self.cell_char_at(col, row);
                if !ch.is_whitespace() {
                    break;
                }
                if col > 0 {
                    col -= 1;
                } else if row > 0 {
                    row -= 1;
                    col = last_col;
                } else {
                    break;
                }
            }
            // Skip word characters
            let on_word = is_word_char(self.cell_char_at(col, row));
            while col > 0 || row > 0 {
                let prev_col = if col > 0 { col - 1 } else { last_col };
                let prev_row = if col > 0 { row } else { row.saturating_sub(1) };
                if col == 0 && row == 0 {
                    break;
                }
                let ch = self.cell_char_at(prev_col, prev_row);
                if on_word != is_word_char(ch) || ch.is_whitespace() {
                    break;
                }
                col = prev_col;
                row = prev_row;
            }
        } else {
            // Move right: skip current class, then skip whitespace
            let on_word = is_word_char(self.cell_char_at(col, row));
            // Skip same-class characters
            while col < last_col || row < max_row {
                let ch = self.cell_char_at(col, row);
                if on_word != is_word_char(ch) || ch.is_whitespace() {
                    break;
                }
                if col < last_col {
                    col += 1;
                } else if row < max_row {
                    row += 1;
                    col = 0;
                } else {
                    break;
                }
            }
            // Skip whitespace
            while col < last_col || row < max_row {
                let ch = self.cell_char_at(col, row);
                if !ch.is_whitespace() {
                    break;
                }
                if col < last_col {
                    col += 1;
                } else if row < max_row {
                    row += 1;
                    col = 0;
                } else {
                    break;
                }
            }
        }

        self.selection.active = (col, row);
    }

    /// Open the main menu popup (brand click) — sessions, themes, actions.
    fn open_main_menu(&mut self) {
        let sessions = crate::commands::discover_sessions();
        let mut items = Vec::new();
        let mut session_names = Vec::new();

        // Section: Sessions
        for session in &sessions {
            if !session.alive {
                continue;
            }
            let status = if session.attached { "Attached" } else { "Detached" };
            items.push(PopupRenderItem {
                label: format!("{} ({})", session.name, status),
                checked: session.name == self.session_name,
                separator_after: false,
                enabled: true,
            });
            session_names.push(session.name.clone());
        }

        // Separator between sessions and actions
        if let Some(last) = items.last_mut() {
            last.separator_after = true;
        }

        // Action: New session
        items.push(PopupRenderItem {
            label: "+ New session".to_string(),
            checked: false,
            separator_after: true,
            enabled: true,
        });
        session_names.push("__new__".to_string());

        // Section: Themes
        for theme in THEME_PRESETS {
            items.push(PopupRenderItem {
                label: format!("  {}", theme.name),
                checked: theme.name == self.status_bar_theme.name,
                separator_after: false,
                enabled: true,
            });
            session_names.push(format!("__theme__{}", theme.name));
        }

        // Separator between themes and appearance
        if let Some(last) = items.last_mut() {
            last.separator_after = true;
        }

        // Section: Appearance toggles
        let renderer_vals = self.gpu.as_ref().map(|g| (
            g.renderer.expression_effects,
            g.renderer.celebrations_enabled,
            g.renderer.danger_effects,
            g.renderer.text_animations,
        ));
        let (expr, celeb, danger, text_anim) = renderer_vals.unwrap_or((true, true, true, true));

        for (label, enabled, key) in [
            ("Expression Effects", expr, "__toggle__expression_effects"),
            ("Celebrations", celeb, "__toggle__celebrations"),
            ("Danger Effects", danger, "__toggle__danger_effects"),
            ("Text Animations", text_anim, "__toggle__text_animations"),
        ] {
            items.push(PopupRenderItem {
                label: format!("  {}", label),
                checked: enabled,
                separator_after: false,
                enabled: true,
            });
            session_names.push(key.to_string());
        }

        // Calculate menu width
        let max_label = items.iter().map(|i| i.label.len()).max().unwrap_or(10);
        let width = max_label + 8;
        let cols = self.terminal.cols();
        // Anchor near the brand text (right side of status bar)
        let anchor = cols.saturating_sub(width + 2);

        self.popup = Some(ActivePopup {
            kind: PopupKind::MainMenu,
            items,
            selected: 0,
            anchor_col: anchor,
            width,
            session_names,
        });
    }

    /// Open the session picker popup (brand click).
    fn open_session_picker(&mut self) {
        let sessions = crate::commands::discover_sessions();
        let mut items = Vec::new();
        let mut session_names = Vec::new();

        for session in &sessions {
            if !session.alive {
                continue;
            }
            let status = if session.attached { "Attached" } else { "Detached" };
            items.push(PopupRenderItem {
                label: format!("{} ({})", session.name, status),
                checked: session.name == self.session_name,
                separator_after: false,
                enabled: true,
            });
            session_names.push(session.name.clone());
        }

        // Add separator before action items
        if let Some(last) = items.last_mut() {
            last.separator_after = true;
        }

        items.push(PopupRenderItem {
            label: "+ New session".to_string(),
            checked: false,
            separator_after: false,
            enabled: true,
        });
        session_names.push(String::new()); // placeholder

        // Calculate menu width (max item label + padding)
        let max_label = items.iter().map(|i| i.label.len()).max().unwrap_or(10);
        let width = max_label + 8; // prefix + padding

        // Anchor at brand start column
        let anchor = self
            .terminal
            .cols()
            .saturating_sub(width);

        self.popup = Some(ActivePopup {
            kind: PopupKind::SessionPicker,
            items,
            selected: 0,
            anchor_col: anchor,
            width,
            session_names,
        });
    }

    /// Open the theme picker popup (theme area click).
    fn open_theme_picker(&mut self) {
        let items: Vec<PopupRenderItem> = THEME_PRESETS
            .iter()
            .map(|theme| PopupRenderItem {
                label: theme.name.to_string(),
                checked: theme.name == self.status_bar_theme.name,
                separator_after: false,
                enabled: true,
            })
            .collect();

        let max_label = items.iter().map(|i| i.label.len()).max().unwrap_or(10);
        let width = max_label + 8;
        let cols = self.terminal.cols();
        // Anchor near the theme area (right-center)
        let anchor = cols.saturating_sub(width + 14);

        self.popup = Some(ActivePopup {
            kind: PopupKind::ThemePicker,
            items,
            selected: 0,
            anchor_col: anchor,
            width,
            session_names: Vec::new(),
        });
    }

    /// Handle a popup action (selected item).
    fn handle_popup_action(&mut self) {
        let popup = match self.popup.take() {
            Some(p) => p,
            None => return,
        };

        let idx = popup.selected;
        match popup.kind {
            PopupKind::MainMenu => {
                // Unified menu: session_names encodes the action
                if idx < popup.session_names.len() {
                    let action = &popup.session_names[idx];
                    if action == "__new__" {
                        self.create_new_session();
                    } else if let Some(theme_name) = action.strip_prefix("__theme__") {
                        // Theme selection
                        if let Some(theme) = THEME_PRESETS.iter().find(|t| t.name == theme_name) {
                            self.status_bar_theme = *theme;
                        }
                    } else if let Some(toggle_key) = action.strip_prefix("__toggle__") {
                        if let Some(ref mut gpu) = self.gpu {
                            match toggle_key {
                                "expression_effects" => gpu.renderer.expression_effects = !gpu.renderer.expression_effects,
                                "celebrations" => gpu.renderer.celebrations_enabled = !gpu.renderer.celebrations_enabled,
                                "danger_effects" => gpu.renderer.danger_effects = !gpu.renderer.danger_effects,
                                "text_animations" => gpu.renderer.text_animations = !gpu.renderer.text_animations,
                                _ => {}
                            }
                        }
                    } else if !action.is_empty() && *action != self.session_name {
                        // Session switch
                        self.switch_session(action);
                    }
                }
            }
            PopupKind::SessionPicker => {
                if idx < popup.session_names.len() && !popup.session_names[idx].is_empty() {
                    let target_name = popup.session_names[idx].clone();
                    if target_name != self.session_name {
                        self.switch_session(&target_name);
                    }
                } else if idx == popup.items.len() - 1 {
                    self.create_new_session();
                }
            }
            PopupKind::ThemePicker => {
                if idx < THEME_PRESETS.len() {
                    self.status_bar_theme = THEME_PRESETS[idx];
                }
            }
        }
    }

    /// Switch to a different daemon session.
    fn switch_session(&mut self, target_name: &str) {
        // Find the socket for the target session
        let socket_path = match crate::commands::find_session_socket_sync(target_name) {
            Ok(path) => path,
            Err(e) => {
                tracing::error!("Failed to find session '{}': {}", target_name, e);
                return;
            }
        };

        // Reconnect to the new daemon
        match connect_to_daemon(&socket_path) {
            Ok((output_rx, ai_layer_rx, input_writer, project, _title, cols, rows)) => {
                self.output_rx = output_rx;
                self.ai_layer_rx = ai_layer_rx;
                self.input_writer = input_writer;
                self.socket_path = socket_path;
                self.session_name = target_name.to_string();
                self.project = project;
                // Clear terminal content from previous session
                self.terminal = Terminal::new(cols, rows);
                self.terminal.set_scrollback(50_000);
                self.scroll_pixels = 0.0;

                // Update window title
                if let Some(window) = &self.window {
                    window.set_title(&format!("ImmorTerm \u{2014} {}", target_name));
                }

                // Resize daemon's PTY to match current window
                if let Some(gpu) = &mut self.gpu {
                    let (cols, rows) = gpu.renderer.resize(
                        &gpu.device,
                        gpu.config.width,
                        gpu.config.height,
                    );
                    self.terminal.resize(cols, rows);
                    send_control_request(
                        &self.socket_path,
                        Request::Resize {
                            cols: cols as u16,
                            rows: rows as u16,
                        },
                    );
                }
            }
            Err(e) => {
                tracing::error!("Failed to connect to session '{}': {}", target_name, e);
            }
        }
    }

    /// Create a new daemon session and switch to it.
    fn create_new_session(&mut self) {
        // Use timestamp for unique session names (PID is same for all from this GUI)
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let name = format!("gui-{}-{}", std::process::id(), ts % 10000);
        match crate::daemon::create_session(&name, &self.shell, 50_000, None, false, None) {
            Ok(_) => {
                // Wait for socket to appear
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    if crate::commands::find_session_socket_sync(&name).is_ok() {
                        self.switch_session(&name);
                        return;
                    }
                    if Instant::now() > deadline {
                        tracing::error!("New session '{}' socket didn't appear in time", name);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            Err(e) => {
                tracing::error!("Failed to create session: {}", e);
            }
        }
    }

    /// Build PopupRenderData from the current ActivePopup state.
    fn popup_render_data(&self) -> Option<PopupRenderData> {
        self.popup.as_ref().map(|p| PopupRenderData {
            items: p.items.clone(),
            selected_index: p.selected,
            anchor_col: p.anchor_col,
            width_cols: p.width,
            visible: true,
        })
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let title = format!("ImmorTerm — {}", self.session_name);
        let attrs = WindowAttributes::default()
            .with_title(&title)
            .with_inner_size(winit::dpi::LogicalSize::new(960, 600));

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("Failed to create window"),
        );

        self.init_gpu(window.clone());
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            // Close window = detach from daemon. Session persists.
            WindowEvent::Focused(focused) => {
                self.focused = focused;
            }
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                // Dismiss popup on resize (simpler than re-layout)
                self.popup = None;

                if let Some(gpu) = &mut self.gpu {
                    gpu.config.width = size.width.max(1);
                    gpu.config.height = size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.config);

                    let (cols, rows) = gpu.renderer.resize(&gpu.device, size.width, size.height);
                    self.terminal.resize(cols, rows);

                    // Resize daemon's PTY via IPC
                    send_control_request(&self.socket_path, Request::Resize {
                        cols: cols as u16,
                        rows: rows as u16,
                    });
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let px = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64 * self.cell_height(),
                    MouseScrollDelta::PixelDelta(pos) => pos.y,
                };
                self.scroll_by_px(px);
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_pos = (position.x, position.y);

                // Status bar hover detection
                if let Some(gpu) = &self.gpu {
                    let cw = gpu.renderer.atlas.metrics.cell_width;
                    let ch = gpu.renderer.atlas.metrics.cell_height;
                    let col = (position.x as f32 / cw).floor() as usize;
                    let display_row = (position.y as f32 / ch).floor() as usize;
                    let visible_rows = self.terminal.grid.num_rows();

                    // Check if cursor is on the status bar row
                    let new_hover = if gpu.renderer.status_bar_enabled
                        && display_row == visible_rows
                    {
                        // Build a minimal StatusBarData for hit-testing
                        let cols = self.terminal.cols();
                        let time_secs = self.created_at.elapsed().as_secs_f32();
                        let title = if self.terminal.title.is_empty() {
                            self.shell.clone()
                        } else {
                            self.terminal.title.clone()
                        };
                        let project = if !self.project.is_empty() {
                            self.project.clone()
                        } else {
                            extract_project_name(&title, &self.shell)
                        };
                        let dot = statusbar::animated_dot_char(time_secs);
                        let last_active = format_last_active(self.last_io_time);
                        let ai_stats = self.format_ai_stats();
                        let data = statusbar::build_sections_with_theme(
                            &project,
                            &title,
                            &ai_stats,
                            &last_active,
                            dot,
                            cols,
                            0.0, // no CTX bar in native window
                            &self.status_bar_theme,
                            0, 0.0, // no scroll for hit-testing
                        );
                        statusbar::hit_test(&data, col)
                    } else {
                        StatusBarTarget::None
                    };

                    // Update cursor icon
                    if new_hover != self.status_bar_hover {
                        self.status_bar_hover = new_hover;
                        if let Some(window) = &self.window {
                            if new_hover != StatusBarTarget::None {
                                window.set_cursor(CursorIcon::Pointer);
                            } else {
                                window.set_cursor(CursorIcon::Default);
                            }
                        }
                    }
                }

                if self.mouse_down
                    && let Some(gpu) = &self.gpu {
                        let cw = gpu.renderer.atlas.metrics.cell_width;
                        let ch = gpu.renderer.atlas.metrics.cell_height;
                        let col = (position.x as f32 / cw).floor() as usize;
                        let display_row = (position.y as f32 / ch).floor() as usize;
                        // Store as absolute content position so selection
                        // sticks to content, not viewport.
                        let sb_len = self.terminal.scrollback.len();
                        let abs_row = (sb_len + display_row).saturating_sub(self.scroll_lines());
                        self.selection.active = (col, abs_row);
                    }
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(gpu) = &self.gpu {
                    let cw = gpu.renderer.atlas.metrics.cell_width;
                    let ch = gpu.renderer.atlas.metrics.cell_height;
                    let col = (self.mouse_pos.0 as f32 / cw).floor() as usize;
                    let display_row = (self.mouse_pos.1 as f32 / ch).floor() as usize;
                    let sb_len = self.terminal.scrollback.len();
                    let abs_row = (sb_len + display_row).saturating_sub(self.scroll_lines());
                    let visible_rows = self.terminal.grid.num_rows();

                    match state {
                        ElementState::Pressed => {
                            // Check if clicking inside popup
                            if let Some(popup_data) = self.popup_render_data() {
                                if popup_data.contains(col, display_row, visible_rows) {
                                    // Click inside popup — select item
                                    if let Some(item_idx) =
                                        popup_data.item_at_row(display_row, visible_rows)
                                    {
                                        if let Some(popup) = &mut self.popup {
                                            popup.selected = item_idx;
                                        }
                                        self.handle_popup_action();
                                    }
                                    return;
                                } else {
                                    // Click outside popup — dismiss
                                    self.popup = None;
                                }
                            }

                            // Check if clicking on status bar
                            if gpu.renderer.status_bar_enabled
                                && display_row == visible_rows
                            {
                                match self.status_bar_hover {
                                    StatusBarTarget::Brand => {
                                        // Brand click opens the main menu (sessions + themes)
                                        self.open_main_menu();
                                        return;
                                    }
                                    StatusBarTarget::AiStats => {
                                        // Direct toggle — no popup needed
                                        self.ai_stats_mode = self.ai_stats_mode.next();
                                        return;
                                    }
                                    StatusBarTarget::Title | StatusBarTarget::ThemeArea | StatusBarTarget::Project | StatusBarTarget::Scratch | StatusBarTarget::None => {
                                        // No action for title, theme area, project click, or
                                        // empty space. Scratch toggle is webview-only — the
                                        // native window has no second-terminal surface.
                                    }
                                }
                            }

                            // Normal terminal click
                            self.mouse_down = true;
                            self.selection.anchor = (col, abs_row);
                            self.selection.active = (col, abs_row);
                            self.selection.is_active = true;
                        }
                        ElementState::Released => {
                            self.mouse_down = false;
                            if self.selection.anchor == self.selection.active {
                                // Click without drag — clear selection
                                self.selection.is_active = false;
                            }
                        }
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                // ── Popup keyboard navigation ──
                if self.popup.is_some() {
                    match &event.logical_key {
                        Key::Named(NamedKey::ArrowUp) => {
                            if let Some(popup) = &mut self.popup
                                && popup.selected > 0 {
                                    popup.selected -= 1;
                                }
                            return;
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            if let Some(popup) = &mut self.popup
                                && popup.selected + 1 < popup.items.len() {
                                    popup.selected += 1;
                                }
                            return;
                        }
                        Key::Named(NamedKey::Enter) => {
                            self.handle_popup_action();
                            return;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.popup = None;
                            return;
                        }
                        _ => {
                            // Any other key dismisses popup and is forwarded to PTY
                            self.popup = None;
                        }
                    }
                }

                let ctrl = self.modifiers.control_key();
                let shift = self.modifiers.shift_key();
                let super_key = self.modifiers.super_key();
                let alt = self.modifiers.alt_key(); // Option on macOS

                // ── Cmd shortcuts (macOS) / Ctrl+Shift (Linux) ──
                if super_key || (ctrl && shift) {
                    match &event.logical_key {
                        Key::Character(c) if c.as_str() == "c" || c.as_str() == "C" => {
                            self.copy_selection();
                            return;
                        }
                        Key::Character(c) if c.as_str() == "v" || c.as_str() == "V" => {
                            self.paste_clipboard();
                            return;
                        }
                        _ => {}
                    }
                }

                // ── Cmd+Shift+Arrow: select to beginning/end of line ──
                if super_key && shift {
                    match &event.logical_key {
                        Key::Named(NamedKey::ArrowLeft) | Key::Named(NamedKey::ArrowRight) => {
                            let sb_len = self.terminal.scrollback.len();
                            let cols = self.terminal.cols();
                            if !self.selection.is_active {
                                let cursor_abs_row = sb_len + self.terminal.cursor.row;
                                let cursor_col = self.terminal.cursor.col;
                                self.selection.anchor = (cursor_col, cursor_abs_row);
                                self.selection.active = (cursor_col, cursor_abs_row);
                                self.selection.is_active = true;
                            }
                            let row = self.selection.active.1;
                            if matches!(&event.logical_key, Key::Named(NamedKey::ArrowLeft)) {
                                self.selection.active = (0, row);
                            } else {
                                self.selection.active = (cols.saturating_sub(1), row);
                            }
                            return;
                        }
                        _ => {}
                    }
                }

                // ── Cmd+key (macOS line-level shortcuts) ──
                if super_key {
                    let bytes: Option<Vec<u8>> = match &event.logical_key {
                        Key::Named(NamedKey::ArrowLeft) => Some(vec![0x01]),    // Ctrl+A: beginning of line
                        Key::Named(NamedKey::ArrowRight) => Some(vec![0x05]),   // Ctrl+E: end of line
                        Key::Named(NamedKey::Backspace) => Some(vec![0x15]),    // Ctrl+U: kill line backward
                        Key::Character(c) if c.as_str() == "k" || c.as_str() == "K" => {
                            Some(vec![0x0b]) // Ctrl+K: kill to end of line
                        }
                        _ => None,
                    };
                    if let Some(data) = bytes {
                        self.write_input(&data);
                        return;
                    }
                }

                // ── Shift+Arrow for emulator-level text selection ──
                // Shift+Arrow selects character-by-character (doesn't send to PTY).
                // Shift+Option+Arrow selects word-by-word.
                if shift && !super_key && !ctrl {
                    let arrow = match &event.logical_key {
                        Key::Named(NamedKey::ArrowLeft) => Some((-1i32, 0i32)),
                        Key::Named(NamedKey::ArrowRight) => Some((1, 0)),
                        Key::Named(NamedKey::ArrowUp) => Some((0, -1)),
                        Key::Named(NamedKey::ArrowDown) => Some((0, 1)),
                        _ => None,
                    };
                    if let Some((dx, dy)) = arrow {
                        let sb_len = self.terminal.scrollback.len();
                        let cols = self.terminal.cols();
                        let max_row = sb_len + self.terminal.rows() - 1;

                        // If no selection, anchor at terminal cursor position
                        if !self.selection.is_active {
                            let cursor_abs_row = sb_len + self.terminal.cursor.row;
                            let cursor_col = self.terminal.cursor.col;
                            self.selection.anchor = (cursor_col, cursor_abs_row);
                            self.selection.active = (cursor_col, cursor_abs_row);
                            self.selection.is_active = true;
                        }

                        let (mut col, mut row) = self.selection.active;

                        if alt {
                            // Shift+Option+Arrow: word selection
                            self.move_selection_by_word(dx, dy, cols, max_row);
                        } else if dy != 0 {
                            // Shift+Up/Down: move one row
                            if dy < 0 && row > 0 {
                                row -= 1;
                            } else if dy > 0 && row < max_row {
                                row += 1;
                            }
                            self.selection.active = (col.min(cols.saturating_sub(1)), row);
                        } else {
                            // Shift+Left/Right: move one character
                            if dx < 0 {
                                if col > 0 {
                                    col -= 1;
                                } else if row > 0 {
                                    row -= 1;
                                    col = cols.saturating_sub(1);
                                }
                            } else if col < cols.saturating_sub(1) {
                                col += 1;
                            } else if row < max_row {
                                row += 1;
                                col = 0;
                            }
                            self.selection.active = (col, row);
                        }

                        // Auto-scroll if selection moves outside visible viewport
                        let visible_rows = self.terminal.rows();
                        let scroll_lines = self.scroll_lines();
                        let first_visible = sb_len.saturating_sub(scroll_lines);
                        let last_visible = first_visible + visible_rows - 1;
                        let active_row = self.selection.active.1;
                        if active_row < first_visible {
                            let diff = first_visible - active_row;
                            self.scroll_by_lines(diff as isize);
                        } else if active_row > last_visible {
                            let diff = active_row - last_visible;
                            self.scroll_by_lines(-(diff as isize));
                        }

                        return;
                    }
                }

                // ── Shift+PageUp/Down for scrolling ──
                if shift {
                    match &event.logical_key {
                        Key::Named(NamedKey::PageUp) => {
                            let page = self.terminal.grid.num_rows().saturating_sub(1);
                            self.scroll_by_lines(page as isize);
                            return;
                        }
                        Key::Named(NamedKey::PageDown) => {
                            let page = self.terminal.grid.num_rows().saturating_sub(1);
                            self.scroll_by_lines(-(page as isize));
                            return;
                        }
                        Key::Named(NamedKey::Home) => {
                            let max = TerminalRenderer::max_scroll(&self.terminal);
                            self.scroll_pixels = max as f64 * self.cell_height();
                            return;
                        }
                        Key::Named(NamedKey::End) => {
                            self.scroll_pixels = 0.0;
                            return;
                        }
                        _ => {}
                    }
                }

                // ── Ctrl+key → control characters ──
                if ctrl && !shift && !super_key {
                    match &event.logical_key {
                        Key::Character(c) if c.as_str() == "c" || c.as_str() == "C" => {
                            if self.selection.is_active {
                                self.copy_selection();
                                self.selection.is_active = false;
                            } else {
                                self.write_input(&[0x03]);
                            }
                            return;
                        }
                        Key::Character(c) => {
                            let ch = c.chars().next().unwrap_or('\0');
                            if ch.is_ascii_lowercase() {
                                let ctrl_byte = (ch as u8) - b'a' + 1;
                                self.write_input(&[ctrl_byte]);
                                return;
                            }
                        }
                        _ => {}
                    }
                }

                // ── Any keypress while scrolled: snap back to live ──
                // (But not for modifier-only keys or Shift+Arrow selection)
                if self.scroll_pixels > 0.0 {
                    match &event.logical_key {
                        Key::Named(
                            NamedKey::Shift
                            | NamedKey::Control
                            | NamedKey::Alt
                            | NamedKey::Super
                            | NamedKey::PageUp
                            | NamedKey::PageDown
                            | NamedKey::Home
                            | NamedKey::End,
                        ) => {}
                        _ => {
                            self.scroll_pixels = 0.0;
                        }
                    }
                }

                // ── Clear selection on any non-Shift navigation/typing ──
                // Shift+Arrow extends selection (handled above with early return).
                // Everything else that reaches here dismisses it.
                if self.selection.is_active && !shift {
                    self.selection.is_active = false;
                }

                // ── Option (Alt/Meta) key handling ──
                // Option acts as Meta: prefix the key's normal output with ESC.
                // This makes Option+Backspace (delete word), Option+D (delete word
                // forward), Option+B/F (word left/right), etc. all work automatically
                // via readline/zsh ESC-prefixed keybindings.
                if alt && !ctrl && !super_key {
                    let bytes: Option<Vec<u8>> = match &event.logical_key {
                        Key::Named(NamedKey::Backspace) => Some(vec![0x1b, 0x7f]),
                        Key::Named(NamedKey::Delete) => Some(b"\x1bd".to_vec()),
                        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1bb".to_vec()),
                        Key::Named(NamedKey::ArrowRight) => Some(b"\x1bf".to_vec()),
                        Key::Named(NamedKey::Enter) => Some(b"\x1b\r".to_vec()),
                        Key::Character(c) => {
                            let mut v = vec![0x1b];
                            v.extend_from_slice(c.as_bytes());
                            Some(v)
                        }
                        _ => None,
                    };
                    if let Some(data) = bytes {
                        self.write_input(&data);
                        return;
                    }
                }

                // ── Standard key dispatch with xterm modifier encoding ──
                // Shift/Alt/Ctrl combos on arrow keys, Home, End, etc. are encoded
                // as CSI sequences: ESC [ 1 ; {mod} {code} — handled automatically.
                let mods = Self::xterm_modifier(shift, alt, ctrl);
                let app = self.terminal.modes.application_cursor_keys;

                let bytes: Option<Vec<u8>> = match &event.logical_key {
                    Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
                    Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
                    Key::Named(NamedKey::Tab) => {
                        if shift { Some(b"\x1b[Z".to_vec()) } else { Some(b"\t".to_vec()) }
                    }
                    Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
                    Key::Named(NamedKey::ArrowUp) => Some(Self::cursor_key(b'A', app, mods)),
                    Key::Named(NamedKey::ArrowDown) => Some(Self::cursor_key(b'B', app, mods)),
                    Key::Named(NamedKey::ArrowRight) => Some(Self::cursor_key(b'C', app, mods)),
                    Key::Named(NamedKey::ArrowLeft) => Some(Self::cursor_key(b'D', app, mods)),
                    Key::Named(NamedKey::Home) => Some(Self::csi_key(b'H', mods)),
                    Key::Named(NamedKey::End) => Some(Self::csi_key(b'F', mods)),
                    Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
                    Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
                    Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
                    Key::Named(NamedKey::Insert) => Some(b"\x1b[2~".to_vec()),
                    // Function keys
                    Key::Named(NamedKey::F1) => Some(b"\x1bOP".to_vec()),
                    Key::Named(NamedKey::F2) => Some(b"\x1bOQ".to_vec()),
                    Key::Named(NamedKey::F3) => Some(b"\x1bOR".to_vec()),
                    Key::Named(NamedKey::F4) => Some(b"\x1bOS".to_vec()),
                    Key::Named(NamedKey::F5) => Some(b"\x1b[15~".to_vec()),
                    Key::Named(NamedKey::F6) => Some(b"\x1b[17~".to_vec()),
                    Key::Named(NamedKey::F7) => Some(b"\x1b[18~".to_vec()),
                    Key::Named(NamedKey::F8) => Some(b"\x1b[19~".to_vec()),
                    Key::Named(NamedKey::F9) => Some(b"\x1b[20~".to_vec()),
                    Key::Named(NamedKey::F10) => Some(b"\x1b[21~".to_vec()),
                    Key::Named(NamedKey::F11) => Some(b"\x1b[23~".to_vec()),
                    Key::Named(NamedKey::F12) => Some(b"\x1b[24~".to_vec()),
                    _ => {
                        if !ctrl {
                            event.text.as_ref().map(|t| t.as_bytes().to_vec())
                        } else {
                            None
                        }
                    }
                };

                if let Some(data) = bytes {
                    self.write_input(&data);
                }
            }

            WindowEvent::RedrawRequested => {
                // Drain daemon output
                let mut daemon_disconnected = false;
                let sb_before = self.terminal.scrollback.len();
                loop {
                    match self.output_rx.try_recv() {
                        Ok(data) => {
                            self.terminal.process(&data);
                            self.last_io_time = Instant::now();
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            daemon_disconnected = true;
                            break;
                        }
                    }
                }

                // When the user is scrolled up into history, compensate for new
                // lines pushed into the scrollback so the viewport stays anchored
                // to the same content (instead of drifting toward newer output).
                if self.scroll_pixels > 0.0 {
                    let added = self.terminal.scrollback.len().saturating_sub(sb_before);
                    if added > 0 {
                        self.scroll_pixels += added as f64 * self.cell_height();
                        // Re-clamp so we don't exceed the new maximum.
                        let max_lines = TerminalRenderer::max_scroll(&self.terminal);
                        let max_px = max_lines as f64 * self.cell_height();
                        self.scroll_pixels = self.scroll_pixels.min(max_px);
                    }
                }

                // Drain AI layer updates (replace local primitives with daemon state)
                let mut latest_primitives: Option<Vec<AiPrimitive>> = None;
                loop {
                    match self.ai_layer_rx.try_recv() {
                        Ok(primitives) => {
                            // Keep only the latest update (skip intermediate frames)
                            latest_primitives = Some(primitives);
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => break,
                    }
                }
                if let Some(primitives) = latest_primitives {
                    self.terminal.ai_layer.primitives = primitives;
                }

                // Daemon disconnected — close the window
                if daemon_disconnected {
                    event_loop.exit();
                    return;
                }

                // Get scroll offset before borrowing gpu mutably.
                let scroll_offset = self.scroll_lines();

                // Compute these before borrowing gpu mutably (they only need &self).
                let ai_stats = self.format_ai_stats();
                let popup_data = self.popup_render_data();

                if let Some(gpu) = &mut self.gpu {
                    let frame = match gpu.surface.get_current_texture() {
                        Ok(f) => f,
                        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                            gpu.surface.configure(&gpu.device, &gpu.config);
                            return;
                        }
                        Err(e) => {
                            tracing::error!("Surface error: {e}");
                            return;
                        }
                    };

                    let view = frame
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default());

                    let sels: Vec<Selection> = if self.selection.is_active {
                        vec![self.selection.clone()]
                    } else {
                        vec![]
                    };

                    let cols = self.terminal.cols();
                    let time_secs = self.created_at.elapsed().as_secs_f32();

                    // Extract title from terminal or shell name
                    let title = if self.terminal.title.is_empty() {
                        self.shell.clone()
                    } else {
                        self.terminal.title.clone()
                    };

                    // Use daemon's project name (from SCREEN_PROJECT_DIR), fall back to extraction
                    let project = if !self.project.is_empty() {
                        self.project.clone()
                    } else {
                        extract_project_name(&title, &self.shell)
                    };

                    // Animation + activity
                    let dot = immorterm_render::statusbar::animated_dot_char(time_secs);
                    let last_active = format_last_active(self.last_io_time);

                    // Claude stats from background poller (mode-aware)
                    // (ai_stats computed above, before gpu borrow)

                    // Compute marquee elapsed time (title_marquee_time stores start time)
                    if self.title_marquee_time == 0.0 {
                        self.title_marquee_time = time_secs;
                    }
                    let marquee_elapsed = time_secs - self.title_marquee_time;

                    // Compute marquee offset for long title LED-sign scroll
                    let (title_scroll, title_scroll_fract) = if self.status_bar_hover == StatusBarTarget::Title {
                        (usize::MAX, 0.0) // hover: expand full title
                    } else {
                        // Compute overflow once and cache — avoids marquee restart when ai_stats toggles
                        if self.title_marquee_overflow == 0 {
                            let probe = statusbar::build_sections_with_theme(
                                &project, &title, &ai_stats, &last_active, dot, cols,
                                0.0, &self.status_bar_theme, 0, 0.0,
                            );
                            if probe.title_truncated {
                                let full_len = probe.full_title.chars().count();
                                let display_len = probe.left_sections.get(2).map_or(0, |s| s.text.chars().count());
                                self.title_marquee_overflow = full_len.saturating_sub(display_len);
                            }
                        }
                        if self.title_marquee_overflow > 0 {
                            let ms = statusbar::marquee_offset(marquee_elapsed, self.title_marquee_overflow);
                            (ms.char_offset, ms.fract)
                        } else {
                            (0, 0.0)
                        }
                    };

                    // Build status bar matching the C version's layout
                    let mut status_bar_data = statusbar::build_sections_with_theme(
                        &project,
                        &title,
                        &ai_stats,
                        &last_active,
                        dot,
                        cols,
                        0.0, // no CTX bar in native window
                        &self.status_bar_theme,
                        title_scroll,
                        title_scroll_fract,
                    );
                    status_bar_data.hovered_target = self.status_bar_hover;
                    self.last_status_bar_cols = cols;

                    // (popup_data computed above, before gpu borrow)

                    let opts = RenderOptions {
                        scroll_offset,
                        selections: &sels,
                        pseudo_selections: &[],
                        status_bar: Some(&status_bar_data),
                        popup: popup_data.as_ref(),
                        pane: None,
                        clear: true,
                    };

                    gpu.renderer
                        .render(&gpu.device, &gpu.queue, &view, &mut self.terminal, &opts);
                    frame.present();
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Focused: 60 FPS for smooth animations. Unfocused: 1 FPS to save resources.
        let interval = if self.focused { 16 } else { 1000 };
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(interval),
        ));
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

// ─── Daemon Connection ──────────────────────────────────────────────────

/// Read a JSON response line from a Unix stream (blocks until newline or EOF).
fn read_json_response(stream: &mut UnixStream) -> Result<Response> {
    let mut buf = vec![0u8; 8192];
    let mut total = 0;
    loop {
        let n = stream.read(&mut buf[total..]).context("Read response")?;
        if n == 0 {
            anyhow::bail!("Connection closed before response");
        }
        total += n;
        // Try to parse what we have — the daemon sends a single JSON blob
        if let Ok(resp) = serde_json::from_slice::<Response>(&buf[..total]) {
            return Ok(resp);
        }
        if total >= buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
    }
}

/// Connect to a daemon session, establishing output, input, and AI layer streams.
///
/// Returns: (output_rx, ai_layer_rx, input_writer, project, title, cols, rows)
fn connect_to_daemon(socket_path: &Path) -> Result<(
    mpsc::Receiver<Vec<u8>>,
    mpsc::Receiver<Vec<AiPrimitive>>,
    InputWriter,
    String, // project
    String, // title
    usize,  // cols
    usize,  // rows
)> {
    // 1. Output connection — subscribes to PTY output stream
    let mut out_sock = UnixStream::connect(socket_path)
        .context("Failed to connect to daemon for output")?;
    let req = serde_json::to_vec(&Request::SubscribeOutput)?;
    out_sock.write_all(&req)?;
    out_sock.flush()?;

    // Read initial Subscribed response
    let resp = read_json_response(&mut out_sock)?;
    let (project, title, cols, rows) = match resp {
        Response::Subscribed { cols, rows, title, project } => (project, title, cols, rows),
        Response::Error(e) => anyhow::bail!("Daemon refused output subscription: {}", e),
        _ => anyhow::bail!("Unexpected response to SubscribeOutput: {:?}", resp),
    };

    // Spawn output reader thread: reads length-prefixed chunks → mpsc
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut len_buf = [0u8; 4];
        loop {
            if out_sock.read_exact(&mut len_buf).is_err() { break; }
            let len = u32::from_be_bytes(len_buf) as usize;
            if len == 0 { continue; }
            let mut data = vec![0u8; len];
            if out_sock.read_exact(&mut data).is_err() { break; }
            if tx.send(data).is_err() { break; }
        }
    });

    // 2. Input connection — sends keyboard bytes to daemon's PTY
    let mut in_sock = UnixStream::connect(socket_path)
        .context("Failed to connect to daemon for input")?;
    let req = serde_json::to_vec(&Request::SubscribeInput)?;
    in_sock.write_all(&req)?;
    in_sock.flush()?;

    // Consume the Subscribed response
    let _ = read_json_response(&mut in_sock)?;

    let input_writer = Arc::new(Mutex::new(in_sock));

    // 3. AI layer connection — subscribes to AI canvas state updates
    let mut ai_sock = UnixStream::connect(socket_path)
        .context("Failed to connect to daemon for AI layer")?;
    let req = serde_json::to_vec(&Request::SubscribeAiLayer)?;
    ai_sock.write_all(&req)?;
    ai_sock.flush()?;

    // Consume the Subscribed response
    let _ = read_json_response(&mut ai_sock)?;

    // Spawn AI layer reader thread: reads length-prefixed JSON → Vec<AiPrimitive>
    let (ai_tx, ai_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut len_buf = [0u8; 4];
        loop {
            if ai_sock.read_exact(&mut len_buf).is_err() { break; }
            let len = u32::from_be_bytes(len_buf) as usize;
            if len == 0 { continue; }
            let mut data = vec![0u8; len];
            if ai_sock.read_exact(&mut data).is_err() { break; }
            // Parse JSON into Vec<AiPrimitive>
            if let Ok(primitives) = serde_json::from_slice::<Vec<AiPrimitive>>(&data)
                && ai_tx.send(primitives).is_err() { break; }
        }
    });

    Ok((rx, ai_rx, input_writer, project, title, cols, rows))
}

/// Send a fire-and-forget control request to the daemon (Resize, etc.).
fn send_control_request(socket_path: &Path, request: Request) {
    if let Ok(mut sock) = UnixStream::connect(socket_path) {
        sock.set_write_timeout(Some(Duration::from_millis(200))).ok();
        if let Ok(json) = serde_json::to_vec(&request) {
            let _ = sock.write_all(&json);
            let _ = sock.flush();
        }
    }
}

/// Query the daemon for Claude session info and update the shared snapshot.
fn poll_claude_stats(socket_path: PathBuf, stats: Arc<Mutex<ClaudeStatsSnapshot>>) {
    loop {
        std::thread::sleep(Duration::from_secs(5));

        let info = match query_daemon_claude_info(&socket_path) {
            Some(info) => info,
            None => continue,
        };

        let mut s = stats.lock().unwrap();
        s.active = info.active;
        s.model = info.model.unwrap_or_default();
        s.cost_usd = info.cost_usd.unwrap_or(0.0);
        s.context_pct = info.context_pct.unwrap_or(0.0);
        s.tool = info.tool.unwrap_or_default();
    }
}

/// Parsed ClaudeInfo fields — avoids importing the full Response enum in the thread.
struct ClaudeInfoData {
    active: bool,
    model: Option<String>,
    cost_usd: Option<f64>,
    context_pct: Option<f64>,
    tool: Option<String>,
}

/// Title-case a vendor identifier for status bar display.
/// Empty input → empty output. "claude" → "Claude", "opencode" → "Opencode".
fn title_case_vendor(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Send GetClaudeInfo to daemon and parse the response.
fn query_daemon_claude_info(socket_path: &Path) -> Option<ClaudeInfoData> {
    let mut sock = UnixStream::connect(socket_path).ok()?;
    sock.set_read_timeout(Some(Duration::from_millis(500))).ok();
    sock.set_write_timeout(Some(Duration::from_millis(200))).ok();

    let req = serde_json::to_vec(&Request::GetClaudeInfo).ok()?;
    sock.write_all(&req).ok()?;
    sock.flush().ok()?;

    let resp = read_json_response(&mut sock).ok()?;
    match resp {
        Response::ClaudeInfo {
            active,
            model,
            cost_usd,
            context_pct,
            tool,
            ..
        } => Some(ClaudeInfoData {
            active,
            model,
            cost_usd,
            context_pct,
            tool,
        }),
        _ => None,
    }
}

// ─── Entry Point ────────────────────────────────────────────────────────

/// Launch the GUI terminal window, connected to a daemon session.
///
/// If `session_name` is None, generates a name like "gui-<pid>".
/// If no daemon exists for the session, spawns one automatically.
pub fn main_gui(session_name: Option<&str>, shell: &str) -> Result<()> {
    let name = session_name
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("gui-{}", std::process::id()));

    // Try to find existing daemon socket, or spawn a new daemon
    let socket_path = match crate::commands::find_session_socket_sync(&name) {
        Ok(path) => path,
        Err(_) => {
            // No daemon running — spawn one
            eprintln!("Spawning daemon session '{}'...", name);
            crate::daemon::create_session(&name, shell, 50_000, None, false, None)?;

            // Wait for socket to appear (daemon double-forks, needs time)
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Ok(path) = crate::commands::find_session_socket_sync(&name) {
                    break path;
                }
                if Instant::now() > deadline {
                    anyhow::bail!("Daemon socket for '{}' didn't appear within 3 seconds", name);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };

    // Connect to daemon
    let (output_rx, ai_layer_rx, input_writer, project, _title, cols, rows) =
        connect_to_daemon(&socket_path)?;

    // Spawn Claude stats poller (every 5 seconds)
    let claude_stats = Arc::new(Mutex::new(ClaudeStatsSnapshot::default()));
    {
        let stats_clone = Arc::clone(&claude_stats);
        let socket_clone = socket_path.clone();
        std::thread::spawn(move || {
            poll_claude_stats(socket_clone, stats_clone);
        });
    }

    // Build terminal and app
    let now = Instant::now();
    let mut terminal = Terminal::new(cols, rows);
    terminal.set_scrollback(50_000);

    let mut app = App {
        window: None,
        gpu: None,
        terminal,
        output_rx,
        ai_layer_rx,
        input_writer,
        socket_path,
        session_name: name,
        project,
        shell: shell.to_string(),
        created_at: now,
        last_io_time: now,
        claude_stats,
        status_bar_hover: StatusBarTarget::None,
        status_bar_theme: StatusBarTheme::default(),
        ai_stats_mode: AiStatsMode::default(),
        popup: None,
        last_status_bar_cols: 0,
        title_marquee_time: 0.0,
        title_marquee_overflow: 0,
        selection: Selection::default(),
        mouse_down: false,
        mouse_pos: (0.0, 0.0),
        scroll_pixels: 0.0,
        modifiers: ModifiersState::empty(),
        focused: true,
    };

    let event_loop = EventLoop::new().context("Failed to create event loop")?;
    // Cap at ~60 FPS instead of spinning as fast as possible.
    // Poll burns 100% of a CPU core; WaitUntil sleeps between frames.
    event_loop.set_control_flow(ControlFlow::WaitUntil(
        Instant::now() + Duration::from_millis(16),
    ));

    event_loop
        .run_app(&mut app)
        .context("Event loop error")?;

    Ok(())
}

/// Extract a short project name from the terminal title.
/// If the title looks like a path, returns the last component.
/// Otherwise returns the shell name as a fallback.
fn extract_project_name(title: &str, shell: &str) -> String {
    // If title is a path like "/Users/foo/Development/project" or "~/project"
    let trimmed = title.trim();
    if trimmed.contains('/')
        && let Some(last) = trimmed.rsplit('/').next()
            && !last.is_empty() {
                return last.to_string();
            }
    // If title contains ":" (e.g. "user@host: ~/dir"), extract dir part
    if let Some(after_colon) = trimmed.split(':').next_back() {
        let path_part = after_colon.trim();
        if path_part.contains('/')
            && let Some(last) = path_part.rsplit('/').next()
                && !last.is_empty() {
                    return last.to_string();
                }
    }
    // Fallback: shell basename (e.g. "bash" from "/bin/bash")
    shell
        .rsplit('/')
        .next()
        .unwrap_or(shell)
        .to_string()
}

/// Format last activity as DD/MM HH:MM timestamp, matching the C version's `%I`.
fn format_last_active(last_io: Instant) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Convert Instant → wall-clock: SystemTime::now() minus elapsed since last_io
    let elapsed = last_io.elapsed();
    let wall_time = SystemTime::now() - elapsed;
    let secs = wall_time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;

    // localtime via nix::libc (already a dependency)
    let mut tm: nix::libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_isdst = -1;
    unsafe { nix::libc::localtime_r(&secs, &mut tm); }

    format!(
        "{:02}/{:02} {:02}:{:02}",
        tm.tm_mday,
        tm.tm_mon + 1,
        tm.tm_hour,
        tm.tm_min,
    )
}
