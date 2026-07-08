//! Multi-pane native GUI for Claude Code Agent Teams.
//!
//! `immorterm-ai team-view [team-name]` opens a window with one GPU-rendered
//! terminal pane per team member. Each pane connects to its daemon session for
//! live output. Tab cycles focus between panes; mouse click selects.
//!
//! Architecture:
//!   1. Discover team from `~/.claude/teams/`
//!   2. For each non-lead member, find their daemon session socket
//!   3. Connect to each daemon (output + input streams)
//!   4. PaneLayout auto-arranges panes on the GPU surface
//!   5. Each frame: drain output for all panes, render each with PaneRegion
//!
//! The team lead is excluded from panes (it runs in the main terminal).

use std::io::{Read as IoRead, Write as IoWrite};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use immorterm_core::subagent::{SubagentEvent, SubagentInfo, SubagentStatus};
use immorterm_core::team::{TeamLifecycle, TeamState};
use immorterm_core::Terminal;
use immorterm_render::panes::{PaneLayout, PANE_BORDER};
use immorterm_render::renderer::{PaneChrome, PaneRegion, RenderOptions};
use immorterm_render::TerminalRenderer;
use tracing::{debug, info, warn};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::ipc::{Request, Response};
use crate::team_watcher::TeamStateChange;
use crate::websocket::TeamEvent;

// ─── User Events (async → winit bridge) ─────────────────────────────────

/// Custom events forwarded from background watchers to the winit event loop.
/// The tokio runtime subscribes to team/subagent watcher broadcasts and sends
/// these via `EventLoopProxy::send_event()` to wake the GUI thread.
#[derive(Debug)]
enum TeamUserEvent {
    /// Team file watcher detected a change (tasks, inboxes, config, lifecycle).
    TeamStateChanged(Box<TeamStateChange>),
    /// Subagent JSONL watcher detected a new/updated subagent transcript.
    SubagentEvent(SubagentEvent),
}

/// Connection state for a pane — tracks disconnection and retry backoff.
#[derive(Debug)]
enum PaneConnection {
    /// Live output stream from daemon.
    Connected,
    /// Socket disconnected — will retry with exponential backoff.
    Disconnected {
        since: std::time::Instant,
        next_retry: std::time::Instant,
        retries: u32,
    },
    /// In-process member (no terminal backend). Shows dashboard content.
    InProcess,
}

impl PaneConnection {
    /// Maximum backoff between retries (30 seconds).
    const MAX_BACKOFF_SECS: u64 = 30;

    fn new_disconnected() -> Self {
        let now = std::time::Instant::now();
        Self::Disconnected {
            since: now,
            next_retry: now + Duration::from_secs(1),
            retries: 0,
        }
    }

    /// Advance to the next retry with exponential backoff (1s, 2s, 4s, 8s... cap 30s).
    fn bump_retry(&mut self) {
        if let Self::Disconnected {
            next_retry,
            retries,
            ..
        } = self
        {
            *retries += 1;
            let delay = Duration::from_secs(
                (1u64 << (*retries).min(5)).min(Self::MAX_BACKOFF_SECS),
            );
            *next_retry = std::time::Instant::now() + delay;
        }
    }

    fn is_disconnected(&self) -> bool {
        matches!(self, Self::Disconnected { .. })
    }

    fn should_retry(&self) -> bool {
        if let Self::Disconnected { next_retry, .. } = self {
            std::time::Instant::now() >= *next_retry
        } else {
            false
        }
    }
}

/// A single team member's terminal pane state.
struct MemberPane {
    /// Member display name.
    name: String,
    /// Accent color [r, g, b, a].
    accent: [f32; 4],
    /// Terminal emulator state.
    terminal: Terminal,
    /// Receives PTY output bytes from daemon.
    output_rx: mpsc::Receiver<Vec<u8>>,
    /// Sends keyboard input to daemon.
    input_writer: Arc<Mutex<UnixStream>>,
    /// Daemon socket path (for resize commands).
    socket_path: PathBuf,
    /// Scroll offset (lines from bottom).
    scroll_offset: usize,
    /// Accumulated scroll pixels (for smooth scrolling).
    scroll_pixels: f64,
    /// Connection state (connected, disconnected with retry, or in-process).
    connection: PaneConnection,
}

struct TeamGpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: TerminalRenderer,
}

struct TeamApp {
    window: Option<Arc<Window>>,
    gpu: Option<TeamGpuState>,

    /// Team name for the window title.
    team_name: String,
    /// One pane per non-lead team member.
    panes: Vec<MemberPane>,
    /// Computed layout (updated on resize).
    layout: PaneLayout,

    /// Keyboard modifier state.
    modifiers: ModifiersState,
    /// Mouse position for click-to-focus.
    mouse_pos: (f64, f64),

    // ── Live state from background watchers ──

    /// Latest team state from the file watcher (tasks, members, lifecycle).
    team_state: Option<TeamState>,
    /// Detected subagents for the current Claude session.
    subagents: Vec<SubagentInfo>,
    /// Whether this window is focused (unfocused windows render at 1 FPS).
    focused: bool,
}

