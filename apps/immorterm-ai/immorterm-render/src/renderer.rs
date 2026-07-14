//! TerminalRenderer — the GPU rendering orchestrator.
//!
//! Takes a `&Terminal` reference, builds instance buffers for backgrounds,
//! glyphs, and decorations, and renders a frame. Does NOT own the Terminal —
//! the platform layer (winit/WASM) owns it.

use immorterm_core::cell::{CellAttrs, Color};
use immorterm_core::cursor::CursorShape;
use immorterm_core::expression::{Celebration, DangerLevel, ExpressionMeta, Mood};
use immorterm_core::grid::Row;
use immorterm_core::Terminal;

use crate::atlas::GlyphAtlas;
use crate::images::{ImageInstance, ImageRenderer};
use crate::pipeline::{
    decor_style, BgInstance, DecorInstance, GlyphInstance, TextPipeline, Uniforms,
};
use crate::popup::PopupRenderData;
use crate::statusbar::{self, StatusBarData, StatusBarTarget};
use crate::theme::Theme;

/// Text selection state — set by the platform layer (mouse events).
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Anchor cell (where mouse-down occurred)
    pub anchor: (usize, usize), // (col, row)
    /// Active cell (current mouse position)
    pub active: (usize, usize), // (col, row)
    /// Whether a selection is currently active
    pub is_active: bool,
    /// Block (rectangular/column) selection mode — selects a rectangle
    /// defined by anchor and active corners, not full lines.
    pub block_mode: bool,
}

impl Selection {
    /// Get the selection range in (start, end) order, normalized so start <= end.
    pub fn range(&self) -> ((usize, usize), (usize, usize)) {
        let a = (self.anchor.1, self.anchor.0); // (row, col) for comparison
        let b = (self.active.1, self.active.0);
        if a <= b {
            (self.anchor, self.active)
        } else {
            (self.active, self.anchor)
        }
    }

    /// Check if a cell (col, row) is within the selection.
    pub fn contains(&self, col: usize, row: usize) -> bool {
        if !self.is_active {
            return false;
        }
        if self.block_mode {
            // Block (rectangular) selection: column range is independent of row.
            let min_col = self.anchor.0.min(self.active.0);
            let max_col = self.anchor.0.max(self.active.0);
            let min_row = self.anchor.1.min(self.active.1);
            let max_row = self.anchor.1.max(self.active.1);
            row >= min_row && row <= max_row && col >= min_col && col <= max_col
        } else {
            let ((sc, sr), (ec, er)) = self.range();
            if row < sr || row > er {
                return false;
            }
            if row == sr && row == er {
                col >= sc && col <= ec
            } else if row == sr {
                col >= sc
            } else if row == er {
                col <= ec
            } else {
                true
            }
        }
    }
}

/// Pane chrome data for header + border rendering in team view.
///
/// Built from `PaneRect` + runtime member status, passed to `render_pane_chrome()`.
#[derive(Debug, Clone)]
pub struct PaneChrome {
    /// Pixel X of the pane (full-surface coords).
    pub x: f32,
    /// Pixel Y of the pane (full-surface coords).
    pub y: f32,
    /// Pixel width of the pane.
    pub width: f32,
    /// Total height including header.
    pub total_height: f32,
    /// Header height in pixels.
    pub header_height: f32,
    /// Display label (member name).
    pub label: String,
    /// Status text ("Active", "Idle", "Done", etc.).
    pub status: String,
    /// Accent color [r, g, b, a].
    pub accent_color: [f32; 4],
    /// Whether this pane has keyboard focus.
    pub is_focused: bool,
    /// Optional badge (label, color) for team lifecycle or permission mode.
    /// Rendered as a small pill after the status text.
    pub badge: Option<(String, [f32; 4])>,
}

/// Sub-region of the surface to render into (for multi-pane layouts).
///
/// When set, the renderer uses `Uniforms::ortho_offset()` to map pane-local
/// pixel coordinates to surface NDC, and sets a scissor rect to clip drawing.
#[derive(Debug, Clone)]
pub struct PaneRegion {
    /// Pixel offset of the pane content area from the surface origin.
    pub x: f32,
    pub y: f32,
    /// Pixel dimensions of the pane content area.
    pub width: f32,
    pub height: f32,
    /// Total surface dimensions (needed for NDC mapping).
    pub surface_width: f32,
    pub surface_height: f32,
}

/// Render options passed to `render()`.
pub struct RenderOptions<'a> {
    /// Scroll offset in whole lines: 0 = live screen, >0 = scrolled up by N rows
    pub scroll_offset: usize,
    /// Active text selections (regular selection)
    pub selections: &'a [Selection],
    /// Pseudo-cursor selections (rendered with accent color)
    pub pseudo_selections: &'a [Selection],
    /// Status bar data (None = no status bar rendered)
    pub status_bar: Option<&'a StatusBarData>,
    /// Active popup menu (None = no popup)
    pub popup: Option<&'a PopupRenderData>,
    /// Sub-region to render into (None = full surface, used by single-terminal mode).
    pub pane: Option<&'a PaneRegion>,
    /// Whether to clear the surface before rendering.
    /// Set to `true` for the first pane (or single-terminal mode),
    /// `false` for subsequent panes so they don't erase earlier panes.
    pub clear: bool,
}

impl Default for RenderOptions<'_> {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            selections: &[],
            pseudo_selections: &[],
            status_bar: None,
            popup: None,
            pane: None,
            clear: true,
        }
    }
}

// ─── Celebration Particle System ──────────────────────────────────────

/// Shape hint for rendering — all rendered as DecorInstance quads,
/// but aspect ratio and size vary.
#[derive(Debug, Clone, Copy)]
enum ParticleShape {
    /// Small rectangle (confetti piece).
    Rect,
    /// Tiny square dot (sparkle point).
    Dot,
    /// Medium circle-ish square (firework ember).
    Ember,
}

/// A single particle in the celebration system.
#[derive(Debug, Clone)]
struct Particle {
    /// Position in pixel coordinates (surface-space).
    x: f32,
    y: f32,
    /// Velocity in pixels per second.
    vx: f32,
    vy: f32,
    /// RGBA color.
    color: [f32; 4],
    /// Current alpha (fades over lifetime).
    alpha: f32,
    /// Total lifetime in seconds.
    lifetime: f32,
    /// Time elapsed since spawn.
    age: f32,
    /// Visual shape hint.
    shape: ParticleShape,
    /// Rotation angle in radians (for confetti visual wobble via size modulation).
    rotation: f32,
    /// Rotation speed in radians per second.
    rotation_speed: f32,
}

/// One-shot particle celebration system. Persists on the Renderer across frames.
///
/// When triggered, spawns a batch of particles that animate for ~2-3 seconds
/// and then self-remove. Renders as DecorInstance quads (solid style) —
/// no new GPU pipeline needed.
#[derive(Debug, Default)]
struct CelebrationSystem {
    particles: Vec<Particle>,
    /// Simple PRNG state (xorshift32) for deterministic-ish randomness.
    rng_state: u32,
}

impl CelebrationSystem {
    fn new() -> Self {
        Self {
            particles: Vec::new(),
            // Seed with a non-zero value; will be re-seeded from time on first use.
            rng_state: 0xDEAD_BEEF,
        }
    }

    /// Xorshift32 PRNG — returns a pseudo-random u32.
    fn next_u32(&mut self) -> u32 {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng_state = x;
        x
    }

    /// Returns a pseudo-random f32 in [0.0, 1.0).
    fn next_f32(&mut self) -> f32 {
        (self.next_u32() & 0x00FF_FFFF) as f32 / 16_777_216.0
    }