// ─── Daemon Connection ──────────────────────────────────────────────────

/// Connect to a daemon session for output + input (no AI layer needed for team view).
///
/// Returns (output_rx, input_writer, cols, rows).
fn connect_team_member(socket_path: &Path) -> Result<(
    mpsc::Receiver<Vec<u8>>,
    Arc<Mutex<UnixStream>>,
    usize, // cols
    usize, // rows
)> {
    // 1. Output subscription
    let mut out_sock = UnixStream::connect(socket_path)
        .context("Connect to daemon for output")?;
    let req = serde_json::to_vec(&Request::SubscribeOutput)?;
    out_sock.write_all(&req)?;
    out_sock.flush()?;

    let resp = read_json_response(&mut out_sock)?;
    let (cols, rows) = match resp {
        Response::Subscribed { cols, rows, .. } => (cols, rows),
        Response::Error(e) => anyhow::bail!("Daemon refused subscription: {}", e),
        _ => anyhow::bail!("Unexpected response: {:?}", resp),
    };

    // Spawn output reader thread
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

    // 2. Input subscription
    let mut in_sock = UnixStream::connect(socket_path)
        .context("Connect to daemon for input")?;
    let req = serde_json::to_vec(&Request::SubscribeInput)?;
    in_sock.write_all(&req)?;
    in_sock.flush()?;
    let _ = read_json_response(&mut in_sock)?;

    let input_writer = Arc::new(Mutex::new(in_sock));

    Ok((rx, input_writer, cols, rows))
}

/// Read a JSON response from a Unix stream.
fn read_json_response(stream: &mut UnixStream) -> Result<Response> {
    let mut buf = vec![0u8; 8192];
    let mut total = 0;
    loop {
        let n = stream.read(&mut buf[total..]).context("Read response")?;
        if n == 0 {
            anyhow::bail!("Connection closed before response");
        }
        total += n;
        if let Ok(resp) = serde_json::from_slice::<Response>(&buf[..total]) {
            return Ok(resp);
        }
        if total >= buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
    }
}

/// Send a fire-and-forget control request (e.g., Resize) to a daemon.
fn send_control(socket_path: &Path, request: Request) {
    if let Ok(mut sock) = UnixStream::connect(socket_path) {
        sock.set_write_timeout(Some(Duration::from_millis(200))).ok();
        if let Ok(json) = serde_json::to_vec(&request) {
            let _ = sock.write_all(&json);
            let _ = sock.flush();
        }
    }
}

// ─── TeamApp Implementation ─────────────────────────────────────────────

impl TeamApp {
    /// Drain output from all pane receivers and feed into their terminals.
    /// Detects channel disconnection and marks the pane for reconnection.
    fn drain_output(&mut self) {
        for pane in &mut self.panes {
            loop {
                match pane.output_rx.try_recv() {
                    Ok(data) => {
                        pane.terminal.process(&data);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // Channel closed — daemon connection lost
                        if matches!(pane.connection, PaneConnection::Connected) {
                            warn!("Output channel disconnected for pane '{}'", pane.name);
                            pane.connection = PaneConnection::new_disconnected();
                            let msg = "\r\n\x1b[31m  Connection lost — retrying...\x1b[0m\r\n".to_string();
                            pane.terminal.process(msg.as_bytes());
                        }
                        break;
                    }
                }
            }
        }
    }

    /// Send raw bytes to the focused pane's daemon.
    fn write_to_focused(&self, data: &[u8]) {
        if let Some(pane) = self.panes.get(self.layout.focused_index)
            && let Ok(mut writer) = pane.input_writer.lock() {
                let _ = writer.write_all(data);
                let _ = writer.flush();
            }
    }

    /// Recompute pane layout after window resize and resize each pane's PTY.
    fn relayout(&mut self, surface_w: u32, surface_h: u32) {
        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };

        let (cell_w, cell_h) = gpu.renderer.cell_metrics();

        let labels: Vec<(String, [f32; 4])> = self.panes
            .iter()
            .map(|p| (p.name.clone(), p.accent))
            .collect();

        self.layout = PaneLayout::auto_arrange(
            &labels,
            surface_w as f32,
            surface_h as f32,
            cell_w,
            cell_h,
        );

        // Resize each pane's terminal and daemon PTY
        for (i, pane_rect) in self.layout.panes.iter().enumerate() {
            if let Some(pane) = self.panes.get_mut(i) {
                let cols = pane_rect.cols.max(1);
                let rows = pane_rect.rows.max(1);
                pane.terminal.resize(cols, rows);
                send_control(&pane.socket_path, Request::Resize {
                    cols: cols as u16,
                    rows: rows as u16,
                });
            }
        }
    }

    /// Get cell height from the renderer (for scroll calculations).
    fn cell_height(&self) -> f64 {
        self.gpu.as_ref()
            .map(|g| g.renderer.cell_metrics().1 as f64)
            .unwrap_or(16.0)
    }

    /// Scroll the focused pane by pixel amount.
    fn scroll_focused(&mut self, px: f64) {
        let ch = self.cell_height();
        if let Some(pane) = self.panes.get_mut(self.layout.focused_index) {
            pane.scroll_pixels += px;
            let lines = (pane.scroll_pixels / ch).round() as isize;
            if lines != 0 {
                pane.scroll_pixels -= lines as f64 * ch;
                let max_scroll = pane.terminal.scrollback.len();
                let new_offset = (pane.scroll_offset as isize + lines).clamp(0, max_scroll as isize);
                pane.scroll_offset = new_offset as usize;
            }
        }
    }

    /// Compute xterm modifier parameter for CSI sequences.
    fn xterm_modifier(shift: bool, alt: bool, ctrl: bool) -> Option<u8> {
        let val = 1
            + if shift { 1 } else { 0 }
            + if alt { 2 } else { 0 }
            + if ctrl { 4 } else { 0 };
        if val > 1 { Some(val) } else { None }
    }

    // ── Dynamic Pane Management ──────────────────────────────────────────

    /// Handle a user event forwarded from background watchers.
    fn handle_user_event(&mut self, event: TeamUserEvent) {
        match event {
            TeamUserEvent::TeamStateChanged(change) => {
                let old_lifecycle = self.team_state.as_ref().map(|s| s.lifecycle);
                self.team_state = Some(change.state.clone());

                // Process granular events
                let mut needs_relayout = false;
                for team_event in &change.events {
                    match team_event {
                        TeamEvent::MemberJoined { name } => {
                            info!("Team member joined: {}", name);
                            if !self.panes.iter().any(|p| p.name == *name) {
                                self.add_member_pane(name, &change.state);
                                needs_relayout = true;
                            }
                        }
                        TeamEvent::MemberLeft { name } => {
                            info!("Team member left: {}", name);
                            if self.remove_member_pane(name) {
                                needs_relayout = true;
                            }
                        }
                        TeamEvent::LifecycleChanged { old, new } => {
                            info!("Team lifecycle: {:?} → {:?}", old, new);
                        }
                        _ => {}
                    }
                }

                // Update window title if lifecycle changed
                if old_lifecycle != Some(change.state.lifecycle) {
                    self.update_window_title();
                }

                // Relayout if panes changed
                if needs_relayout
                    && let Some(gpu) = &self.gpu {
                        let w = gpu.config.width;
                        let h = gpu.config.height;
                        self.relayout(w, h);
                    }

                // Refresh in-process dashboard panes with latest team state
                self.refresh_dashboard_panes(&change.state);
            }

            TeamUserEvent::SubagentEvent(event) => {
                match event {
                    SubagentEvent::Detected(info) => {
                        debug!("Subagent detected: {} ({})", info.agent_id, info.slug);
                        if !self.subagents.iter().any(|a| a.agent_id == info.agent_id) {
                            self.subagents.push(info);
                        }
                    }
                    SubagentEvent::Updated(info) => {
                        if let Some(existing) = self
                            .subagents
                            .iter_mut()
                            .find(|a| a.agent_id == info.agent_id)
                        {
                            *existing = info;
                        }
                    }
                    SubagentEvent::Completed(id) => {
                        if let Some(a) = self.subagents.iter_mut().find(|a| a.agent_id == id) {
                            a.status = SubagentStatus::Completed;
                        }
                    }
                    SubagentEvent::NewTranscriptLine { .. } => {
                        // Transcript line streaming — used in Step 5 for transcript panes
                    }
                }
            }
        }
    }

    /// Add a pane for a newly joined team member.
    fn add_member_pane(&mut self, name: &str, team_state: &TeamState) {
        // Find member config for accent color
        let member = team_state
            .config
            .members
            .iter()
            .find(|m| m.name == name);
        let accent = member.map(|m| m.color_rgba()).unwrap_or([0.7, 0.7, 0.7, 1.0]);

        // Try to connect to their daemon session
        let socket_path = find_member_socket(name, &self.team_name);
        match socket_path {
            Some(path) => {
                info!("Connecting to new member {} @ {:?}", name, path);
                match connect_team_member(&path) {
                    Ok((output_rx, input_writer, cols, rows)) => {
                        let mut terminal = Terminal::new(cols, rows);
                        terminal.set_scrollback(10_000);
                        self.panes.push(MemberPane {
                            name: name.to_string(),
                            accent,
                            terminal,
                            output_rx,
                            input_writer,
                            socket_path: path,
                            scroll_offset: 0,
                            scroll_pixels: 0.0,
                            connection: PaneConnection::Connected,
                        });
                    }
                    Err(e) => {
                        warn!("Failed to connect to {}: {}", name, e);
                        self.panes
                            .push(make_placeholder_pane(name, accent, Some(path)));
                    }
                }
            }
            None => {
                info!("No daemon socket for {} — creating in-process pane", name);
                self.panes.push(make_placeholder_pane(name, accent, None));
            }
        }
    }

    /// Remove a pane for a member who left. Returns true if a pane was removed.
    fn remove_member_pane(&mut self, name: &str) -> bool {
        let before = self.panes.len();
        self.panes.retain(|p| p.name != name);
        let removed = self.panes.len() < before;

        // Adjust focus if it's now out of bounds
        if removed && !self.panes.is_empty()
            && self.layout.focused_index >= self.panes.len() {
                self.layout.set_focus(self.panes.len() - 1);
            }

        removed
    }

    /// Refresh in-process dashboard panes with synthetic content showing
    /// member status, current task, and recent messages from team state.
    fn refresh_dashboard_panes(&mut self, state: &TeamState) {
        for pane in &mut self.panes {
            if !matches!(pane.connection, PaneConnection::InProcess) {
                continue;
            }

            // Clear terminal and write dashboard content
            pane.terminal.process(b"\x1b[2J\x1b[H"); // clear + home

            let header = format!(
                "\x1b[1;36m[{}]\x1b[0m  In-process mode\r\n\r\n",
                pane.name
            );
            pane.terminal.process(header.as_bytes());

            // Show member status
            let status = state
                .member_status
                .get(&pane.name)
                .map(|s| s.display_label())
                .unwrap_or("Unknown");
            let line = format!("  Status: \x1b[1m{}\x1b[0m\r\n", status);
            pane.terminal.process(line.as_bytes());

            // Show current task (if any)
            let current_task = state.tasks.iter().find(|t| {
                t.owner.as_deref() == Some(&pane.name)
                    && t.status == immorterm_core::team::TaskStatus::InProgress
            });
            if let Some(task) = current_task {
                let line = format!("  Task:   \x1b[33m{}\x1b[0m\r\n", task.subject);
                pane.terminal.process(line.as_bytes());
            } else {
                pane.terminal.process(b"  Task:   \x1b[2m(none)\x1b[0m\r\n");
            }

            // Show recent messages (last 5 from inbox)
            if let Some(messages) = state.inboxes.get(&pane.name) {
                let recent: Vec<_> = messages.iter().rev().take(5).collect();
                if !recent.is_empty() {
                    pane.terminal
                        .process(b"\r\n  \x1b[1mRecent messages:\x1b[0m\r\n");
                    for msg in recent.iter().rev() {
                        let preview: String = msg.text.chars().take(60).collect();
                        let line = format!(
                            "    \x1b[2m{}\x1b[0m: {}\r\n",
                            msg.from, preview
                        );
                        pane.terminal.process(line.as_bytes());
                    }
                }
            }
        }
    }

    /// Update window title to reflect team lifecycle and agent count.
    fn update_window_title(&self) {
        let window = match &self.window {
            Some(w) => w,
            None => return,
        };

        let lifecycle_badge = self
            .team_state
            .as_ref()
            .map(|s| s.lifecycle)
            .unwrap_or(TeamLifecycle::Active);

        let permission_badge = self
            .team_state
            .as_ref()
            .map(|s| s.permission_mode)
            .unwrap_or_default();

        let mut title = format!(
            "ImmorTerm Team — {} ({} agents)",
            self.team_name,
            self.panes.len()
        );

        if permission_badge.is_delegate() {
            title = format!("[DELEGATE] {}", title);
        }

        if lifecycle_badge.is_finished() {
            title = format!("{} [{}]", title, lifecycle_badge.display_label().to_uppercase());
        }

        window.set_title(&title);
    }
}