    /// Returns a pseudo-random f32 in [lo, hi).
    fn next_range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.next_f32() * (hi - lo)
    }

    /// Seed the PRNG from the current time so each celebration looks different.
    fn seed_from_time(&mut self, time: f32) {
        self.rng_state = (time * 1_000_000.0) as u32;
        if self.rng_state == 0 {
            self.rng_state = 1;
        }
        // Warm up a few rounds to decorrelate from seed.
        for _ in 0..4 {
            self.next_u32();
        }
    }

    /// Spawn particles for the given celebration type.
    fn spawn(&mut self, celebration: Celebration, surface_w: f32, surface_h: f32, time: f32) {
        self.seed_from_time(time);

        match celebration {
            Celebration::Confetti => self.spawn_confetti(surface_w, surface_h),
            Celebration::Sparkle => self.spawn_sparkle(surface_w, surface_h),
            Celebration::Fireworks => self.spawn_fireworks(surface_w, surface_h),
        }
    }

    /// Confetti: ~60 small colored rectangles falling from top with horizontal drift.
    fn spawn_confetti(&mut self, surface_w: f32, _surface_h: f32) {
        let colors: [[f32; 3]; 8] = [
            [1.0, 0.2, 0.3],   // red
            [0.2, 0.6, 1.0],   // blue
            [1.0, 0.85, 0.1],  // yellow
            [0.3, 0.9, 0.4],   // green
            [1.0, 0.5, 0.0],   // orange
            [0.8, 0.3, 1.0],   // purple
            [1.0, 0.4, 0.7],   // pink
            [0.0, 0.9, 0.9],   // cyan
        ];

        let count = 60;
        self.particles.reserve(count);
        for _ in 0..count {
            let ci = (self.next_u32() as usize) % colors.len();
            let c = colors[ci];
            let x = self.next_range(0.0, surface_w);
            let y = self.next_range(-60.0, -5.0); // start above viewport
            let vx = self.next_range(-40.0, 40.0);
            let vy = self.next_range(80.0, 220.0); // falling down
            let lifetime = self.next_range(2.0, 3.0);
            let rotation_speed = self.next_range(-4.0, 4.0);
            let rotation = self.next_range(0.0, std::f32::consts::TAU);
            self.particles.push(Particle {
                x,
                y,
                vx,
                vy,
                color: [c[0], c[1], c[2], 1.0],
                alpha: 1.0,
                lifetime,
                age: 0.0,
                shape: ParticleShape::Rect,
                rotation,
                rotation_speed,
            });
        }
    }

    /// Sparkle: ~40 tiny white/gold dots radiating outward from center, fading quickly.
    fn spawn_sparkle(&mut self, surface_w: f32, surface_h: f32) {
        let cx = surface_w * 0.5;
        let cy = surface_h * 0.5;

        let count = 40;
        self.particles.reserve(count);
        for _ in 0..count {
            let angle = self.next_range(0.0, std::f32::consts::TAU);
            let speed = self.next_range(60.0, 250.0);
            let vx = angle.cos() * speed;
            let vy = angle.sin() * speed;
            // White to gold spectrum
            let gold_mix = self.next_f32();
            let r = 1.0;
            let g = 0.85 + gold_mix * 0.15; // 0.85..1.0
            let b = 0.6 + (1.0 - gold_mix) * 0.4; // 0.6..1.0
            let lifetime = self.next_range(0.6, 1.4);
            let px = cx + self.next_range(-5.0, 5.0);
            let py = cy + self.next_range(-5.0, 5.0);
            self.particles.push(Particle {
                x: px,
                y: py,
                vx,
                vy,
                color: [r, g, b, 1.0],
                alpha: 1.0,
                lifetime,
                age: 0.0,
                shape: ParticleShape::Dot,
                rotation: 0.0,
                rotation_speed: 0.0,
            });
        }
    }

    /// Fireworks: ~100 particles exploding from a center point, arcing downward with gravity.
    fn spawn_fireworks(&mut self, surface_w: f32, surface_h: f32) {
        // Explosion origin: slightly above center
        let cx = surface_w * 0.5;
        let cy = surface_h * 0.35;

        // Two color palettes that blend (warm + cool)
        let palette: [[f32; 3]; 6] = [
            [1.0, 0.3, 0.1],   // warm red-orange
            [1.0, 0.7, 0.1],   // gold
            [1.0, 0.9, 0.5],   // bright yellow
            [0.4, 0.6, 1.0],   // cool blue
            [0.8, 0.4, 1.0],   // violet
            [1.0, 1.0, 1.0],   // white
        ];

        let count = 100;
        self.particles.reserve(count);
        for _ in 0..count {
            let angle = self.next_range(0.0, std::f32::consts::TAU);
            let speed = self.next_range(100.0, 350.0);
            let vx = angle.cos() * speed;
            let vy = angle.sin() * speed;
            let ci = (self.next_u32() as usize) % palette.len();
            let c = palette[ci];
            let lifetime = self.next_range(1.5, 2.5);
            let px = cx + self.next_range(-3.0, 3.0);
            let py = cy + self.next_range(-3.0, 3.0);
            self.particles.push(Particle {
                x: px,
                y: py,
                vx,
                vy,
                color: [c[0], c[1], c[2], 1.0],
                alpha: 1.0,
                lifetime,
                age: 0.0,
                shape: ParticleShape::Ember,
                rotation: 0.0,
                rotation_speed: 0.0,
            });
        }
    }

    /// Update all particles by `dt` seconds. Removes dead particles.
    fn update(&mut self, dt: f32) {
        const GRAVITY: f32 = 200.0; // pixels/sec^2 (downward)

        for p in &mut self.particles {
            p.age += dt;

            // Physics
            p.x += p.vx * dt;
            p.y += p.vy * dt;

            // Gravity (affects all types, stronger for fireworks embers)
            match p.shape {
                ParticleShape::Rect => {
                    // Confetti: gentle gravity + slight horizontal wobble
                    p.vy += GRAVITY * 0.4 * dt;
                    p.vx += (p.rotation).sin() * 20.0 * dt; // wobble
                }
                ParticleShape::Dot => {
                    // Sparkle: decelerate (drag), no gravity
                    p.vx *= 1.0 - 2.5 * dt;
                    p.vy *= 1.0 - 2.5 * dt;
                }
                ParticleShape::Ember => {
                    // Fireworks: full gravity + slight drag
                    p.vy += GRAVITY * dt;
                    p.vx *= 1.0 - 0.5 * dt;
                }
            }

            // Rotation (confetti tumble)
            p.rotation += p.rotation_speed * dt;

            // Alpha fade: hold full brightness for first 40% of lifetime, then fade to 0
            let t = p.age / p.lifetime;
            p.alpha = if t < 0.4 {
                1.0
            } else {
                ((1.0 - t) / 0.6).max(0.0)
            };
        }

        // Remove dead particles
        self.particles.retain(|p| p.age < p.lifetime);
    }

    /// Whether there are active particles to render.
    fn is_active(&self) -> bool {
        !self.particles.is_empty()
    }

    /// Append DecorInstance quads for all live particles.
    fn render(&self, decor_instances: &mut Vec<DecorInstance>) {
        for p in &self.particles {
            if p.alpha < 0.001 {
                continue;
            }

            let (w, h) = match p.shape {
                ParticleShape::Rect => {
                    // Confetti: ~6x4 px rectangle, modulated by rotation for visual tumble
                    let wobble = p.rotation.cos().abs();
                    (6.0 * wobble.max(0.3), 4.0)
                }
                ParticleShape::Dot => {
                    // Sparkle: tiny 2x2..3x3 dot
                    let s = 2.0 + p.alpha; // shrinks as it fades
                    (s, s)
                }
                ParticleShape::Ember => {
                    // Firework ember: 3x3..4x4
                    let s = 3.0 + p.alpha;
                    (s, s)
                }
            };

            let mut color = p.color;
            color[3] = p.alpha;

            decor_instances.push(DecorInstance {
                pos: [p.x, p.y],
                size: [w, h],
                color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
        }
    }
}

/// GPU terminal renderer. Platform-agnostic — receives wgpu Device/Queue
/// from the host and renders into a provided surface texture view.
pub struct TerminalRenderer {
    pub atlas: GlyphAtlas,
    pub pipeline: TextPipeline,
    pub image_renderer: ImageRenderer,
    pub theme: Theme,
    width: f32,
    height: f32,
    pub start_time: web_time::Instant,
    /// Linear alpha multiplier for glyph thickening (> 1.0 = bolder text).
    /// Compensates for grayscale AA looking thinner than subpixel AA.
    pub font_thicken: f32,
    /// Whether the status bar is enabled (reserves one row at the bottom).
    pub status_bar_enabled: bool,
    /// Content padding in physical pixels [top, right, bottom, left].
    /// Terminal content is inset by this amount while the window border
    /// remains at the canvas edges. Default: no padding.
    pub content_padding: [f32; 4],
    /// Whether the window border is rendered.
    pub border_enabled: bool,
    /// Border opacity multiplier (0.0 = invisible, 1.0 = fully opaque).
    pub border_opacity: f32,
    /// Whether status bar animations (shimmer, bloom, breathing, wave) are active.
    pub animations_enabled: bool,
    /// Whether AI expression effects (mood colors, confidence alpha) are rendered.
    pub expression_effects: bool,
    /// Whether celebration particle effects (confetti, sparkle, fireworks) are rendered.
    pub celebrations_enabled: bool,
    /// Whether danger visual effects (vignette, pulse, border accent) are rendered.
    pub danger_effects: bool,
    /// Whether per-character text animations (pulse, glow, wave, etc.) are rendered.
    pub text_animations: bool,
    /// Status bar visual reveal factor (0.0 = hidden, 1.0 = fully visible).
    /// Used for smooth fade+slide animation without triggering PTY resize.
    pub status_bar_reveal: f32,
    /// Celebration particle system (confetti, sparkle, fireworks).
    celebration: CelebrationSystem,
    /// Time of the previous frame, for computing delta-time for particles.
    prev_frame_time: f32,
    /// Previous frame's scroll offset — detects scroll activity for fade timing.
    scroll_indicator_prev_offset: usize,
    /// Time (seconds) when scroll offset last changed — drives fade-out delay.
    scroll_indicator_last_change: f32,
    /// Current animated opacity (0.0–1.0) of the scroll indicator.
    scroll_indicator_opacity: f32,
    /// Mouse proximity to scroll indicator (0.0–1.0), set by platform layer.
    scroll_indicator_proximity: f32,
    /// Animated width expansion factor (0.0–1.0) driven by proximity.
    scroll_indicator_hover: f32,
    /// Global text alignment setting (Left/Right/Center/Auto).
    pub text_alignment: crate::bidi::TextAlignment,
    /// Global paragraph direction (Ltr/Rtl/Auto).
    pub paragraph_direction: crate::bidi::ParagraphDirection,
    /// Per-row BiDi reordering cache. Indexed by content row index.
    /// Invalidated when rows become dirty. Reset on resize or scrollback growth.
    bidi_cache: Vec<Option<crate::bidi::BidiRowCache>>,
    /// Scrollback length when bidi_cache was last valid. Cache is cleared when this changes,
    /// because content_idx = sb_len + display_row shifts when scrollback grows.
    bidi_cache_sb_len: usize,
    /// Reusable scratch buffer for tracking which logical columns are covered by run-based
    /// RTL rendering. Avoids per-row heap allocation in render_row.
    run_covered_scratch: Vec<bool>,
}

impl TerminalRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        font_data: Option<&[&[u8]]>,
        font_size: f32,
    ) -> Self {
        Self::with_line_height(device, queue, surface_format, font_data, font_size, 1.2)
    }

    pub fn with_line_height(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        font_data: Option<&[&[u8]]>,
        font_size: f32,
        line_height_ratio: f32,
    ) -> Self {
        let atlas = GlyphAtlas::with_line_height(device, queue, font_size, font_data, line_height_ratio);
        let pipeline = TextPipeline::new(device, surface_format, atlas.bind_group_layout());
        let image_renderer = ImageRenderer::new(
            device,
            surface_format,
            &pipeline.uniform_bind_group_layout,
        );

        Self {
            atlas,
            pipeline,
            image_renderer,
            theme: Theme::default(),
            width: 800.0,
            height: 600.0,
            start_time: web_time::Instant::now(),
            font_thicken: 1.15, // power curve exponent: pow(alpha, 1/thicken); >1.0 = bolder glyphs
            status_bar_enabled: false,
            content_padding: [0.0; 4],
            border_enabled: true,
            border_opacity: 1.0,
            animations_enabled: true,
            expression_effects: true,
            celebrations_enabled: true,
            danger_effects: true,
            text_animations: true,
            status_bar_reveal: 1.0,
            celebration: CelebrationSystem::new(),
            prev_frame_time: 0.0,
            scroll_indicator_prev_offset: 0,
            scroll_indicator_last_change: 0.0,
            scroll_indicator_opacity: 0.0,
            scroll_indicator_proximity: 0.0,
            scroll_indicator_hover: 0.0,
            text_alignment: crate::bidi::TextAlignment::default(),
            paragraph_direction: crate::bidi::ParagraphDirection::default(),
            bidi_cache: Vec::new(),
            bidi_cache_sb_len: 0,
            run_covered_scratch: Vec::new(),
        }
    }

    /// Get the BiDi cache entry for a content row.
    /// Returns None if BiDi is disabled, the row hasn't been cached, or the index is out of range.
    pub fn bidi_cache_for(&self, content_idx: usize) -> Option<&crate::bidi::BidiRowCache> {
        self.bidi_cache.get(content_idx).and_then(|c| c.as_ref())
    }

    /// Clear the BiDi reordering cache. Call on session switch so stale RTL
    /// entries from one session don't apply to another session's content.
    pub fn clear_bidi_cache(&mut self) {
        self.bidi_cache.clear();
        self.bidi_cache_sb_len = 0;
    }

    /// Render a frame of the terminal into the given surface texture view.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_view: &wgpu::TextureView,
        terminal: &mut Terminal,
        opts: &RenderOptions,
    ) {
        let cw = self.atlas.metrics.cell_width;
        let ch = self.atlas.metrics.cell_height;
        let time = self.start_time.elapsed().as_secs_f32();
        // Frozen time for status bar animations: 14.0s falls outside ALL
        // animation windows (shimmer 0-1.5s, bloom 2-7s, breathing 8-13s).
        let sb_time = if self.animations_enabled { time } else { 14.0 };
        let blink_visible = (time % 1.0) < 0.7;

        // Content padding offsets (pixels and fractional cells for BgInstance)
        let pad_top = self.content_padding[0];
        let pad_left = self.content_padding[3];
        let _pad_cell_x = pad_left / cw;
        let pad_cell_y = pad_top / ch;

        let visible_rows = terminal.grid.num_rows();
        let visible_cols = terminal.grid.cols();
        let cell_count = visible_cols * visible_rows;
        let mut bg_instances: Vec<BgInstance> = Vec::with_capacity(cell_count);
        let mut glyph_instances: Vec<GlyphInstance> = Vec::with_capacity(cell_count);
        let mut decor_instances: Vec<DecorInstance> = Vec::with_capacity(cell_count / 4);

        let underline_thickness = (ch * 0.06).max(1.0);
        let baseline_y = self.atlas.metrics.baseline_y;
        // Position underline just below the baseline (not at cell bottom)
        let underline_y_offset = baseline_y + underline_thickness * 2.0;
        let strikethrough_y_offset = ch * 0.45;

        let sb_len = terminal.scrollback.len();
        let scroll_offset = opts.scroll_offset.min(sb_len);

        // Choose projection: pane-local offset or full-surface
        let (render_width, render_height) = if let Some(pane) = opts.pane {
            (pane.width, pane.height)
        } else {
            (self.width, self.height)
        };
        let uniforms = if let Some(pane) = opts.pane {
            Uniforms::ortho_offset(
                pane.width, pane.height,
                pane.x, pane.y,
                pane.surface_width, pane.surface_height,
                cw, ch, time, self.font_thicken,
            )
        } else {
            Uniforms::ortho(self.width, self.height, cw, ch, time, self.font_thicken)
        };
        queue.write_buffer(
            &self.pipeline.uniform_buffer,
            0,
            bytemuck::cast_slice(&[uniforms]),
        );

        let render_rows = visible_rows;

        // Compute popup rect for cell-skipping (popup occludes terminal content)
        let popup_rect: Option<(usize, usize, usize, usize)> =
            opts.popup
                .filter(|p| p.visible && !p.items.is_empty())
                .map(|p| {
                    let (row_start, row_end) = p.row_range(visible_rows);
                    (p.anchor_col, p.anchor_col + p.width_cols, row_start, row_end)
                });

        // ── BiDi: ensure cache is sized and compute per-row reordering ──
        let bidi_enabled = terminal.modes.bidi_implicit;
        let total_content_rows = sb_len + visible_rows;
        // Invalidate cache when scrollback grows — content indices shift, stale entries
        // from old rows would be applied to different rows at the same index.
        if sb_len != self.bidi_cache_sb_len {
            self.bidi_cache.clear();
            self.bidi_cache_sb_len = sb_len;
        }
        if self.bidi_cache.len() < total_content_rows {
            self.bidi_cache.resize_with(total_content_rows, || None);
        }

        // Compose viewport: when scrolled up, top rows come from scrollback,
        // bottom rows from the live grid.
        for display_row in 0..render_rows {
            let content_idx = (sb_len + display_row).saturating_sub(scroll_offset);

            let row: Option<&Row> = if content_idx < sb_len {
                terminal.scrollback.get(content_idx)
            } else {
                terminal.grid.row(content_idx - sb_len)
            };

            let row = match row {
                Some(r) => r,
                None => continue,
            };

            // Compute BiDi reordering for this row (only when dirty or uncached)
            if bidi_enabled && (row.dirty || self.bidi_cache.get(content_idx).is_none_or(|c| c.is_none())) {
                // Resolve per-row overrides, falling back to global settings
                let direction = match row.direction {
                    Some(0) => crate::bidi::ParagraphDirection::Ltr,
                    Some(1) => crate::bidi::ParagraphDirection::Rtl,
                    Some(2) => crate::bidi::ParagraphDirection::Auto,
                    _ => self.paragraph_direction,
                };
                let alignment = match row.alignment {
                    Some(0) => crate::bidi::TextAlignment::Left,
                    Some(1) => crate::bidi::TextAlignment::Right,
                    Some(2) => crate::bidi::TextAlignment::Center,
                    Some(3) => crate::bidi::TextAlignment::Auto,
                    _ => self.text_alignment,
                };

                let cache = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::bidi::reorder_row(
                        row,
                        &terminal.combining_marks,
                        content_idx,
                        direction,
                        alignment,
                        visible_cols,
                        cw,
                    )
                }));
                match cache {
                    Ok(c) => {
                        if content_idx < self.bidi_cache.len() {
                            self.bidi_cache[content_idx] = Some(c);
                        }
                    }
                    Err(_) => {
                        // BiDi computation panicked — skip this row's reordering
                        // rather than crashing the entire render frame.
                    }
                }
            }

            self.render_row(
                display_row,
                content_idx,
                sb_len,
                row,
                visible_cols,
                cw,
                ch,
                blink_visible,
                underline_thickness,
                underline_y_offset,
                strikethrough_y_offset,
                opts.selections,
                opts.pseudo_selections,
                popup_rect,
                &terminal.expression_colors,
                terminal.expression_meta,
                &terminal.combining_marks,
                queue,
                &mut bg_instances,
                &mut glyph_instances,
                &mut decor_instances,
            );
        }

        // Cursor — only shown when viewing the live screen (scroll_offset == 0)
        if scroll_offset == 0 && terminal.cursor.visible && terminal.modes.cursor_visible {
            let cc = terminal.cursor.col;
            let cr = terminal.cursor.row;
            if cc < visible_cols && cr < visible_rows {
                // BiDi: map logical cursor column to visual position
                let cursor_content_idx = sb_len + cr;
                let cursor_bidi = self.bidi_cache.get(cursor_content_idx).and_then(|c| c.as_ref());
                let visual_cc = cursor_bidi
                    .and_then(|b| b.logical_to_visual.get(cc).copied())
                    .unwrap_or(cc);
                let cursor_align_offset = cursor_bidi.map(|b| b.alignment_offset_px).unwrap_or(0.0);
                let cursor_x = pad_left + cursor_align_offset + visual_cc as f32 * cw;
                let cursor_y = pad_top + cr as f32 * ch;
                let breathing_phase = 1.0_f32;

                match terminal.cursor.shape {
                    CursorShape::Block => {
                        decor_instances.push(DecorInstance {
                            pos: [cursor_x, cursor_y],
                            size: [cw, ch],
                            color: self.theme.cursor,
                            extra: [decor_style::SOLID, breathing_phase],
                            _pad: [0.0, 0.0],
                        });
                    }
                    CursorShape::Underline => {
                        let bar_h = (ch * 0.1).max(2.0);
                        decor_instances.push(DecorInstance {
                            pos: [cursor_x, cursor_y + ch - bar_h],
                            size: [cw, bar_h],
                            color: self.theme.cursor,
                            extra: [decor_style::SOLID, breathing_phase],
                            _pad: [0.0, 0.0],
                        });
                    }
                    CursorShape::Bar => {
                        let bar_w = (cw * 0.1).max(2.0);
                        decor_instances.push(DecorInstance {
                            pos: [cursor_x, cursor_y],
                            size: [bar_w, ch],
                            color: self.theme.cursor,
                            extra: [decor_style::SOLID, breathing_phase],
                            _pad: [0.0, 0.0],
                        });
                    }
                }
            }
        }

        // ── Sync GPU images with terminal graphics state ──
        // Upload any new images, collect instances for visible ones
        let mut image_instances: Vec<ImageInstance> = Vec::new();
        let mut image_ids: Vec<u32> = Vec::new();

        // Collect placements sorted by z_index
        let mut placements: Vec<_> = terminal.graphics.placements().collect();
        placements.sort_by_key(|p| p.z_index);

        for placement in &placements {
            // Upload to GPU if not yet present
            if !self.image_renderer.has_image(placement.id) {
                self.image_renderer.upload(
                    device,
                    queue,
                    placement.id,
                    &placement.data,
                    placement.width,
                    placement.height,
                );
            }

            // Compute pixel position from absolute content row.
            // placement.row is absolute (scrollback.len() + grid_row at placement time).
            // The viewport starts at content index (sb_len - scroll_offset).
            let display_row = placement.row as isize - (sb_len as isize - scroll_offset as isize);
            let pixel_x = pad_left + placement.col as f32 * cw;
            let pixel_y = pad_top + display_row as f32 * ch;
            let pixel_w = placement.cell_width as f32 * cw;
            let pixel_h = placement.cell_height as f32 * ch;

            // Only render if at least partially visible
            if pixel_y + pixel_h < 0.0 || pixel_y > render_height {
                continue;
            }

            image_instances.push(ImageInstance {
                pos: [pixel_x, pixel_y],
                size: [pixel_w, pixel_h],
                uv_pos: [0.0, 0.0],
                uv_size: [1.0, 1.0],
                opacity: 1.0,
                _pad: [0.0, 0.0, 0.0],
            });
            image_ids.push(placement.id);
        }

        // Clean up GPU textures for images the terminal no longer has
        let active_ids: Vec<u32> = placements.iter().map(|p| p.id).collect();
        self.image_renderer.retain(&active_ids);

        // ── AI Canvas Layer (persistent AI-drawn primitives) ──
        self.render_ai_layer(
            terminal,
            cw,
            ch,
            time,
            scroll_offset,
            sb_len,
            pad_top,
            pad_left,
            queue,
            &mut bg_instances,
            &mut glyph_instances,
            &mut decor_instances,
        );

        // ── Overlays (annotations + charts) ──
        self.render_overlays(
            terminal,
            scroll_offset,
            cw,
            ch,
            visible_rows,
            queue,
            &mut decor_instances,
            &mut glyph_instances,
        );

        // ── Status bar (rendered below terminal grid, full-width chrome) ──
        // Status bar spans the entire canvas width — no horizontal content padding.
        // Only vertical offset is applied so it sits below the padded content rows.
        if let Some(sb_data) = opts.status_bar {
            self.render_status_bar(
                sb_data,
                visible_rows, // one row below the grid
                cw,
                ch,
                [0.0, pad_cell_y],   // no horizontal offset — full-width chrome
                [0.0, pad_top],      // no horizontal pixel offset
                sb_time,
                queue,
                &mut bg_instances,
                &mut glyph_instances,
            );

            // Gap-fill below status bar is handled by extension quads inside
            // render_status_bar() — they tile to the canvas bottom automatically.
        }

        // ── Popup menu (rendered above status bar, occludes terminal content) ──
        if let Some(popup) = opts.popup {
            // Get theme colors from status bar data, or use defaults
            let (popup_accent, popup_grad_start) = if let Some(sb) = opts.status_bar {
                (sb.accent, sb.gradient_stops[0])
            } else {
                ([0.878, 0.690, 1.0, 1.0], [0.176, 0.0, 0.302])
            };
            self.render_popup(
                popup,
                visible_rows,
                cw,
                ch,
                [0.0, pad_cell_y],   // no horizontal offset — popup is status bar chrome
                [0.0, pad_top],      // no horizontal pixel offset
                popup_accent,
                popup_grad_start,
                queue,
                &mut bg_instances,
                &mut glyph_instances,
                &mut decor_instances,
            );
        }

        // ── Danger vignette (screen-edge darkening) ──
        // When danger is Medium or higher, render dark transparent strips along
        // all 4 edges that fade inward, creating a vignette effect. Intensity
        // scales with danger level.
        if self.danger_effects
            && terminal.expression.danger != DangerLevel::None
            && terminal.expression.danger != DangerLevel::Low
        {
            let vignette_alpha = match terminal.expression.danger {
                DangerLevel::Medium => 0.05,
                DangerLevel::High => 0.10,
                DangerLevel::Critical => 0.18,
                _ => 0.0,
            };
            if vignette_alpha > 0.0 {
                let vignette_h = self.height * 0.08; // 8% of screen height
                let vignette_w = self.width * 0.06; // 6% of screen width
                let strips = 8;

                // Top edge vignette (gradient from dark at top to transparent)
                for i in 0..strips {
                    let t = i as f32 / strips as f32;
                    let alpha = vignette_alpha * (1.0 - t);
                    let y = t * vignette_h;
                    let strip_h = vignette_h / strips as f32;
                    decor_instances.push(DecorInstance {
                        pos: [0.0, y],
                        size: [self.width, strip_h],
                        color: [0.0, 0.0, 0.0, alpha],
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });
                }
                // Bottom edge (mirror of top)
                for i in 0..strips {
                    let t = i as f32 / strips as f32;
                    let alpha = vignette_alpha * (1.0 - t);
                    let y = self.height - (t + 1.0) * vignette_h / strips as f32;
                    let strip_h = vignette_h / strips as f32;
                    decor_instances.push(DecorInstance {
                        pos: [0.0, y],
                        size: [self.width, strip_h],
                        color: [0.0, 0.0, 0.0, alpha],
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });
                }
                // Left edge
                for i in 0..strips {
                    let t = i as f32 / strips as f32;
                    let alpha = vignette_alpha * (1.0 - t);
                    let x = t * vignette_w;
                    let strip_w = vignette_w / strips as f32;
                    decor_instances.push(DecorInstance {
                        pos: [x, 0.0],
                        size: [strip_w, self.height],
                        color: [0.0, 0.0, 0.0, alpha],
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });
                }
                // Right edge (mirror of left)
                for i in 0..strips {
                    let t = i as f32 / strips as f32;
                    let alpha = vignette_alpha * (1.0 - t);
                    let x = self.width - (t + 1.0) * vignette_w / strips as f32;
                    let strip_w = vignette_w / strips as f32;
                    decor_instances.push(DecorInstance {
                        pos: [x, 0.0],
                        size: [strip_w, self.height],
                        color: [0.0, 0.0, 0.0, alpha],
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });
                }
            }
        }

        // Critical pulse removed — the full-screen breathing red overlay was too
        // distracting for inline marker use. Danger is now communicated via
        // per-cell BG tint + vignette + border accent, which is sufficient.

        // ── macOS-style scroll indicator ──
        // Thin semi-transparent thumb on right edge; appears on scroll, fades after idle.
        {
            let total_lines = sb_len + visible_rows;
            let dt = (time - self.prev_frame_time).clamp(0.0, 0.1);

            // Detect scroll activity
            if scroll_offset != self.scroll_indicator_prev_offset {
                self.scroll_indicator_last_change = time;
            }
            self.scroll_indicator_prev_offset = scroll_offset;

            // Compute target opacity
            let target = if total_lines <= visible_rows {
                0.0 // no scrollback — never show
            } else if self.scroll_indicator_proximity > 0.01 {
                1.0 // mouse near scrollbar — keep visible
            } else if scroll_offset == 0 {
                0.0 // at live view — fade out
            } else {
                let idle_secs = time - self.scroll_indicator_last_change;
                if idle_secs < 1.5 {
                    1.0
                } else {
                    (1.0 - (idle_secs - 1.5) / 0.4).clamp(0.0, 1.0)
                }
            };

            // Smooth lerp toward target (~150ms fade-in, ~400ms fade-out)
            let lerp_speed = if target > self.scroll_indicator_opacity { 12.0 } else { 6.0 };
            self.scroll_indicator_opacity +=
                (target - self.scroll_indicator_opacity) * (lerp_speed * dt).min(1.0);

            // Animate hover expansion (mouse proximity → width growth)
            let hover_speed = if self.scroll_indicator_proximity > self.scroll_indicator_hover {
                10.0
            } else {
                6.0
            };
            self.scroll_indicator_hover += (self.scroll_indicator_proximity
                - self.scroll_indicator_hover)
                * (hover_speed * dt).min(1.0);

            if self.scroll_indicator_opacity > 0.001 && total_lines > visible_rows {
                let alpha = self.scroll_indicator_opacity;
                let base_w = 20.0_f32;
                let expanded_w = 30.0_f32;
                let indicator_w = base_w + (expanded_w - base_w) * self.scroll_indicator_hover;
                let inset = 6.0_f32;
                let indicator_x = render_width - inset - indicator_w;
                let track_top = pad_top;
                let track_height = visible_rows as f32 * ch;

                // Track (subtle background line)
                decor_instances.push(DecorInstance {
                    pos: [indicator_x, track_top],
                    size: [indicator_w, track_height],
                    color: [1.0, 1.0, 1.0, 0.04 * alpha],
                    extra: [decor_style::SOLID, 0.0],
                    _pad: [0.0, 0.0],
                });

                // Thumb — sized proportionally, minimum 80px
                let thumb_height =
                    (track_height * visible_rows as f32 / total_lines as f32).max(80.0);
                let scroll_fraction = scroll_offset as f32 / sb_len as f32;
                let thumb_y =
                    track_top + (track_height - thumb_height) * (1.0 - scroll_fraction);
                let accent = if let Some(sb) = opts.status_bar {
                    sb.accent
                } else {
                    self.theme.fg
                };
                let thumb_alpha = 0.55 + 0.15 * self.scroll_indicator_hover;
                decor_instances.push(DecorInstance {
                    pos: [indicator_x, thumb_y],
                    size: [indicator_w, thumb_height],
                    color: [accent[0], accent[1], accent[2], thumb_alpha * alpha],
                    extra: [decor_style::SOLID, 0.0],
                    _pad: [0.0, 0.0],
                });
            }
        }

        // ── Themed window border ──
        // Thin accent border around all 4 edges, rendered as DecorInstance
        // rectangles in pixel coordinates. Uses theme accent color if available.
        if self.border_enabled {
            let border_thick = 2.0_f32;
            let mut border_color = if let Some(sb) = opts.status_bar {
                sb.accent
            } else {
                self.theme.border
            };
            border_color[3] *= self.border_opacity;

            // Danger border accent — override to red when danger is High or Critical
            if self.danger_effects
                && (terminal.expression.danger == DangerLevel::High
                    || terminal.expression.danger == DangerLevel::Critical)
            {
                let danger_red = match terminal.expression.danger {
                    DangerLevel::High => [0.8, 0.15, 0.1, border_color[3]],
                    DangerLevel::Critical => {
                        // Pulse the border brightness in sync with the overlay pulse
                        let pulse = (time * 2.0 * std::f32::consts::PI).sin() * 0.5 + 0.5;
                        let brightness = 0.7 + pulse * 0.3; // 0.7..1.0
                        [brightness, 0.1, 0.05, border_color[3]]
                    }
                    _ => border_color,
                };
                border_color = danger_red;
            }

            // Top edge
            decor_instances.push(DecorInstance {
                pos: [0.0, 0.0],
                size: [self.width, border_thick],
                color: border_color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
            // Bottom edge
            decor_instances.push(DecorInstance {
                pos: [0.0, self.height - border_thick],
                size: [self.width, border_thick],
                color: border_color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
            // Left edge
            decor_instances.push(DecorInstance {
                pos: [0.0, 0.0],
                size: [border_thick, self.height],
                color: border_color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
            // Right edge
            decor_instances.push(DecorInstance {
                pos: [self.width - border_thick, 0.0],
                size: [border_thick, self.height],
                color: border_color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
        }

        // ── Celebration particle system ──
        // Check for new celebration trigger (one-shot from expression state)
        if let Some(celebration) = terminal.expression.take_celebration()
            && self.celebrations_enabled {
                self.celebration.spawn(celebration, self.width, self.height, time);
            }

        // Update and render active particles
        if self.celebration.is_active() {
            let dt = (time - self.prev_frame_time).clamp(0.0, 0.1); // cap dt to avoid explosion on lag
            self.celebration.update(dt);
            self.celebration.render(&mut decor_instances);
        }
        self.prev_frame_time = time;

        // Upload text + decor buffers
        if !bg_instances.is_empty() {
            queue.write_buffer(
                &self.pipeline.bg_buffer,
                0,
                bytemuck::cast_slice(&bg_instances),
            );
        }
        if !glyph_instances.is_empty() {
            queue.write_buffer(
                &self.pipeline.glyph_buffer,
                0,
                bytemuck::cast_slice(&glyph_instances),
            );
        }
        if !decor_instances.is_empty() {
            queue.write_buffer(
                &self.pipeline.decor_buffer,
                0,
                bytemuck::cast_slice(&decor_instances),
            );
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render_encoder"),
        });

        {
            // Choose clear vs load based on whether this is the first pane
            let load_op = if opts.clear {
                wgpu::LoadOp::Clear(wgpu::Color {
                    r: self.theme.bg[0] as f64,
                    g: self.theme.bg[1] as f64,
                    b: self.theme.bg[2] as f64,
                    a: 1.0,
                })
            } else {
                wgpu::LoadOp::Load
            };

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terminal_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: surface_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: load_op,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Scissor rect: clip drawing to pane region (prevents bleed into neighbors)
            if let Some(pane) = opts.pane {
                pass.set_scissor_rect(
                    pane.x as u32,
                    pane.y as u32,
                    pane.width as u32,
                    pane.height as u32,
                );
            }

            if !bg_instances.is_empty() {
                pass.set_pipeline(&self.pipeline.bg_pipeline);
                pass.set_bind_group(0, &self.pipeline.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, self.pipeline.bg_buffer.slice(..));
                pass.draw(0..6, 0..bg_instances.len() as u32);
            }

            if !glyph_instances.is_empty() {
                pass.set_pipeline(&self.pipeline.glyph_pipeline);
                pass.set_bind_group(0, &self.pipeline.uniform_bind_group, &[]);
                pass.set_bind_group(1, self.atlas.bind_group(), &[]);
                pass.set_vertex_buffer(0, self.pipeline.glyph_buffer.slice(..));
                pass.draw(0..6, 0..glyph_instances.len() as u32);
            }

            if !decor_instances.is_empty() {
                pass.set_pipeline(&self.pipeline.decor_pipeline);
                pass.set_bind_group(0, &self.pipeline.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, self.pipeline.decor_buffer.slice(..));
                pass.draw(0..6, 0..decor_instances.len() as u32);
            }

            // Images render ON TOP of text (alpha-blended)
            if !image_instances.is_empty() {
                self.image_renderer.render(
                    &mut pass,
                    queue,
                    &self.pipeline.uniform_bind_group,
                    &image_instances,
                    &image_ids,
                );
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
    }

    /// Render overlay annotations and charts from terminal state.
    #[allow(clippy::too_many_arguments)]
    fn render_overlays(
        &mut self,
        terminal: &Terminal,
        scroll_offset: usize,
        cw: f32,
        ch: f32,
        visible_rows: usize,
        queue: &wgpu::Queue,
        decor_instances: &mut Vec<DecorInstance>,
        glyph_instances: &mut Vec<GlyphInstance>,
    ) {
        let sb_len = terminal.scrollback.len();
        let border_thickness = 2.0_f32;
        let pad_left = self.content_padding[3];
        let pad_top = self.content_padding[0];

        // ── Annotations: bordered rectangles with labels ──
        for ann in &terminal.overlays.annotations {
            let display_row = ann.row as isize - (sb_len as isize - scroll_offset as isize);
            let display_end = display_row + ann.height as isize;

            // Skip if entirely off-screen
            if display_end < 0 || display_row >= visible_rows as isize {
                continue;
            }

            let x = pad_left + ann.col as f32 * cw;
            let y = pad_top + display_row as f32 * ch;
            let w = ann.width as f32 * cw;
            let h = ann.height as f32 * ch;

            // Top border
            decor_instances.push(DecorInstance {
                pos: [x, y],
                size: [w, border_thickness],
                color: ann.color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
            // Bottom border
            decor_instances.push(DecorInstance {
                pos: [x, y + h - border_thickness],
                size: [w, border_thickness],
                color: ann.color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
            // Left border
            decor_instances.push(DecorInstance {
                pos: [x, y],
                size: [border_thickness, h],
                color: ann.color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
            // Right border
            decor_instances.push(DecorInstance {
                pos: [x + w - border_thickness, y],
                size: [border_thickness, h],
                color: ann.color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });

            // Label text above the region
            if !ann.label.is_empty() && display_row > 0 {
                let label_y = (display_row - 1) as usize;
                self.render_text_at(
                    &ann.label,
                    ann.col,
                    label_y,
                    cw, ch,
                    [pad_left, pad_top],
                    ann.color,
                    1.0,
                    0, 0, // no shimmer
                    0.0,
                    queue,
                    glyph_instances,
                );
            }
        }

        // ── Charts: bar charts rendered as filled rectangles ──
        for chart in &terminal.overlays.charts {
            let display_row = chart.row as isize - (sb_len as isize - scroll_offset as isize);
            let display_end = display_row + chart.height as isize;

            if display_end < 0 || display_row >= visible_rows as isize {
                continue;
            }

            let chart_x = pad_left + chart.col as f32 * cw;
            let chart_y = pad_top + display_row as f32 * ch;
            let chart_w = chart.width as f32 * cw;
            let chart_h = chart.height as f32 * ch;
            let num_values = chart.values.len();

            if num_values == 0 {
                continue;
            }

            match chart.chart_type {
                immorterm_core::overlays::ChartType::Bar => {
                    let bar_width = chart_w / num_values as f32;
                    let gap = (bar_width * 0.1).max(1.0);

                    for (i, &val) in chart.values.iter().enumerate() {
                        let bar_h = val * chart_h;
                        let bar_x = chart_x + i as f32 * bar_width + gap;
                        let bar_y = chart_y + chart_h - bar_h;

                        decor_instances.push(DecorInstance {
                            pos: [bar_x, bar_y],
                            size: [(bar_width - gap * 2.0).max(1.0), bar_h],
                            color: chart.color,
                            extra: [decor_style::SOLID, 0.0],
                            _pad: [0.0, 0.0],
                        });
                    }
                }
                immorterm_core::overlays::ChartType::Sparkline => {
                    // Sparkline: connected line segments
                    let step = if num_values > 1 {
                        chart_w / (num_values - 1) as f32
                    } else {
                        chart_w
                    };
                    let line_thickness = 2.0_f32;

                    for i in 0..num_values.saturating_sub(1) {
                        let x1 = chart_x + i as f32 * step;
                        let y1 = chart_y + chart_h - chart.values[i] * chart_h;
                        let x2 = chart_x + (i + 1) as f32 * step;
                        let y2 = chart_y + chart_h - chart.values[i + 1] * chart_h;

                        // Approximate line with a thin rectangle
                        let min_y = y1.min(y2);
                        let max_y = y1.max(y2);
                        let seg_h = (max_y - min_y).max(line_thickness);

                        decor_instances.push(DecorInstance {
                            pos: [x1, min_y],
                            size: [x2 - x1, seg_h],
                            color: chart.color,
                            extra: [decor_style::SOLID, 0.0],
                            _pad: [0.0, 0.0],
                        });
                    }
                }
            }
        }
    }

    /// Render AI canvas layer primitives using existing GPU pipelines.
    ///
    /// Each primitive maps to existing instance types:
    /// - AiRect   → BgInstance (fill) + DecorInstance (border)
    /// - AiText   → GlyphInstance via render_text_at()
    /// - AiButton → BgInstance (fill, brighter when hovered) + GlyphInstance + DecorInstance
    /// - AiLine   → DecorInstance (thin rectangle)
    ///
    /// Call `terminal.ai_layer.tick_animations(time)` from the platform layer
    /// BEFORE calling render() — the renderer reads already-interpolated state.
    #[allow(clippy::too_many_arguments)]
    fn render_ai_layer(
        &mut self,
        terminal: &Terminal,
        cw: f32,
        ch: f32,
        time: f32,
        scroll_offset: usize,
        sb_len: usize,
        _pad_top: f32,
        _pad_left: f32,
        queue: &wgpu::Queue,
        _bg_instances: &mut Vec<BgInstance>,
        glyph_instances: &mut Vec<GlyphInstance>,
        decor_instances: &mut Vec<DecorInstance>,
    ) {
        use immorterm_core::ai_layer::{AnchorMode, AiPrimitiveKind};

        let border_thickness_default = 2.0_f32;

        for prim in &terminal.ai_layer.primitives {
            if !prim.visible || prim.alpha <= 0.0 {
                continue;
            }

            let alpha_mul = prim.alpha;

            // Compute y offset for scroll-anchored primitives
            let adj_y = match &prim.anchor {
                AnchorMode::Fixed => 0.0,
                AnchorMode::Scroll { scrollback_at_creation } => {
                    let new_lines = sb_len.saturating_sub(*scrollback_at_creation);
                    let scroll_back = scroll_offset as f32 * ch;
                    -(new_lines as f32 * ch) + scroll_back
                }
            };

            match &prim.kind {
                AiPrimitiveKind::Rect(rect) => {
                    let ry = rect.y + adj_y;
                    let fill_color = [
                        rect.color[0],
                        rect.color[1],
                        rect.color[2],
                        rect.color[3] * alpha_mul,
                    ];
                    decor_instances.push(DecorInstance {
                        pos: [rect.x, ry],
                        size: [rect.width, rect.height],
                        color: fill_color,
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });

                    // Border (if specified)
                    if let Some(bc) = rect.border_color {
                        let bw = if rect.border_width > 0.0 {
                            rect.border_width
                        } else {
                            border_thickness_default
                        };
                        let bc_alpha = [bc[0], bc[1], bc[2], bc[3] * alpha_mul];
                        // Top
                        decor_instances.push(DecorInstance {
                            pos: [rect.x, ry],
                            size: [rect.width, bw],
                            color: bc_alpha,
                            extra: [decor_style::SOLID, 0.0],
                            _pad: [0.0, 0.0],
                        });
                        // Bottom
                        decor_instances.push(DecorInstance {
                            pos: [rect.x, ry + rect.height - bw],
                            size: [rect.width, bw],
                            color: bc_alpha,
                            extra: [decor_style::SOLID, 0.0],
                            _pad: [0.0, 0.0],
                        });
                        // Left
                        decor_instances.push(DecorInstance {
                            pos: [rect.x, ry],
                            size: [bw, rect.height],
                            color: bc_alpha,
                            extra: [decor_style::SOLID, 0.0],
                            _pad: [0.0, 0.0],
                        });
                        // Right
                        decor_instances.push(DecorInstance {
                            pos: [rect.x + rect.width - bw, ry],
                            size: [bw, rect.height],
                            color: bc_alpha,
                            extra: [decor_style::SOLID, 0.0],
                            _pad: [0.0, 0.0],
                        });
                    }
                }

                AiPrimitiveKind::Text(text) => {
                    // Convert pixel position to nearest cell for render_text_at
                    let col = (text.x / cw) as usize;
                    let row = ((text.y + adj_y) / ch) as usize;
                    let color = [
                        text.color[0],
                        text.color[1],
                        text.color[2],
                        text.color[3] * alpha_mul,
                    ];
                    self.render_text_at(
                        &text.text,
                        col,
                        row,
                        cw, ch,
                        [0.0, 0.0], // AI layer uses absolute canvas coords
                        color,
                        1.0,
                        0, 0, // no shimmer
                        time,
                        queue,
                        glyph_instances,
                    );
                }

                AiPrimitiveKind::Button(btn) => {
                    let by = btn.y + adj_y;
                    // Background fill (brighter when hovered)
                    let hover_boost = if btn.hovered { 1.4 } else { 1.0 };
                    let bg_color = [
                        (btn.bg_color[0] * hover_boost).min(1.0),
                        (btn.bg_color[1] * hover_boost).min(1.0),
                        (btn.bg_color[2] * hover_boost).min(1.0),
                        btn.bg_color[3] * alpha_mul,
                    ];
                    decor_instances.push(DecorInstance {
                        pos: [btn.x, by],
                        size: [btn.width, btn.height],
                        color: bg_color,
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });

                    // Border
                    let bw = 1.0_f32;
                    let border_color = [
                        (btn.bg_color[0] * 1.8).min(1.0),
                        (btn.bg_color[1] * 1.8).min(1.0),
                        (btn.bg_color[2] * 1.8).min(1.0),
                        btn.bg_color[3] * alpha_mul,
                    ];
                    // Top
                    decor_instances.push(DecorInstance {
                        pos: [btn.x, by],
                        size: [btn.width, bw],
                        color: border_color,
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });
                    // Bottom
                    decor_instances.push(DecorInstance {
                        pos: [btn.x, by + btn.height - bw],
                        size: [btn.width, bw],
                        color: border_color,
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });
                    // Left
                    decor_instances.push(DecorInstance {
                        pos: [btn.x, by],
                        size: [bw, btn.height],
                        color: border_color,
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });
                    // Right
                    decor_instances.push(DecorInstance {
                        pos: [btn.x + btn.width - bw, by],
                        size: [bw, btn.height],
                        color: border_color,
                        extra: [decor_style::SOLID, 0.0],
                        _pad: [0.0, 0.0],
                    });

                    // Centered text
                    let text_width = btn.text.chars().count() as f32 * cw;
                    let text_x = btn.x + (btn.width - text_width) / 2.0;
                    let text_y = by + (btn.height - ch) / 2.0;
                    let col = (text_x / cw) as usize;
                    let row = (text_y / ch) as usize;
                    let text_color = [
                        btn.text_color[0],
                        btn.text_color[1],
                        btn.text_color[2],
                        btn.text_color[3] * alpha_mul,
                    ];
                    self.render_text_at(
                        &btn.text,
                        col,
                        row,
                        cw, ch,
                        [0.0, 0.0], // AI layer uses absolute canvas coords
                        text_color,
                        1.0,
                        0, 0, // no shimmer
                        0.0,
                        queue,
                        glyph_instances,
                    );
                }

                AiPrimitiveKind::Line(line) => {
                    // Approximate line as a thin rectangle
                    let dx = line.x2 - line.x1;
                    let dy = line.y2 - line.y1;
                    let ly1 = line.y1 + adj_y;
                    let ly2 = line.y2 + adj_y;
                    let thickness = line.thickness;
                    let color = [
                        line.color[0],
                        line.color[1],
                        line.color[2],
                        line.color[3] * alpha_mul,
                    ];

                    if dx.abs() >= dy.abs() {
                        // More horizontal — use width = |dx|, height = thickness
                        let (sx, ex) = if dx >= 0.0 {
                            (line.x1, line.x2)
                        } else {
                            (line.x2, line.x1)
                        };
                        let min_y = ly1.min(ly2);
                        let max_y = ly1.max(ly2);
                        let mid_y = (min_y + max_y) / 2.0 - thickness / 2.0;
                        decor_instances.push(DecorInstance {
                            pos: [sx, mid_y],
                            size: [ex - sx, (max_y - min_y).max(thickness)],
                            color,
                            extra: [decor_style::SOLID, 0.0],
                            _pad: [0.0, 0.0],
                        });
                    } else {
                        // More vertical — use height = |dy|, width = thickness
                        let (sy, ey) = if dy >= 0.0 {
                            (ly1, ly2)
                        } else {
                            (ly2, ly1)
                        };
                        let min_x = line.x1.min(line.x2);
                        let max_x = line.x1.max(line.x2);
                        let mid_x = (min_x + max_x) / 2.0 - thickness / 2.0;
                        decor_instances.push(DecorInstance {
                            pos: [mid_x, sy],
                            size: [(max_x - min_x).max(thickness), ey - sy],
                            color,
                            extra: [decor_style::SOLID, 0.0],
                            _pad: [0.0, 0.0],
                        });
                    }
                }

                // Html primitives are rendered purely as DOM overlays by the webview —
                // no GPU rendering needed.
                AiPrimitiveKind::Html(_) => {}
            }
        }
    }

    /// Render the status bar as one row below the terminal grid.
    /// Smooth gradient background (interpolated across all columns) with
    /// per-section FG colors matching the C version's layout.
    ///
    /// When `status_bar_reveal < 1.0`, the bar is partially hidden:
    /// background and text fade (alpha) and slide downward, creating a
    /// smooth reveal/hide animation without triggering PTY resize.
    #[allow(clippy::too_many_arguments)]
    fn render_status_bar(
        &mut self,
        data: &StatusBarData,
        display_row: usize,
        cw: f32,
        ch: f32,
        cell_offset: [f32; 2],   // [pad_cell_x, pad_cell_y] — fractional cell offset for BgInstance
        pixel_offset: [f32; 2],  // [pad_left, pad_top] — pixel offset for glyph/text positioning
        time: f32,
        queue: &wgpu::Queue,
        bg_instances: &mut Vec<BgInstance>,
        glyph_instances: &mut Vec<GlyphInstance>,
    ) {
        let reveal = self.status_bar_reveal;
        // Skip rendering entirely when fully hidden
        if reveal <= 0.001 {
            return;
        }

        let cols = data.cols;
        // Cover full canvas width so status bar spans edge-to-edge (no padding gaps).
        let full_cols = (self.width / cw).ceil() as usize + 1;
        let wave_offset = statusbar::gradient_wave_offset(time);
        let breathing = statusbar::breathing_factor(time);

        // Slide offset: bar slides down as reveal decreases (0 = one row below its position)
        let slide_cells = 1.0 - reveal;

        let content_h = self.height - self.content_padding[0] - self.content_padding[2];

        // ── Background: smooth gradient across full width + breathing + hover ──
        for col in 0..full_cols {
            let t = if full_cols > 1 {
                col as f32 / (full_cols - 1) as f32
            } else {
                0.5
            };
            let mut color = statusbar::gradient_color_7stop(
                t,
                wave_offset,
                &data.gradient_stops,
            );
            color[0] *= breathing;
            color[1] *= breathing;
            color[2] *= breathing;

            // Hover highlight: brighten the hovered section
            let hovered = match data.hovered_target {
                StatusBarTarget::Brand => {
                    // Extend to full canvas width — brand is rightmost, highlight
                    // should cover the padding gap to the right edge.
                    col >= data.brand_start_col
                }
                StatusBarTarget::AiStats => {
                    data.ai_stats_end_col > data.ai_stats_start_col
                        && col >= data.ai_stats_start_col
                        && col < data.ai_stats_end_col
                }
                StatusBarTarget::ThemeArea => {
                    data.theme_area_end_col > data.theme_area_start_col
                        && col >= data.theme_area_start_col
                        && col < data.theme_area_end_col
                }
                StatusBarTarget::Scratch => {
                    data.scratch_end_col > data.scratch_start_col
                        && col >= data.scratch_start_col
                        && col < data.scratch_end_col
                }
                StatusBarTarget::Title => {
                    data.title_end_col > data.title_start_col
                        && col >= data.title_start_col
                        && col < data.title_end_col
                }
                StatusBarTarget::Project => {
                    data.project_end_col > data.project_start_col
                        && col >= data.project_start_col
                        && col < data.project_end_col
                }
                StatusBarTarget::None => false,
            };
            if hovered {
                color[0] = (color[0] * 1.4).min(1.0);
                color[1] = (color[1] * 1.4).min(1.0);
                color[2] = (color[2] * 1.4).min(1.0);
            }

            // Apply reveal: fade alpha + slide Y position
            color[3] *= reveal;
            bg_instances.push(BgInstance {
                pos: [cell_offset[0] + col as f32, cell_offset[1] + display_row as f32 + slide_cells],
                color,
            });
            // Extension: tile quads below the main bar until we overshoot the canvas
            // bottom. GPU clips at the edge → zero black strip. We need multiple
            // quads because floor() in resize() can leave a fractional-cell gap.
            let canvas_bottom_cell = self.height / ch;
            let mut ext_y = cell_offset[1] + display_row as f32 + 1.0;
            while ext_y < canvas_bottom_cell {
                bg_instances.push(BgInstance {
                    pos: [cell_offset[0] + col as f32, ext_y + slide_cells],
                    color,
                });
                ext_y += 1.0;
            }
        }

        // ── Text vertical centering ──
        // The bar occupies: content_h - display_row * ch pixels (1 reserved row + fractional gap).
        // Center the cell-height text line in that space; the +ch*0.1 compensates for
        // glyph ascent sitting above the cell's vertical midpoint.
        let bar_height = content_h - display_row as f32 * ch;
        let gap_center_nudge = (bar_height - ch) / 2.0 + ch * 0.1;

        // Slide offset in pixels for text (matches bg slide)
        let slide_px = slide_cells * ch;
        let text_offset = [pixel_offset[0], pixel_offset[1] + gap_center_nudge + slide_px];

        // ── Text: left sections (project / title [dot]) ──
        let mut col_cursor = 0usize;
        for (i, section) in data.left_sections.iter().enumerate() {
            let mut fg = section.fg;
            fg[3] *= reveal;
            // Apply sub-pixel shift to the title section (index 2) for smooth marquee
            let section_offset = if i == 2 && data.title_pixel_shift > 0.0 {
                [text_offset[0] - data.title_pixel_shift * cw, text_offset[1]]
            } else {
                text_offset
            };
            self.render_text_at(
                &section.text,
                col_cursor,
                display_row,
                cw,
                ch,
                section_offset,
                fg,
                1.0,
                0,
                0,
                time,
                queue,
                glyph_instances,
            );
            col_cursor += section.text.chars().count();
        }

        // CTX bar is rendered as colored text (▰▱) in center_sections — no BgInstance needed.

        // ── Text: center sections (AI stats — centered) ──
        let mut center_cursor = data.ai_stats_start_col;
        for section in &data.center_sections {
            let mut fg = section.fg;
            fg[3] *= reveal;
            self.render_text_at(
                &section.text,
                center_cursor,
                display_row,
                cw,
                ch,
                text_offset,
                fg,
                1.0,
                0,
                0,
                time,
                queue,
                glyph_instances,
            );
            center_cursor += section.text.chars().count();
        }

        // ── Text: right sections (Last Active, time, brand) ──
        let right_total: usize = data
            .right_sections
            .iter()
            .map(|s| s.text.chars().count())
            .sum();
        let right_start_col = cols.saturating_sub(right_total);
        let mut right_cursor = right_start_col;

        for section in &data.right_sections {
            let is_brand = right_cursor >= data.brand_start_col;
            let mut fg = section.fg;
            fg[3] *= reveal;

            // Soft drop shadow on the brand: stack 4 low-alpha passes at slight
            // pixel offsets to fake a gaussian falloff. No shimmer on shadow
            // glyphs — only the foreground gets the highlight pass.
            if is_brand {
                let shadow_alpha = 0.30 * reveal;
                for &(dx, dy) in &[(1.0, 1.0), (2.0, 1.5), (1.0, 2.0), (2.0, 2.0)] {
                    let shadow_offset = [text_offset[0] + dx, text_offset[1] + dy];
                    self.render_text_at(
                        &section.text,
                        right_cursor,
                        display_row,
                        cw,
                        ch,
                        shadow_offset,
                        [0.0, 0.0, 0.0, shadow_alpha],
                        1.0,
                        0,
                        0,
                        time,
                        queue,
                        glyph_instances,
                    );
                }
            }

            self.render_text_at(
                &section.text,
                right_cursor,
                display_row,
                cw,
                ch,
                text_offset,
                fg,
                1.0,
                if is_brand { data.brand_start_col } else { 0 },
                if is_brand { data.brand_end_col } else { 0 },
                time,
                queue,
                glyph_instances,
            );
            right_cursor += section.text.chars().count();
        }
    }

    /// Render a popup menu above the status bar.
    #[allow(clippy::too_many_arguments)]
    fn render_popup(
        &mut self,
        popup: &PopupRenderData,
        visible_rows: usize,
        cw: f32,
        ch: f32,
        cell_offset: [f32; 2],   // [pad_cell_x, pad_cell_y]
        pixel_offset: [f32; 2],  // [pad_left, pad_top]
        accent: [f32; 4],
        gradient_start: [f32; 3],
        queue: &wgpu::Queue,
        bg_instances: &mut Vec<BgInstance>,
        glyph_instances: &mut Vec<GlyphInstance>,
        decor_instances: &mut Vec<DecorInstance>,
    ) {
        if !popup.visible || popup.items.is_empty() {
            return;
        }

        let (row_start, row_end) = popup.row_range(visible_rows);
        let col_start = popup.anchor_col;
        let col_end = col_start + popup.width_cols;

        // Dark background derived from theme's gradient_start (darkened)
        let bg_color: [f32; 4] = [
            gradient_start[0] * 0.4,
            gradient_start[1] * 0.4,
            gradient_start[2] * 0.4,
            0.95,
        ];
        for row in row_start..row_end {
            for col in col_start..col_end {
                bg_instances.push(BgInstance {
                    pos: [cell_offset[0] + col as f32, cell_offset[1] + row as f32],
                    color: bg_color,
                });
            }
        }

        // Border (theme accent color)
        let border_color = accent;
        let border_thick = 2.0_f32;
        let px_x = pixel_offset[0] + col_start as f32 * cw;
        let px_y = pixel_offset[1] + row_start as f32 * ch;
        let px_w = popup.width_cols as f32 * cw;
        let px_h = (row_end - row_start) as f32 * ch;

        // Top border
        decor_instances.push(DecorInstance {
            pos: [px_x, px_y],
            size: [px_w, border_thick],
            color: border_color,
            extra: [decor_style::SOLID, 0.0],
            _pad: [0.0, 0.0],
        });
        // Bottom border
        decor_instances.push(DecorInstance {
            pos: [px_x, px_y + px_h - border_thick],
            size: [px_w, border_thick],
            color: border_color,
            extra: [decor_style::SOLID, 0.0],
            _pad: [0.0, 0.0],
        });
        // Left border
        decor_instances.push(DecorInstance {
            pos: [px_x, px_y],
            size: [border_thick, px_h],
            color: border_color,
            extra: [decor_style::SOLID, 0.0],
            _pad: [0.0, 0.0],
        });
        // Right border
        decor_instances.push(DecorInstance {
            pos: [px_x + px_w - border_thick, px_y],
            size: [border_thick, px_h],
            color: border_color,
            extra: [decor_style::SOLID, 0.0],
            _pad: [0.0, 0.0],
        });

        // Menu items
        let padding = 1;
        for (i, item) in popup.items.iter().enumerate() {
            let item_row = row_start + padding + i;

            // Highlight selected row (theme gradient_start brightened)
            if i == popup.selected_index {
                let highlight_color: [f32; 4] = [
                    gradient_start[0] * 1.5 + 0.1,
                    gradient_start[1] * 1.5 + 0.05,
                    gradient_start[2] * 1.5 + 0.1,
                    1.0,
                ];
                for col in col_start..col_end {
                    bg_instances.push(BgInstance {
                        pos: [cell_offset[0] + col as f32, cell_offset[1] + item_row as f32],
                        color: highlight_color,
                    });
                }
            }

            // Item text
            let prefix = if item.checked { " \u{2713} " } else { "   " };
            let text = format!("{}{}", prefix, item.label);
            let fg = if item.enabled {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                [0.5, 0.5, 0.5, 1.0]
            };

            self.render_text_at(
                &text,
                col_start + 1,
                item_row,
                cw,
                ch,
                pixel_offset,
                fg,
                1.0,
                0,
                0,
                0.0,
                queue,
                glyph_instances,
            );

            // Separator line after item
            if item.separator_after {
                let sep_y = pixel_offset[1] + (item_row + 1) as f32 * ch - 1.0;
                decor_instances.push(DecorInstance {
                    pos: [px_x + 4.0, sep_y],
                    size: [px_w - 8.0, 1.0],
                    color: [accent[0] * 0.6, accent[1] * 0.6, accent[2] * 0.6, 0.6],
                    extra: [decor_style::SOLID, 0.0],
                    _pad: [0.0, 0.0],
                });
            }
        }
    }

    /// Render a text string at a given column/row position into glyph instances.
    /// `shimmer_start`/`shimmer_end` define the column range where shimmer is applied.
    #[allow(clippy::too_many_arguments)]
    fn render_text_at(
        &mut self,
        text: &str,
        start_col: usize,
        display_row: usize,
        cw: f32,
        ch: f32,
        pixel_offset: [f32; 2],
        base_color: [f32; 4],
        _brightness: f32,
        shimmer_start: usize,
        shimmer_end: usize,
        time: f32,
        queue: &wgpu::Queue,
        glyph_instances: &mut Vec<GlyphInstance>,
    ) {
        for (i, ch_char) in text.chars().enumerate() {
            let col = start_col + i;
            if ch_char == ' ' || ch_char == '\0' {
                continue;
            }

            let cell_x = pixel_offset[0] + col as f32 * cw;
            let cell_y = pixel_offset[1] + display_row as f32 * ch;

            // Apply shimmer brightness to brand text columns
            let shimmer = if shimmer_start < shimmer_end {
                statusbar::shimmer_brightness(col, shimmer_start, shimmer_end, time)
            } else {
                1.0
            };

            let color = [
                (base_color[0] * shimmer).min(1.0),
                (base_color[1] * shimmer).min(1.0),
                (base_color[2] * shimmer).min(1.0),
                base_color[3],
            ];

            if let Some(entry) = self.atlas.get_glyph(ch_char, CellAttrs::empty(), queue)
                && entry.size[0] > 0.0 {
                    glyph_instances.push(GlyphInstance {
                        pos: [cell_x + entry.offset[0], cell_y + entry.offset[1]],
                        size: entry.size,
                        uv_pos: entry.uv_pos,
                        uv_size: entry.uv_size,
                        color,
                        is_color: if entry.is_color { 1.0 } else { 0.0 },
                    });
                }
        }
    }

    /// Render a single row into the instance buffers.
    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &mut self,
        display_row: usize,
        content_idx: usize,
        sb_len: usize,
        row: &Row,
        visible_cols: usize,
        cw: f32,
        ch: f32,
        blink_visible: bool,
        underline_thickness: f32,
        underline_y_offset: f32,
        strikethrough_y_offset: f32,
        selections: &[Selection],
        pseudo_selections: &[Selection],
        popup_rect: Option<(usize, usize, usize, usize)>,
        expression_colors: &std::collections::HashMap<(usize, usize), [f32; 4]>,
        global_expression: ExpressionMeta,
        combining_marks: &immorterm_core::CombiningMarks,
        queue: &wgpu::Queue,
        bg_instances: &mut Vec<BgInstance>,
        glyph_instances: &mut Vec<GlyphInstance>,
        decor_instances: &mut Vec<DecorInstance>,
    ) {
        // Content padding offsets for this row
        let pad_left = self.content_padding[3];
        let pad_top = self.content_padding[0];
        let pad_cell_x = pad_left / cw;
        let pad_cell_y = pad_top / ch;

        // BiDi: fetch cached reordering for this row (if any)
        let bidi = self.bidi_cache.get(content_idx).and_then(|c| c.as_ref());
        let align_offset_px = bidi.map(|b| b.alignment_offset_px).unwrap_or(0.0);
        let align_offset_cells = align_offset_px / cw;

        // ── Run-based RTL rendering (disabled — per-character centering is stable) ──
        // Run rendering renders contiguous RTL sequences as single wide glyphs with
        // natural kerning. Currently disabled because it produces visual artifacts
        // in mixed Hebrew/English lines. The infrastructure (RunRasterizer, rtl_runs,
        // run_covered_scratch) is preserved for future activation via Phase 6 controls.
        self.run_covered_scratch.clear();
        self.run_covered_scratch.resize(visible_cols, false);

        for (col_idx, cell) in row.cells.iter().enumerate().take(visible_cols) {
            if cell.width == 0 {
                continue;
            }

            // Map logical column to visual position via BiDi reordering
            let visual_col = bidi
                .and_then(|b| b.logical_to_visual.get(col_idx).copied())
                .unwrap_or(col_idx);

            // Skip cells occluded by popup menu (use visual position for occlusion test)
            if let Some((pc_start, pc_end, pr_start, pr_end)) = popup_rect
                && display_row >= pr_start
                    && display_row < pr_end
                    && visual_col >= pc_start
                    && visual_col < pc_end
                {
                    continue;
                }

            let cell_x = pad_left + align_offset_px + visual_col as f32 * cw;
            let cell_y = pad_top + display_row as f32 * ch;
            let cell_w = cell.width as f32 * cw;

            let (fg_color, bg_color) = if cell.attrs.contains(CellAttrs::INVERSE) {
                (
                    self.theme.resolve_bg(&cell.fg),
                    self.theme.resolve_fg(&cell.bg),
                )
            } else {
                (
                    self.theme.resolve_fg(&cell.fg),
                    self.theme.resolve_bg(&cell.bg),
                )
            };

            let fg_color = if cell.attrs.contains(CellAttrs::DIM) {
                [fg_color[0], fg_color[1], fg_color[2], fg_color[3] * 0.5]
            } else {
                fg_color
            };

            // ── Expression effects (confidence, mood, danger, color override) ──
            // Use per-cell expression if set, otherwise fall back to global expression.
            // This ensures `express()` retroactively colors all visible cells, not just
            // future PTY output.
            let effective_expression = if !cell.expression.is_none() {
                cell.expression
            } else {
                global_expression
            };
            let (fg_color, expr_danger_bg) = if self.expression_effects && !effective_expression.is_none() {
                apply_expression(
                    &effective_expression,
                    fg_color,
                    expression_colors,
                    content_idx,
                    col_idx,
                )
            } else {
                (fg_color, None)
            };

            // Visual cell position in cell-unit coords (for bg shader)
            let visual_cell_x = pad_cell_x + align_offset_cells + visual_col as f32;
            let visual_cell_y = pad_cell_y + display_row as f32;

            // Push danger background glow if expression danger is active
            if let Some(danger_color) = expr_danger_bg {
                bg_instances.push(BgInstance {
                    pos: [visual_cell_x, visual_cell_y],
                    color: danger_color,
                });
            }

            // Cell's own background must be pushed BEFORE selection so the
            // translucent selection color blends on top via bg_pipeline alpha
            // blend. Order matters: with bg_pipeline alpha-blending enabled,
            // each later push composites over earlier ones. Reversing this
            // would let opaque cell bg overwrite the selection tint.
            if cell.bg != Color::Default || cell.attrs.contains(CellAttrs::INVERSE) {
                bg_instances.push(BgInstance {
                    pos: [visual_cell_x, visual_cell_y],
                    color: bg_color,
                });
            }

            // Selection highlight — pseudo-cursor and block selections use accent color,
            // regular line selections use theme.selection.
            // Note: selection.contains() uses logical col (selection is stored in logical coords).
            //
            // Push one bg_instance per pseudo-selection that contains the
            // cell, NOT just one when any matches — the bg_pipeline's
            // alpha blend then stacks each layer on top. Lets callers
            // brighten a specific range by overlapping it twice (used
            // by the Cmd+E bullet wizard's "active bullet" highlight).
            let pseudo_hits = pseudo_selections
                .iter()
                .filter(|s| s.contains(col_idx, content_idx))
                .count();
            if pseudo_hits > 0 {
                for _ in 0..pseudo_hits {
                    bg_instances.push(BgInstance {
                        pos: [visual_cell_x, visual_cell_y],
                        color: self.theme.pseudo_selection,
                    });
                }
            } else if selections.iter().any(|s| s.contains(col_idx, content_idx)) {
                // All selections use the accent-derived color for consistency
                bg_instances.push(BgInstance {
                    pos: [visual_cell_x, visual_cell_y],
                    color: self.theme.pseudo_selection,
                });
            }

            let blink_hide = cell.attrs.contains(CellAttrs::BLINK) && !blink_visible;
            if cell.grapheme != ' '
                && !cell.attrs.contains(CellAttrs::HIDDEN)
                && !blink_hide
            {
                // Block drawing characters (U+2580-U+259F) — render as pixel-perfect
                // filled rectangles instead of font glyphs. Font-based rendering of
                // block elements produces gaps and misalignment because font metrics
                // are optimized for letters, not full-cell fills.
                if push_block_rects(cell.grapheme, cell_x, cell_y, cell_w, ch, fg_color, decor_instances) {
                    // Handled — skip font-based rendering
                } else {
                    // BiDi mirroring (UAX #9 Rule L4): swap paired chars in RTL runs
                    let render_ch = if let Some(b) = bidi
                        && crate::bidi::is_in_rtl_run(b, col_idx)
                    {
                        crate::bidi::bidi_mirror(cell.grapheme)
                    } else {
                        cell.grapheme
                    };

                    // Check for combining marks (Hebrew niqqud, Arabic diacritics, etc.)
                    // The table is keyed by (grid_row, col) — live-grid rows only,
                    // marks are dropped when a row is evicted to scrollback. Keying
                    // by content_idx here silently missed every lookup once
                    // scrollback was non-empty.
                    //
                    // WASM (browser): mark shaping is DISABLED. Per-cell cluster
                    // rasterization centers each proportional RTL glyph in one
                    // monospace cell, which blows the letters up and detaches
                    // niqqud — worse than no marks. Correct RTL+mark rendering
                    // needs the dormant run-renderer (whole-word shaping), a
                    // separate project. Until then render bare base letters.
                    // Native keeps full cluster shaping (cosmic-text/swash).
                    #[cfg(target_arch = "wasm32")]
                    let marks: Option<&smallvec::SmallVec<[char; 4]>> = {
                        let _ = (sb_len, combining_marks); // only used by native shaping
                        None
                    };
                    #[cfg(not(target_arch = "wasm32"))]
                    let marks = content_idx
                        .checked_sub(sb_len)
                        .and_then(|grid_row| combining_marks.get(&(grid_row, col_idx)));
                    let entry = if let Some(marks) = marks {
                        self.atlas.get_glyph_cluster(
                            render_ch,
                            marks,
                            cell.attrs,
                            cell.width as usize,
                            queue,
                        )
                    } else {
                        self.atlas.get_glyph(render_ch, cell.attrs, queue)
                    };
                    if let Some(entry) = entry
                        && entry.size[0] > 0.0
                    {
                        glyph_instances.push(GlyphInstance {
                            pos: [cell_x + entry.offset[0], cell_y + entry.offset[1]],
                            size: entry.size,
                            uv_pos: entry.uv_pos,
                            uv_size: entry.uv_size,
                            color: fg_color,
                            is_color: if entry.is_color { 1.0 } else { 0.0 },
                        });
                    }
                }
            }

            // Decorations
            let ul_color = if cell.underline_color == Color::Default {
                fg_color
            } else {
                self.theme.resolve_fg(&cell.underline_color)
            };

            let underline_style = if cell.attrs.contains(CellAttrs::CURLY_UNDERLINE) {
                Some(decor_style::CURLY)
            } else if cell.attrs.contains(CellAttrs::DOUBLE_UNDERLINE) {
                Some(decor_style::DOUBLE)
            } else if cell.attrs.contains(CellAttrs::DOTTED_UNDERLINE) {
                Some(decor_style::DOTTED)
            } else if cell.attrs.contains(CellAttrs::DASHED_UNDERLINE) {
                Some(decor_style::DASHED)
            } else if cell.attrs.contains(CellAttrs::UNDERLINE) {
                Some(decor_style::SOLID)
            } else {
                None
            };

            if let Some(style) = underline_style {
                let (ul_h, ul_y) = if style == decor_style::CURLY {
                    (underline_thickness * 4.0, underline_y_offset - underline_thickness * 2.0)
                } else if style == decor_style::DOUBLE {
                    (underline_thickness * 3.0, underline_y_offset - underline_thickness)
                } else {
                    (underline_thickness, underline_y_offset)
                };

                decor_instances.push(DecorInstance {
                    pos: [cell_x, cell_y + ul_y],
                    size: [cell_w, ul_h],
                    color: ul_color,
                    extra: [style, 0.0],
                    _pad: [0.0, 0.0],
                });
            }

            if cell.attrs.contains(CellAttrs::STRIKETHROUGH) {
                decor_instances.push(DecorInstance {
                    pos: [cell_x, cell_y + strikethrough_y_offset],
                    size: [cell_w, underline_thickness],
                    color: fg_color,
                    extra: [decor_style::SOLID, 0.0],
                    _pad: [0.0, 0.0],
                });
            }
        }
    }

    /// Handle resize. Returns new terminal dimensions (cols, rows).
    /// When `status_bar_enabled`, one row is reserved for the status bar.
    /// Content padding is subtracted from the available area.
    pub fn resize(&mut self, _device: &wgpu::Device, width: u32, height: u32) -> (usize, usize) {
        self.width = width as f32;
        self.height = height as f32;
        // Invalidate BiDi cache — column count changed, all reordering stale
        self.bidi_cache.clear();

        let [pad_top, pad_right, pad_bottom, pad_left] = self.content_padding;
        let content_w = (width as f32 - pad_left - pad_right).max(0.0);
        let content_h = (height as f32 - pad_top - pad_bottom).max(0.0);

        let cw = self.atlas.metrics.cell_width;
        let ch = self.atlas.metrics.cell_height;

        let cols = (content_w / cw).floor() as usize;
        let mut rows = (content_h / ch).floor() as usize;

        // Reserve one row for the ~1.15× status bar; the fractional overshoot
        // past the cell boundary is covered by extension quads (GPU-clipped).
        if self.status_bar_enabled && rows > 1 {
            rows -= 1;
        }

        (cols.max(1), rows.max(1))
    }

    /// Set content padding in physical pixels [top, right, bottom, left].
    /// Insets terminal content while the window border remains at canvas edges.
    pub fn set_content_padding(&mut self, top: f32, right: f32, bottom: f32, left: f32) {
        self.content_padding = [top, right, bottom, left];
    }

    /// Set mouse proximity to scroll indicator (0.0 = far, 1.0 = on it).
    pub fn set_scroll_indicator_proximity(&mut self, proximity: f32) {
        self.scroll_indicator_proximity = proximity.clamp(0.0, 1.0);
    }

    /// Get cell metrics for the current font.
    pub fn cell_metrics(&self) -> (f32, f32) {
        (self.atlas.metrics.cell_width, self.atlas.metrics.cell_height)
    }

    /// Maximum scroll offset for the given terminal.
    pub fn max_scroll(terminal: &Terminal) -> usize {
        terminal.scrollback.len()
    }

    /// Render pane chrome (headers + borders) for a team view.
    ///
    /// Called AFTER all per-pane terminal content has been rendered.
    /// Uses full-surface coordinates (no scissor, no pane offset).
    pub fn render_pane_chrome(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface_view: &wgpu::TextureView,
        chrome: &[PaneChrome],
        surface_w: f32,
        surface_h: f32,
    ) {
        if chrome.is_empty() {
            return;
        }

        let cw = self.atlas.metrics.cell_width;
        let ch = self.atlas.metrics.cell_height;
        let time = self.start_time.elapsed().as_secs_f32();

        // Full-surface projection (no pane offset)
        let uniforms = Uniforms::ortho(surface_w, surface_h, cw, ch, time, self.font_thicken);
        queue.write_buffer(
            &self.pipeline.uniform_buffer,
            0,
            bytemuck::cast_slice(&[uniforms]),
        );

        let mut decor_instances: Vec<DecorInstance> = Vec::with_capacity(chrome.len() * 8);
        let mut glyph_instances: Vec<GlyphInstance> = Vec::with_capacity(chrome.len() * 40);

        let border_focused = 2.0_f32;
        let border_normal = 1.0_f32;
        let dim_border: [f32; 4] = [0.3, 0.3, 0.35, 1.0];

        for pane in chrome {
            let border_thick = if pane.is_focused {
                border_focused
            } else {
                border_normal
            };
            let border_color = if pane.is_focused {
                pane.accent_color
            } else {
                dim_border
            };

            // ── Header background ──
            // Darken the accent color for the header bg
            let header_bg = [
                pane.accent_color[0] * 0.25,
                pane.accent_color[1] * 0.25,
                pane.accent_color[2] * 0.25,
                0.95,
            ];
            decor_instances.push(DecorInstance {
                pos: [pane.x, pane.y],
                size: [pane.width, pane.header_height],
                color: header_bg,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });

            // ── Header text: "label  status" ──
            let text_padding = 6.0_f32;
            let text_y = pane.y + (pane.header_height - ch) / 2.0;

            // Member name (in accent color, bright)
            let name_color = if pane.is_focused {
                [
                    (pane.accent_color[0] * 1.5).min(1.0),
                    (pane.accent_color[1] * 1.5).min(1.0),
                    (pane.accent_color[2] * 1.5).min(1.0),
                    1.0,
                ]
            } else {
                [0.7, 0.7, 0.75, 1.0]
            };
            let mut text_x = pane.x + text_padding;
            for ch_char in pane.label.chars() {
                if ch_char != ' '
                    && let Some(entry) =
                        self.atlas
                            .get_glyph(ch_char, CellAttrs::empty(), queue)
                        && entry.size[0] > 0.0 {
                            glyph_instances.push(GlyphInstance {
                                pos: [text_x + entry.offset[0], text_y + entry.offset[1]],
                                size: entry.size,
                                uv_pos: entry.uv_pos,
                                uv_size: entry.uv_size,
                                color: name_color,
                                is_color: if entry.is_color { 1.0 } else { 0.0 },
                            });
                        }
                text_x += cw;
            }

            // Status indicator (right-aligned, dimmer)
            if !pane.status.is_empty() {
                let status_width = pane.status.chars().count() as f32 * cw;
                let status_x = pane.x + pane.width - status_width - text_padding;

                // Status dot color
                let status_dot_color = match pane.status.as_str() {
                    "Active" => [0.2, 0.9, 0.3, 1.0],         // green
                    "Idle" => [0.9, 0.7, 0.1, 1.0],           // yellow
                    "Done" => [0.4, 0.4, 0.5, 1.0],           // gray
                    "Disconnected" => [0.9, 0.2, 0.2, 1.0],   // red
                    "Reconnecting" => [0.9, 0.5, 0.1, 1.0],   // orange
                    _ => [0.5, 0.5, 0.5, 1.0],                // dim
                };

                // Small status dot before text
                let dot_size = 6.0_f32;
                let dot_x = status_x - dot_size - 4.0;
                let dot_y = pane.y + (pane.header_height - dot_size) / 2.0;
                decor_instances.push(DecorInstance {
                    pos: [dot_x, dot_y],
                    size: [dot_size, dot_size],
                    color: status_dot_color,
                    extra: [decor_style::SOLID, 0.0],
                    _pad: [0.0, 0.0],
                });

                // Status text
                let status_color = [0.55, 0.55, 0.6, 1.0];
                let mut sx = status_x;
                for ch_char in pane.status.chars() {
                    if ch_char != ' '
                        && let Some(entry) =
                            self.atlas
                                .get_glyph(ch_char, CellAttrs::empty(), queue)
                            && entry.size[0] > 0.0 {
                                glyph_instances.push(GlyphInstance {
                                    pos: [sx + entry.offset[0], text_y + entry.offset[1]],
                                    size: entry.size,
                                    uv_pos: entry.uv_pos,
                                    uv_size: entry.uv_size,
                                    color: status_color,
                                    is_color: if entry.is_color { 1.0 } else { 0.0 },
                                });
                            }
                    sx += cw;
                }
            }

            // ── Optional badge pill (e.g., "DELEGATE", "DONE") ──
            if let Some((badge_text, badge_color)) = &pane.badge {
                let badge_chars = badge_text.chars().count();
                let badge_text_w = badge_chars as f32 * cw;
                let badge_pad = 4.0_f32;
                let badge_h = ch * 0.8;
                let badge_w = badge_text_w + badge_pad * 2.0;

                // Position: after label, before status
                let label_w = pane.label.chars().count() as f32 * cw;
                let badge_x = pane.x + text_padding + label_w + 8.0;
                let badge_y = pane.y + (pane.header_height - badge_h) / 2.0;

                // Badge background pill
                decor_instances.push(DecorInstance {
                    pos: [badge_x, badge_y],
                    size: [badge_w, badge_h],
                    color: [badge_color[0] * 0.3, badge_color[1] * 0.3, badge_color[2] * 0.3, 0.9],
                    extra: [decor_style::SOLID, 0.0],
                    _pad: [0.0, 0.0],
                });

                // Badge text
                let mut bx = badge_x + badge_pad;
                for ch_char in badge_text.chars() {
                    if ch_char != ' '
                        && let Some(entry) =
                            self.atlas
                                .get_glyph(ch_char, CellAttrs::empty(), queue)
                            && entry.size[0] > 0.0 {
                                glyph_instances.push(GlyphInstance {
                                    pos: [bx + entry.offset[0], text_y + entry.offset[1]],
                                    size: entry.size,
                                    uv_pos: entry.uv_pos,
                                    uv_size: entry.uv_size,
                                    color: *badge_color,
                                    is_color: if entry.is_color { 1.0 } else { 0.0 },
                                });
                            }
                    bx += cw;
                }
            }

            // ── Pane border (4 edges around the total pane area) ──
            // Top
            decor_instances.push(DecorInstance {
                pos: [pane.x, pane.y],
                size: [pane.width, border_thick],
                color: border_color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
            // Bottom
            decor_instances.push(DecorInstance {
                pos: [pane.x, pane.y + pane.total_height - border_thick],
                size: [pane.width, border_thick],
                color: border_color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
            // Left
            decor_instances.push(DecorInstance {
                pos: [pane.x, pane.y],
                size: [border_thick, pane.total_height],
                color: border_color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
            // Right
            decor_instances.push(DecorInstance {
                pos: [pane.x + pane.width - border_thick, pane.y],
                size: [border_thick, pane.total_height],
                color: border_color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });

            // Header/content separator line
            let separator_y = pane.y + pane.header_height;
            decor_instances.push(DecorInstance {
                pos: [pane.x, separator_y],
                size: [pane.width, 1.0],
                color: border_color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
        }

        // Upload and render
        if !glyph_instances.is_empty() {
            queue.write_buffer(
                &self.pipeline.glyph_buffer,
                0,
                bytemuck::cast_slice(&glyph_instances),
            );
        }
        if !decor_instances.is_empty() {
            queue.write_buffer(
                &self.pipeline.decor_buffer,
                0,
                bytemuck::cast_slice(&decor_instances),
            );
        }

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pane_chrome_encoder"),
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pane_chrome_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: surface_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load, // preserve pane content
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            // No scissor — draw across full surface

            if !decor_instances.is_empty() {
                pass.set_pipeline(&self.pipeline.decor_pipeline);
                pass.set_bind_group(0, &self.pipeline.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, self.pipeline.decor_buffer.slice(..));
                pass.draw(0..6, 0..decor_instances.len() as u32);
            }

            if !glyph_instances.is_empty() {
                pass.set_pipeline(&self.pipeline.glyph_pipeline);
                pass.set_bind_group(0, &self.pipeline.uniform_bind_group, &[]);
                pass.set_bind_group(1, self.atlas.bind_group(), &[]);
                pass.set_vertex_buffer(0, self.pipeline.glyph_buffer.slice(..));
                pass.draw(0..6, 0..glyph_instances.len() as u32);
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
    }
}

/// Apply AI expression effects to a cell's foreground color.
///
/// Returns `(modified_fg_color, optional_danger_bg_color)`.
/// This is called per-cell only when `cell.expression.is_none()` is false,
/// ensuring zero overhead for normal (non-expression) cells.
///
/// Effects applied in order:
/// 1. **Color override** — if the expression has an explicit color, use it as FG (replaces mood tinting)
/// 2. **Mood tinting** — blend a mood-specific color into the existing FG (only if no color override)
/// 3. **Confidence alpha** — multiply FG alpha by the confidence value (dimmer = less confident)
/// 4. **Danger glow** — return a red-tinted background color to underpaint the cell
#[inline]
fn apply_expression(
    expression: &ExpressionMeta,
    fg_color: [f32; 4],
    expression_colors: &std::collections::HashMap<(usize, usize), [f32; 4]>,
    content_idx: usize,
    col_idx: usize,
) -> ([f32; 4], Option<[f32; 4]>) {
    let mut fg = fg_color;

    // 1. Color override — explicit FG color from expression_colors map
    if expression.has_color_override() {
        if let Some(override_color) = expression_colors.get(&(content_idx, col_idx)) {
            fg = *override_color;
        }
    } else {
        // 2. Mood tinting — blend mood color into FG (only when no color override)
        let mood_color: Option<[f32; 3]> = match expression.mood() {
            Mood::Neutral   => None,
            Mood::Confident => Some([0.4, 0.6, 1.0]),
            Mood::Cautious  => Some([1.0, 0.8, 0.3]),
            Mood::Creative  => Some([0.7, 0.4, 1.0]),
            Mood::Warning   => Some([1.0, 0.6, 0.1]),
            Mood::Error     => Some([1.0, 0.2, 0.2]),
            Mood::Success   => Some([0.2, 0.9, 0.3]),
            Mood::Excited   => Some([1.0, 0.3, 0.6]),
            Mood::Focused   => Some([0.2, 0.8, 0.9]),
            Mood::Playful   => Some([0.9, 0.3, 0.9]),
        };

        if let Some(mc) = mood_color {
            // Subtle tint — 15% blend so text remains readable against any bg
            const BLEND: f32 = 0.15;
            const INV_BLEND: f32 = 1.0 - BLEND;
            fg[0] = fg[0] * INV_BLEND + mc[0] * BLEND;
            fg[1] = fg[1] * INV_BLEND + mc[1] * BLEND;
            fg[2] = fg[2] * INV_BLEND + mc[2] * BLEND;
        }
    }

    // 3. Confidence alpha modulation
    if let Some(confidence) = expression.confidence() {
        fg[3] *= confidence;
    }

    // 4. Danger background glow (reduced intensity for inline marker use)
    let danger_bg = match expression.danger() {
        DangerLevel::None     => None,
        DangerLevel::Low      => Some([0.3,  0.0,  0.0,  0.08]),
        DangerLevel::Medium   => Some([0.4,  0.0,  0.0,  0.12]),
        DangerLevel::High     => Some([0.5,  0.05, 0.0,  0.15]),
        DangerLevel::Critical => Some([0.6,  0.1,  0.05, 0.25]),
    };

    (fg, danger_bg)
}

/// Render a block drawing character (U+2580–U+259F) as pixel-perfect filled rectangles.
///
/// Modern GPU terminals (Alacritty, WezTerm, Kitty) all special-case these characters
/// because font-based rendering produces gaps from baseline/metrics misalignment.
/// Returns `true` if the character was handled (caller should skip font rendering).
///
/// Shade characters (░▒▓) are NOT handled here — they fall through to font rendering.
fn push_block_rects(
    c: char,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [f32; 4],
    decor: &mut Vec<DecorInstance>,
) -> bool {
    // Pre-compute divisions with rounding to avoid sub-pixel gaps
    let hh = (h / 2.0).round();   // half height
    let hw = (w / 2.0).round();   // half width
    let e = h / 8.0;              // eighth height

    // Helper: push a solid filled rectangle
    macro_rules! rect {
        ($rx:expr, $ry:expr, $rw:expr, $rh:expr) => {
            decor.push(DecorInstance {
                pos: [$rx, $ry],
                size: [$rw, $rh],
                color,
                extra: [decor_style::SOLID, 0.0],
                _pad: [0.0, 0.0],
            });
        };
    }

    match c {
        // ── Full and half blocks ──
        '\u{2588}' => { rect!(x, y, w, h); }                           // █ Full block
        '\u{2580}' => { rect!(x, y, w, hh); }                          // ▀ Upper half
        '\u{2584}' => { rect!(x, y + hh, w, h - hh); }                 // ▄ Lower half
        '\u{258C}' => { rect!(x, y, hw, h); }                          // ▌ Left half
        '\u{2590}' => { rect!(x + hw, y, w - hw, h); }                 // ▐ Right half

        // ── Lower fractional blocks (1/8 to 7/8) ──
        '\u{2581}' => { let t = (7.0 * e).round(); rect!(x, y + t, w, h - t); }  // ▁ Lower 1/8
        '\u{2582}' => { let t = (6.0 * e).round(); rect!(x, y + t, w, h - t); }  // ▂ Lower 1/4
        '\u{2583}' => { let t = (5.0 * e).round(); rect!(x, y + t, w, h - t); }  // ▃ Lower 3/8
        '\u{2585}' => { let t = (3.0 * e).round(); rect!(x, y + t, w, h - t); }  // ▅ Lower 5/8
        '\u{2586}' => { let t = (2.0 * e).round(); rect!(x, y + t, w, h - t); }  // ▆ Lower 3/4
        '\u{2587}' => { let t = e.round();         rect!(x, y + t, w, h - t); }  // ▇ Lower 7/8

        // ── Left fractional blocks (1/8 to 7/8) ──
        '\u{2589}' => { let r = (7.0 * w / 8.0).round(); rect!(x, y, r, h); }    // ▉ Left 7/8
        '\u{258A}' => { let r = (6.0 * w / 8.0).round(); rect!(x, y, r, h); }    // ▊ Left 3/4
        '\u{258B}' => { let r = (5.0 * w / 8.0).round(); rect!(x, y, r, h); }    // ▋ Left 5/8
        '\u{258D}' => { let r = (3.0 * w / 8.0).round(); rect!(x, y, r, h); }    // ▍ Left 3/8
        '\u{258E}' => { let r = (2.0 * w / 8.0).round(); rect!(x, y, r, h); }    // ▎ Left 1/4
        '\u{258F}' => { let r = (w / 8.0).round().max(1.0); rect!(x, y, r, h); }  // ▏ Left 1/8

        // ── Upper and right eighth blocks ──
        '\u{2594}' => { let t = e.round().max(1.0); rect!(x, y, w, t); }          // ▔ Upper 1/8
        '\u{2595}' => { let r = (w / 8.0).round().max(1.0); rect!(x + w - r, y, r, h); } // ▕ Right 1/8

        // ── Quadrant characters (may need 2 rectangles) ──
        '\u{2596}' => { rect!(x, y + hh, hw, h - hh); }                           // ▖ Lower left
        '\u{2597}' => { rect!(x + hw, y + hh, w - hw, h - hh); }                  // ▗ Lower right
        '\u{2598}' => { rect!(x, y, hw, hh); }                                     // ▘ Upper left
        '\u{259D}' => { rect!(x + hw, y, w - hw, hh); }                            // ▝ Upper right

        '\u{2599}' => {  // ▙ Upper left + lower left + lower right (= left half + lower right)
            rect!(x, y, hw, h);
            rect!(x + hw, y + hh, w - hw, h - hh);
        }
        '\u{259A}' => {  // ▚ Upper left + lower right (diagonal quadrants)
            rect!(x, y, hw, hh);
            rect!(x + hw, y + hh, w - hw, h - hh);
        }
        '\u{259B}' => {  // ▛ Upper left + upper right + lower left (= top half + lower left)
            rect!(x, y, w, hh);
            rect!(x, y + hh, hw, h - hh);
        }
        '\u{259C}' => {  // ▜ Upper left + upper right + lower right (= top half + lower right)
            rect!(x, y, w, hh);
            rect!(x + hw, y + hh, w - hw, h - hh);
        }
        '\u{259E}' => {  // ▞ Upper right + lower left (anti-diagonal quadrants)
            rect!(x + hw, y, w - hw, hh);
            rect!(x, y + hh, hw, h - hh);
        }
        '\u{259F}' => {  // ▟ Upper right + lower left + lower right (= lower half + upper right)
            rect!(x, y + hh, w, h - hh);
            rect!(x + hw, y, w - hw, hh);
        }

        // Shade characters (░▒▓) and anything else — fall through to font rendering
        _ => return false,
    }

    true
}