impl ApplicationHandler<TeamUserEvent> for TeamApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let n_panes = self.panes.len();
        let title = format!("ImmorTerm Team — {} ({} agents)", self.team_name, n_panes);
        let attrs = WindowAttributes::default()
            .with_title(&title)
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 800));

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("Failed to create window"),
        );

        // Init GPU
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
                label: Some("team_window"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
            },
            None,
        ))
        .expect("Failed to create GPU device");

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
        renderer.status_bar_enabled = false; // No status bar in team view

        self.gpu = Some(TeamGpuState {
            surface,
            device,
            queue,
            config,
            renderer,
        });

        // Compute initial layout
        self.relayout(size.width, size.height);

        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::Focused(focused) => {
                self.focused = focused;
            }
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.config.width = size.width.max(1);
                    gpu.config.height = size.height.max(1);
                    gpu.surface.configure(&gpu.device, &gpu.config);
                }
                self.relayout(size.width.max(1), size.height.max(1));
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let px = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64 * self.cell_height(),
                    MouseScrollDelta::PixelDelta(pos) => pos.y,
                };
                self.scroll_focused(px);
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_pos = (position.x, position.y);
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Click to focus a pane
                let (mx, my) = self.mouse_pos;
                if let Some(idx) = self.layout.pane_at(mx as f32, my as f32) {
                    self.layout.set_focus(idx);
                    // Reset scroll on the newly focused pane if clicked
                    if let Some(pane) = self.panes.get_mut(idx) {
                        pane.scroll_offset = 0;
                        pane.scroll_pixels = 0.0;
                    }
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                let shift = self.modifiers.shift_key();
                let ctrl = self.modifiers.control_key();
                let alt = self.modifiers.alt_key();

                // Tab = cycle pane focus
                if event.logical_key == Key::Named(NamedKey::Tab) && !ctrl && !alt {
                    self.layout.cycle_focus();
                    return;
                }

                // Ctrl+Shift+Tab = reverse cycle
                if event.logical_key == Key::Named(NamedKey::Tab) && ctrl && shift {
                    let n = self.panes.len();
                    if n > 0 {
                        let idx = if self.layout.focused_index == 0 {
                            n - 1
                        } else {
                            self.layout.focused_index - 1
                        };
                        self.layout.set_focus(idx);
                    }
                    return;
                }

                // Ctrl+1..9 = jump to pane
                if ctrl
                    && let Key::Character(ch) = &event.logical_key
                        && let Ok(num) = ch.parse::<usize>()
                            && num >= 1 && num <= self.panes.len() {
                                self.layout.set_focus(num - 1);
                                return;
                            }

                // Forward keyboard input to focused pane's daemon
                let modifier_param = Self::xterm_modifier(shift, alt, ctrl);

                match &event.logical_key {
                    Key::Named(named) => {
                        let seq: Option<Vec<u8>> = match named {
                            NamedKey::Enter => Some(b"\r".to_vec()),
                            NamedKey::Backspace => {
                                if alt {
                                    Some(b"\x1b\x7f".to_vec())
                                } else {
                                    Some(b"\x7f".to_vec())
                                }
                            }
                            NamedKey::Escape => Some(b"\x1b".to_vec()),
                            NamedKey::Tab => Some(b"\t".to_vec()),
                            NamedKey::ArrowUp => Some(csi_arrow(b'A', modifier_param)),
                            NamedKey::ArrowDown => Some(csi_arrow(b'B', modifier_param)),
                            NamedKey::ArrowRight => Some(csi_arrow(b'C', modifier_param)),
                            NamedKey::ArrowLeft => Some(csi_arrow(b'D', modifier_param)),
                            NamedKey::Home => Some(csi_tilde(1, modifier_param)),
                            NamedKey::End => Some(csi_tilde(4, modifier_param)),
                            NamedKey::PageUp => {
                                if shift {
                                    // Shift+PageUp = scroll back
                                    self.scroll_focused(self.cell_height() * 24.0);
                                    None
                                } else {
                                    Some(csi_tilde(5, modifier_param))
                                }
                            }
                            NamedKey::PageDown => {
                                if shift {
                                    self.scroll_focused(-self.cell_height() * 24.0);
                                    None
                                } else {
                                    Some(csi_tilde(6, modifier_param))
                                }
                            }
                            NamedKey::Insert => Some(csi_tilde(2, modifier_param)),
                            NamedKey::Delete => Some(csi_tilde(3, modifier_param)),
                            NamedKey::F1 => Some(ss3_or_csi(b'P', 11, modifier_param)),
                            NamedKey::F2 => Some(ss3_or_csi(b'Q', 12, modifier_param)),
                            NamedKey::F3 => Some(ss3_or_csi(b'R', 13, modifier_param)),
                            NamedKey::F4 => Some(ss3_or_csi(b'S', 14, modifier_param)),
                            NamedKey::F5 => Some(csi_tilde(15, modifier_param)),
                            NamedKey::F6 => Some(csi_tilde(17, modifier_param)),
                            NamedKey::F7 => Some(csi_tilde(18, modifier_param)),
                            NamedKey::F8 => Some(csi_tilde(19, modifier_param)),
                            NamedKey::F9 => Some(csi_tilde(20, modifier_param)),
                            NamedKey::F10 => Some(csi_tilde(21, modifier_param)),
                            NamedKey::F11 => Some(csi_tilde(23, modifier_param)),
                            NamedKey::F12 => Some(csi_tilde(24, modifier_param)),
                            _ => None,
                        };
                        if let Some(bytes) = seq {
                            // Auto snap-back on keyboard input
                            if let Some(pane) = self.panes.get_mut(self.layout.focused_index) {
                                pane.scroll_offset = 0;
                            }
                            self.write_to_focused(&bytes);
                        }
                    }
                    Key::Character(ch) => {
                        // Auto snap-back
                        if let Some(pane) = self.panes.get_mut(self.layout.focused_index) {
                            pane.scroll_offset = 0;
                        }

                        if ctrl {
                            // Ctrl+letter → control character
                            if let Some(c) = ch.chars().next() {
                                let ctrl_code = (c as u8).wrapping_sub(b'a').wrapping_add(1);
                                if ctrl_code <= 26 {
                                    if alt {
                                        self.write_to_focused(&[0x1b, ctrl_code]);
                                    } else {
                                        self.write_to_focused(&[ctrl_code]);
                                    }
                                }
                            }
                        } else if alt {
                            // Alt+key → ESC prefix
                            let bytes = ch.as_bytes();
                            let mut buf = Vec::with_capacity(1 + bytes.len());
                            buf.push(0x1b);
                            buf.extend_from_slice(bytes);
                            self.write_to_focused(&buf);
                        } else {
                            self.write_to_focused(ch.as_bytes());
                        }
                    }
                    _ => {}
                }
            }

            WindowEvent::RedrawRequested => {
                // Drain output from all panes
                self.drain_output();

                let gpu = match &mut self.gpu {
                    Some(g) => g,
                    None => return,
                };

                let frame = match gpu.surface.get_current_texture() {
                    Ok(f) => f,
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        gpu.surface.configure(&gpu.device, &gpu.config);
                        return;
                    }
                    Err(_) => return,
                };

                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());

                let surface_w = gpu.config.width as f32;
                let surface_h = gpu.config.height as f32;

                // Render each pane
                for (i, pane_rect) in self.layout.panes.iter().enumerate() {
                    let pane = match self.panes.get_mut(i) {
                        Some(p) => p,
                        None => continue,
                    };

                    let _is_focused = i == self.layout.focused_index;

                    // Map PaneRect → PaneRegion for the renderer.
                    // Content starts below the header.
                    let region = PaneRegion {
                        x: pane_rect.x + PANE_BORDER,
                        y: pane_rect.content_y() + PANE_BORDER,
                        width: pane_rect.width - PANE_BORDER * 2.0,
                        height: pane_rect.height - PANE_BORDER * 2.0,
                        surface_width: surface_w,
                        surface_height: surface_h,
                    };

                    let opts = RenderOptions {
                        scroll_offset: pane.scroll_offset,
                        selections: &[],
                        pseudo_selections: &[],
                        status_bar: None,
                        popup: None,
                        pane: Some(&region),
                        // Only clear on the first pane
                        clear: i == 0,
                    };

                    gpu.renderer.render(
                        &gpu.device,
                        &gpu.queue,
                        &view,
                        &mut pane.terminal,
                        &opts,
                    );

                }

                // ── Pane chrome: headers + borders (full-surface pass) ──
                let chrome: Vec<PaneChrome> = self
                    .layout
                    .panes
                    .iter()
                    .enumerate()
                    .map(|(i, pr)| {
                        let pane_name = self
                            .panes
                            .get(i)
                            .map(|p| p.name.as_str())
                            .unwrap_or("");

                        // Look up real member status from live team state
                        let status = self
                            .team_state
                            .as_ref()
                            .and_then(|s| s.member_status.get(pane_name))
                            .map(|s| s.display_label().to_string())
                            .unwrap_or_else(|| {
                                // No team state yet — check if pane is connected
                                if self.panes.get(i).map(|p| p.socket_path.as_os_str().is_empty()).unwrap_or(true) {
                                    "Disconnected".to_string()
                                } else {
                                    "Active".to_string()
                                }
                            });

                        let label = self
                            .panes
                            .get(i)
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| pr.label.clone());

                        // Build badge from team lifecycle or permission mode
                        let badge = self.team_state.as_ref().and_then(|s| {
                            if s.permission_mode.is_delegate() {
                                Some(("DELEGATE".to_string(), [0.9, 0.5, 0.1, 1.0])) // orange
                            } else if s.lifecycle.is_finished() {
                                Some((
                                    s.lifecycle.display_label().to_uppercase(),
                                    [0.4, 0.4, 0.5, 1.0], // gray
                                ))
                            } else {
                                None
                            }
                        });

                        PaneChrome {
                            x: pr.x,
                            y: pr.y,
                            width: pr.width,
                            total_height: pr.total_height(),
                            header_height: pr.header_height,
                            label,
                            status,
                            accent_color: pr.accent_color,
                            is_focused: i == self.layout.focused_index,
                            badge,
                        }
                    })
                    .collect();

                gpu.renderer.render_pane_chrome(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    &chrome,
                    surface_w,
                    surface_h,
                );

                frame.present();
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

        // Retry disconnected panes with exponential backoff
        for pane in &mut self.panes {
            if pane.connection.should_retry() {
                debug!("Retrying connection for pane '{}'...", pane.name);
                if !pane.socket_path.as_os_str().is_empty() {
                    match connect_team_member(&pane.socket_path) {
                        Ok((output_rx, input_writer, cols, rows)) => {
                            info!("Reconnected pane '{}'", pane.name);
                            pane.output_rx = output_rx;
                            pane.input_writer = input_writer;
                            pane.terminal.resize(cols, rows);
                            pane.connection = PaneConnection::Connected;
                            let msg = "\r\n\x1b[32m  Reconnected!\x1b[0m\r\n";
                            pane.terminal.process(msg.as_bytes());
                        }
                        Err(_) => {
                            pane.connection.bump_retry();
                        }
                    }
                } else {
                    // No socket path — try finding one now
                    if let Some(path) = find_member_socket(&pane.name, &self.team_name) {
                        pane.socket_path = path.clone();
                        match connect_team_member(&path) {
                            Ok((output_rx, input_writer, cols, rows)) => {
                                info!("Connected pane '{}' (found socket)", pane.name);
                                pane.output_rx = output_rx;
                                pane.input_writer = input_writer;
                                pane.terminal.resize(cols, rows);
                                pane.connection = PaneConnection::Connected;
                                let msg = "\r\n\x1b[32m  Connected!\x1b[0m\r\n";
                                pane.terminal.process(msg.as_bytes());
                            }
                            Err(_) => {
                                pane.connection.bump_retry();
                            }
                        }
                    } else {
                        pane.connection.bump_retry();
                    }
                }
            }
        }

        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: TeamUserEvent) {
        self.handle_user_event(event);
    }
}

// ─── CSI / SS3 helpers ──────────────────────────────────────────────────

fn csi_arrow(arrow: u8, modifier: Option<u8>) -> Vec<u8> {
    match modifier {
        Some(m) => format!("\x1b[1;{}{}", m, arrow as char).into_bytes(),
        None => vec![0x1b, b'[', arrow],
    }
}

fn csi_tilde(code: u8, modifier: Option<u8>) -> Vec<u8> {
    match modifier {
        Some(m) => format!("\x1b[{};{}~", code, m).into_bytes(),
        None => format!("\x1b[{}~", code).into_bytes(),
    }
}

fn ss3_or_csi(ss3_char: u8, csi_code: u8, modifier: Option<u8>) -> Vec<u8> {
    match modifier {
        Some(m) => format!("\x1b[{};{}~", csi_code, m).into_bytes(),
        None => vec![0x1b, b'O', ss3_char],
    }
}

// ─── Entry Point ────────────────────────────────────────────────────────

/// Launch the multi-pane team view window.
///
/// If `team_name` is None, discovers the most recent active team.
pub fn main_team_view(team_name: Option<&str>) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

    // Discover team
    let name = match team_name {
        Some(n) => n.to_string(),
        None => {
            let teams = crate::team_watcher::discover_teams(&home);
            teams
                .into_iter()
                .last()
                .context("No active teams found in ~/.claude/teams/")?
        }
    };

    eprintln!("Opening team view for '{}'...", name);

    // Load team state
    let config_path = immorterm_core::team::team_config_path(&home, &name);
    let config_json = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Cannot read team config: {}", config_path))?;
    let config = immorterm_core::team::parse_team_config(&config_json)
        .with_context(|| format!("Failed to parse team config: {}", config_path))?;

    // Connect to each non-lead member's daemon
    let mut panes = Vec::new();
    for member in &config.members {
        if member.is_lead() {
            continue;
        }

        // Find daemon session socket.
        // Convention: team members run in sessions named "{team}-{member}" or just "{member}".
        let socket_path = find_member_socket(&member.name, &name);

        match socket_path {
            Some(path) => {
                eprintln!("  Connecting to {} @ {:?}", member.name, path);
                match connect_team_member(&path) {
                    Ok((output_rx, input_writer, cols, rows)) => {
                        let mut terminal = Terminal::new(cols, rows);
                        terminal.set_scrollback(10_000);
                        panes.push(MemberPane {
                            name: member.name.clone(),
                            accent: member.color_rgba(),
                            terminal,
                            output_rx,
                            input_writer,
                            socket_path: path,
                            scroll_offset: 0,
                            scroll_pixels: 0.0,
                            connection: PaneConnection::Connected,
                        });
                    }
                    Err(e) => {
                        eprintln!("  Warning: failed to connect to {}: {}", member.name, e);
                        panes.push(make_placeholder_pane(&member.name, member.color_rgba(), Some(path)));
                    }
                }
            }
            None => {
                eprintln!("  Warning: no daemon session found for {}", member.name);
                panes.push(make_placeholder_pane(&member.name, member.color_rgba(), None));
            }
        }
    }

    if panes.is_empty() {
        anyhow::bail!("No team members to display (team has {} members, all are leads?)",
            config.members.len());
    }

    eprintln!("  {} pane(s) connected. Launching window...", panes.len());

    // Build empty layout (will be computed on first resize/resumed)
    let labels: Vec<(String, [f32; 4])> = panes
        .iter()
        .map(|p| (p.name.clone(), p.accent))
        .collect();
    let layout = PaneLayout::auto_arrange(&labels, 1280.0, 800.0, 8.0, 16.0);

    // ── Event loop with typed user events ──
    let event_loop = EventLoop::<TeamUserEvent>::with_user_event()
        .build()
        .context("Failed to create event loop")?;
    let proxy = event_loop.create_proxy();

    // ── Background watcher thread ──
    // Runs a tokio runtime that subscribes to team + subagent file watchers
    // and forwards events to the winit event loop via EventLoopProxy.
    let watcher_team_name = name.clone();
    std::thread::Builder::new()
        .name("team-watchers".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime for team watchers");

            rt.block_on(async move {
                // Start team file watcher
                let (shared_state, change_tx) = match crate::team_watcher::start_team_watcher().await
                {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("Failed to start team watcher: {}", e);
                        return;
                    }
                };
                let mut change_rx = change_tx.subscribe();

                // Send initial state for our team (so TeamApp picks up lifecycle + member_status)
                {
                    let states = shared_state.read().await;
                    if let Some(state) = states.get(&watcher_team_name) {
                        let _ = proxy.send_event(TeamUserEvent::TeamStateChanged(
                            Box::new(TeamStateChange {
                                team_name: watcher_team_name.clone(),
                                state: state.clone(),
                                events: vec![],
                            }),
                        ));
                    }
                }

                // Start subagent watcher (best-effort — non-fatal if it fails)
                let subagent_rx = match crate::subagent_watcher::start_subagent_watcher() {
                    Ok(tx) => Some(tx.subscribe()),
                    Err(e) => {
                        eprintln!("Subagent watcher failed (non-fatal): {}", e);
                        None
                    }
                };

                // Forward events to winit
                if let Some(mut sub_rx) = subagent_rx {
                    loop {
                        tokio::select! {
                            Ok(change) = change_rx.recv() => {
                                if change.team_name == watcher_team_name
                                    && proxy.send_event(
                                        TeamUserEvent::TeamStateChanged(Box::new(change))
                                    ).is_err() {
                                        break; // Event loop closed
                                    }
                            }
                            Ok(event) = sub_rx.recv() => {
                                if proxy.send_event(
                                    TeamUserEvent::SubagentEvent(event)
                                ).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    // No subagent watcher — only forward team events
                    while let Ok(change) = change_rx.recv().await {
                        if change.team_name == watcher_team_name
                            && proxy
                                .send_event(TeamUserEvent::TeamStateChanged(Box::new(change)))
                                .is_err()
                            {
                                break;
                            }
                    }
                }
            });
        })
        .context("Failed to spawn watcher thread")?;

    let mut app = TeamApp {
        window: None,
        gpu: None,
        team_name: name,
        panes,
        layout,
        modifiers: ModifiersState::empty(),
        mouse_pos: (0.0, 0.0),
        team_state: None,
        subagents: Vec::new(),
        focused: true,
    };

    event_loop.set_control_flow(ControlFlow::WaitUntil(
        Instant::now() + Duration::from_millis(16),
    ));
    event_loop.run_app(&mut app).context("Event loop error")?;

    Ok(())
}

/// Try to find a daemon socket for a team member.
///
/// Checks several naming conventions:
/// 1. "{team_name}-{member_name}" (most specific)
/// 2. "{member_name}" (if member has their own session)
fn find_member_socket(member_name: &str, team_name: &str) -> Option<PathBuf> {
    // Try team-qualified name first
    let qualified = format!("{}-{}", team_name, member_name);
    if let Ok(path) = crate::commands::find_session_socket_sync(&qualified) {
        return Some(path);
    }

    // Try member name alone
    if let Ok(path) = crate::commands::find_session_socket_sync(member_name) {
        return Some(path);
    }

    // Scan sockets directory for partial matches
    let socket_dir = crate::socket_dir();
    if let Ok(entries) = std::fs::read_dir(&socket_dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname.contains(member_name) && fname.ends_with(".sock") {
                return Some(entry.path());
            }
        }
    }

    None
}

/// Create a placeholder pane for a member whose daemon isn't running.
///
/// If `socket_path` is Some, the pane is "Disconnected" and will retry connection.
/// If `socket_path` is None, the pane is "InProcess" (no terminal backend).
fn make_placeholder_pane(name: &str, accent: [f32; 4], socket_path: Option<PathBuf>) -> MemberPane {
    let (_, rx) = mpsc::channel::<Vec<u8>>();

    // Create a dummy UnixStream pair for the input writer.
    let devnull = UnixStream::connect("/dev/null")
        .or_else(|_| {
            let (a, _b) = std::os::unix::net::UnixStream::pair()
                .expect("socketpair");
            Ok::<UnixStream, std::io::Error>(a)
        })
        .unwrap();
    let input_writer = Arc::new(Mutex::new(devnull));

    let mut terminal = Terminal::new(80, 24);
    terminal.set_scrollback(100);

    let (connection, path) = if let Some(path) = socket_path {
        // Known socket path but failed to connect — will retry
        let msg = format!(
            "\x1b[1;33m[{}]\x1b[0m\r\n\r\n  \x1b[31mDisconnected\x1b[0m — retrying...\r\n",
            name
        );
        terminal.process(msg.as_bytes());
        (PaneConnection::new_disconnected(), path)
    } else {
        // No socket — in-process member (team lead or backend agent)
        let msg = format!(
            "\x1b[1;36m[{}]\x1b[0m\r\n\r\n  In-process mode — no live terminal\r\n",
            name
        );
        terminal.process(msg.as_bytes());
        (PaneConnection::InProcess, PathBuf::new())
    };

    MemberPane {
        name: name.to_string(),
        accent,
        terminal,
        output_rx: rx,
        input_writer,
        socket_path: path,
        scroll_offset: 0,
        scroll_pixels: 0.0,
        connection,
    }
}
