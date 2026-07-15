//! WASM browser target for ImmorTerm.
//!
//! Exposes a `WasmTerminal` that runs the same GPU renderer as native,
//! but on a browser `<canvas>` via WebGPU. The terminal processes raw
//! byte streams (ANSI escape sequences) and renders via wgpu.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// Register a font in the browser via the FontFace API so Canvas 2D can use it.
/// The data is copied out of WASM linear memory before the async load() resolves.
#[wasm_bindgen(inline_js = "
export async function register_font_face(name, data, weight, style) {
    try {
        const buf = new Uint8Array(data).buffer;
        const font = new FontFace(name, buf, { weight: weight || 'normal', style: style || 'normal' });
        await font.load();
        document.fonts.add(font);
        console.log(`[FontFace] Registered '${name}' (weight=${weight}, style=${style}, ${data.length} bytes)`);
    } catch (e) {
        console.error(`[FontFace] FAILED '${name}': ${e.message}`);
    }
}
")]
extern "C" {
    #[wasm_bindgen(js_name = "register_font_face")]
    async fn register_font_face(name: &str, data: &[u8], weight: &str, style: &str);
}

mod comments;

use self::comments::Comments;
use immorterm_core::cell::CellAttrs;
use immorterm_core::grid::Row;
use immorterm_core::Terminal;
use immorterm_render::statusbar::{self, StatusBarData, StatusBarTarget, StatusBarTheme, THEME_PRESETS};
use immorterm_render::{FallbackGlyph, MonoGlyph, RenderOptions, Selection, TerminalRenderer, Theme};

/// Detect whether a row boundary is a structural break (not a soft-wrap).
/// Called when `is_soft_wrapped()` heuristic says "join" — this overrides
/// that decision if structural signals say otherwise.
fn has_hard_break_signals(row: &Row, next_row: Option<&Row>) -> bool {
    // 1. Current row ends with box-drawing/decorative chars → hard break
    //    (heading separators like "★ Insight ─────────────────")
    if row.cells.iter().rev()
        .find(|c| c.width > 0 && c.grapheme != ' ')
        .map(|c| c.grapheme)
        .is_some_and(|ch| ('\u{2500}'..='\u{257F}').contains(&ch) // Box Drawing block
            || ch == '─' || ch == '═' || ch == '━')
    {
        return true;
    }

    // 2. Next row signals
    if let Some(nr) = next_row {
        // 2a. Next row is blank → paragraph boundary
        let next_has_content = nr.cells.iter()
            .any(|c| c.width > 0 && c.grapheme != ' ');
        if !next_has_content {
            return true;
        }

        // 2b. Next row starts with a structural marker → list item or section
        //     Collect the first few non-space chars on the next row.
        let leading: String = nr.cells.iter()
            .filter(|c| c.width > 0)
            .skip_while(|c| c.grapheme == ' ')
            .take(6)
            .map(|c| c.grapheme)
            .collect();

        // Bullet points: "- ", "* ", "+ ", "• ", "▸ "
        if leading.starts_with("- ") || leading.starts_with("* ")
            || leading.starts_with("+ ") || leading.starts_with("• ")
            || leading.starts_with("▸ ") || leading.starts_with("▹ ")
            || leading.starts_with("► ")
        {
            return true;
        }

        // Numbered list: "1. ", "2. ", "10. " (digits followed by ". ")
        if let Some(dot_pos) = leading.find(". ")
            && dot_pos > 0 && leading[..dot_pos].chars().all(|c| c.is_ascii_digit())
        {
            return true;
        }

        // Lettered list: "a) ", "b) ", "a. ", "b. ", "A) ", "A. "
        let chars: Vec<char> = leading.chars().collect();
        if chars.len() >= 2 && chars[0].is_ascii_alphabetic()
            && (chars[1] == ')' || (chars[1] == '.' && chars.get(2) == Some(&' ')))
        {
            return true;
        }
    }

    false
}

/// CSS font stack for Canvas 2D run/cluster rasterization.
///
/// Uses the terminal's own monospace stack (which includes the embedded
/// Heebo for Hebrew) so a cluster renders at the same metrics as every
/// other cell. Proportional system fonts (e.g. "Arial Hebrew") blow the
/// glyph up past the monospace cell and gap the whole line — the browser
/// still shapes the cluster and anchors niqqud/harakat via Heebo's GPOS.
/// `_text` retained for signature symmetry with the call sites.
fn run_font_stack(_text: &str, base_font: &str) -> String {
    base_font.to_string()
}

/// Embedded monospace fonts — JetBrains Mono family (~1MB total).
/// No system fonts in the browser, so we bundle all 4 standard variants
/// plus Noto Sans Symbols 2 (full) for standard Unicode symbols (Dingbats, Arrows,
/// Braille, Geometric Shapes, etc.), Nerd Font for PUA dev icons/powerline,
/// and Apple Symbols subset for Misc Technical + Math gaps (U+23BF etc).
/// Emoji are rendered via Canvas 2D fallback (accesses system color emoji fonts).
const FONT_REGULAR: &[u8] = include_bytes!("fonts/JetBrainsMono-Regular.ttf");
const FONT_ITALIC: &[u8] = include_bytes!("fonts/JetBrainsMono-Italic.ttf");
const FONT_BOLD: &[u8] = include_bytes!("fonts/JetBrainsMono-Bold.ttf");
const FONT_BOLD_ITALIC: &[u8] = include_bytes!("fonts/JetBrainsMono-BoldItalic.ttf");
const FONT_SYMBOLS: &[u8] = include_bytes!("fonts/NotoSansSymbols2-Regular.ttf");
const FONT_NERD: &[u8] = include_bytes!("fonts/SymbolsNerdFontMono-Regular.ttf");
const FONT_MISC: &[u8] = include_bytes!("fonts/AppleSymbols-Subset.ttf");
const FONT_HEBREW: &[u8] = include_bytes!("fonts/Heebo-Medium.ttf");
const FONT_HEBREW_BOLD: &[u8] = include_bytes!("fonts/Heebo-Bold.ttf");

/// Default font size in CSS pixels. Multiplied by devicePixelRatio
/// at init time so glyphs are crisp on HiDPI screens.
const DEFAULT_FONT_SIZE: f32 = 16.0;

/// GPU state — initialized asynchronously after canvas is available.
struct GpuState {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
}

/// Per-session terminal state, stored when a session is backgrounded.
/// The active session's state lives directly in WasmTerminal fields;
/// background sessions are parked here until restored.
struct BackgroundState {
    terminal: Terminal,
    scroll_offset: usize,
    selection: Selection,
    last_activity_ms: f64,
    ai_stats: String,
    ai_ctx_pct: f32,
    session_title: String,
    immorterm_id: String,
    /// Accumulated time for title marquee animation (seconds).
    title_marquee_time: f32,
    /// Cached title overflow chars for stable marquee cycle duration.
    title_marquee_overflow: usize,
    /// Staged inline comments — per-session. line_ids are tied to this
    /// session's scrollback net_shift so they MUST NOT bleed across tabs.
    comments: Comments,
}

/// Internal state — holds the live terminal, renderer, GPU, and all cached
/// settings. Not exposed to JS directly; the JS-facing wrapper `WasmTerminal`
/// below owns a `RefCell<WasmTerminalInner>` and delegates every method call
/// through a runtime borrow check that gracefully degrades on re-entrancy
/// (WebKit fires events synchronously mid-wasm call, which would otherwise
/// trigger wasm-bindgen's "recursive use of an object" panic — see TODO.md).
pub struct WasmTerminalInner {
    terminal: Terminal,
    renderer: Option<TerminalRenderer>,
    gpu: Option<GpuState>,
    /// Background session states — slot index is the ID returned to JS.
    background_states: Vec<Option<BackgroundState>>,
    cols: usize,
    rows: usize,
    width: u32,
    height: u32,
    scroll_offset: usize,
    /// Lines the user wanted to scroll but couldn't (local scrollback exhausted).
    /// Applied when `prepend_scrollback` delivers more rows from the daemon.
    scroll_deficit: usize,
    selection: Selection,
    /// Multi-cursor pseudo-cursors (Alt+Click to place, independent from terminal cursor)
    pseudo_cursors: Vec<Selection>,
    /// Device pixel ratio — needed to convert CSS mouse coords to physical pixels
    dpr: f32,
    /// Current status bar hover target (set by JS mousemove → set_status_bar_hover)
    status_bar_hover: StatusBarTarget,
    /// Active status bar theme
    status_bar_theme: StatusBarTheme,
    /// Cached status bar data from last render (for hit-testing between frames)
    last_status_bar: Option<StatusBarData>,
    /// Project name shown in status bar (left side)
    project_name: String,
    /// Session title shown in status bar (after project name)
    session_title: String,
    /// Font size in CSS pixels (from VS Code settings)
    font_size_css: f32,
    /// Line height multiplier from VS Code settings (default 1.2).
    /// VS Code's `terminal.integrated.lineHeight` value is factored in:
    /// actual_ratio = 1.2 × vs_code_lineHeight_setting
    line_height_ratio: f32,
    /// Custom font data from the host (e.g. the user's VS Code terminal font).
    /// If set, these are used instead of the embedded JetBrains Mono.
    /// Each Vec<u8> is a font file (TTF/OTF/TTC).
    custom_font_data: Vec<Vec<u8>>,
    /// Theme set before init_gpu() — applied to renderer after construction.
    pending_theme: Option<Theme>,
    /// Font weight set before init_gpu() — applied to atlas after construction.
    pending_font_weight: Option<u16>,
    /// Content padding in physical pixels [top, right, bottom, left].
    /// Terminal content is inset by this amount while the border stays at canvas edges.
    pending_content_padding: [f32; 4],
    /// Font family name explicitly provided by the host (e.g. "Menlo").
    /// Used instead of extracting from fontdb to ensure correct font matching.
    custom_font_name: Option<String>,
    /// AI stats string shown in status bar (e.g. "Opus 4 · $1.23 · 45% ctx")
    ai_stats: String,
    /// CTX usage percentage (0.0 = no bar, 1–100 = show progress bar behind center section)
    ai_ctx_pct: f32,
    /// Epoch millis (from js_sys::Date::now()) of last PTY data received.
    /// Used to display "Xs ago" / "Xm ago" in the status bar.
    last_activity_ms: f64,
    /// Border enabled state — buffered for pre-init calls.
    pending_border_enabled: bool,
    /// Border opacity (0.0–1.0) — buffered for pre-init calls.
    pending_border_opacity: f32,
    /// Animations enabled state — buffered for pre-init calls.
    pending_animations_enabled: bool,
    /// Expression effects enabled state — buffered for pre-init calls.
    pending_expression_effects: bool,
    /// Celebrations enabled state — buffered for pre-init calls.
    pending_celebrations: bool,
    /// Danger effects enabled state — buffered for pre-init calls.
    pending_danger_effects: bool,
    /// Text animations enabled state — buffered for pre-init calls.
    pending_text_animations: bool,
    /// Status bar visibility mode: "always", "auto", or "hidden".
    /// In "auto" mode the row is reserved but visual presence is driven by JS animation.
    status_bar_mode: String,
    /// The immorterm_id of the session currently rendered in this terminal.
    /// Used by `load_snapshot` to detect session changes and avoid preserving
    /// stale scrollback from a different session.
    immorterm_id: String,
    /// Accumulated time for title marquee animation (seconds).
    /// Reset when session title changes.
    title_marquee_time: f32,
    /// Cached title overflow chars for stable marquee cycle duration.
    /// Recalculated only when title or cols change, NOT when ai_stats toggles.
    title_marquee_overflow: usize,
    /// DOM-measured char height in CSS pixels (via hidden span with lineHeight: normal).
    /// Matches xterm.js CharSizeService exactly. When > 0, used instead of fontBoundingBox
    /// for cell height computation. Set from JS before init_gpu().
    char_height_css: f32,
    /// Staged inline comments awaiting send-with-next-prompt.
    /// See `src/comments.rs` for the data model and lifecycle.
    comments: Comments,
}

impl WasmTerminalInner {
    /// Create a new terminal with the given dimensions.
    /// Call `init_gpu()` after construction to set up WebGPU rendering.
    pub fn new(cols: usize, rows: usize) -> Self {
        console_error_panic_hook::set_once();

        let terminal = Terminal::new(cols, rows);

        Self {
            terminal,
            renderer: None,
            gpu: None,
            background_states: Vec::new(),
            cols,
            rows,
            width: 800,
            height: 600,
            scroll_offset: 0,
            scroll_deficit: 0,
            selection: Selection::default(),
            pseudo_cursors: Vec::new(),
            dpr: 1.0,
            status_bar_hover: StatusBarTarget::None,
            status_bar_theme: StatusBarTheme::default(),
            last_status_bar: None,
            project_name: String::from("ImmorTerm"),
            session_title: String::new(),
            font_size_css: DEFAULT_FONT_SIZE,
            line_height_ratio: 1.15,
            custom_font_data: Vec::new(),
            pending_theme: None,
            pending_font_weight: None,
            pending_content_padding: [0.0; 4],
            custom_font_name: None,
            ai_stats: String::new(),
            ai_ctx_pct: 0.0,
            last_activity_ms: 0.0,
            pending_border_enabled: true,
            pending_border_opacity: 1.0,
            pending_animations_enabled: true,
            pending_expression_effects: true,
            pending_celebrations: true,
            pending_danger_effects: true,
            pending_text_animations: true,
            status_bar_mode: String::from("always"),
            immorterm_id: String::new(),
            title_marquee_time: 0.0,
            title_marquee_overflow: 0,
            char_height_css: 0.0,
            comments: Comments::new(),
        }
    }

    /// Initialize WebGPU rendering on the given canvas element.
    /// This is async because adapter/device requests are async in the browser.
    pub async fn init_gpu(&mut self, canvas_id: &str, dpr: f32) -> Result<(), JsValue> {
        self.dpr = dpr.max(1.0);

        let window = web_sys::window().ok_or("no window")?;
        let document = window.document().ok_or("no document")?;
        let canvas = document
            .get_element_by_id(canvas_id)
            .ok_or_else(|| JsValue::from_str(&format!("no element with id '{}'", canvas_id)))?
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|_| "element is not a canvas")?;

        self.width = canvas.width();
        self.height = canvas.height();

        // Create wgpu instance with WebGPU backend
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..Default::default()
        });

        // Create surface from canvas. wgpu::SurfaceTarget::Canvas requires 'static.
        // We leak the canvas reference — it lives for the page lifetime anyway.
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(|e| JsValue::from_str(&format!("surface creation failed: {}", e)))?;

        // Request adapter
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or("no suitable GPU adapter found")?;

        // Request device
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("immorterm-wasm"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            }, None)
            .await
            .map_err(|e| JsValue::from_str(&format!("device request failed: {}", e)))?;

        // Configure surface
        let surface_caps = surface.get_capabilities(&adapter);
        // Prefer non-sRGB format — our color values are already in sRGB space.
        // Using a non-sRGB surface avoids double gamma encoding.
        // Matches the native daemon's window.rs surface format preference.
        let surface_format = surface_caps
            .formats
            .iter()
            .find(|f| !f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);
        web_sys::console::log_1(
            &format!("[WASM] surface format: {:?}, all: {:?}", surface_format, surface_caps.formats).into(),
        );

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: self.width,
            height: self.height,
            present_mode: wgpu::PresentMode::Fifo,
            // Force Opaque: glyph anti-aliasing writes sub-1.0 alpha fragments,
            // and PreMultiplied compositing (Chrome's default) would blend those
            // against the HTML page background, creating visible dark lines.
            // Opaque tells the browser to ignore the alpha channel entirely.
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // Create the renderer — same code path as native, but with embedded font.
        // Scale font size by DPR for crisp text on HiDPI displays.
        let font_size = self.font_size_css * self.dpr;
        let lh = self.line_height_ratio;
        let mut renderer = if self.custom_font_data.is_empty() {
            // Use embedded JetBrains Mono
            web_sys::console::log_1(&"[WASM] init_gpu: using EMBEDDED JetBrains Mono (no custom font data)".into());
            let fonts: &[&[u8]] = &[FONT_REGULAR, FONT_ITALIC, FONT_BOLD, FONT_BOLD_ITALIC, FONT_SYMBOLS, FONT_NERD, FONT_MISC, FONT_HEBREW, FONT_HEBREW_BOLD];
            TerminalRenderer::with_line_height(&device, &queue, surface_format, Some(fonts), font_size, lh)
        } else {
            // Use custom font from the host (e.g. user's VS Code terminal font)
            // Always append the symbols + nerd fonts for Dingbats and CLI tool glyphs
            let mut font_refs: Vec<&[u8]> = self.custom_font_data.iter().map(|v| v.as_slice()).collect();
            web_sys::console::log_1(&format!(
                "[WASM] init_gpu: using CUSTOM font ({} bytes + symbols + nerd)",
                self.custom_font_data[0].len()
            ).into());
            font_refs.push(FONT_SYMBOLS);
            font_refs.push(FONT_NERD);
            font_refs.push(FONT_MISC);

            let mut r = TerminalRenderer::with_line_height(&device, &queue, surface_format, Some(&font_refs), font_size, lh);
            // Override the font family name if explicitly provided by the host
            if let Some(ref name) = self.custom_font_name {
                web_sys::console::log_1(&format!(
                    "[WASM] init_gpu: setting explicit font family: '{}' (was: {:?})",
                    name, r.atlas.custom_family()
                ).into());
                r.atlas.set_custom_family(name.clone());
            }
            r
        };

        // Apply VS Code theme colors if set before init
        if let Some(ref theme) = self.pending_theme {
            renderer.theme = theme.clone();
        }
        // Apply font weight if set before init
        if let Some(weight) = self.pending_font_weight {
            renderer.atlas.set_base_weight(weight);
        }
        // Apply content padding if set before init
        let [pt, pr, pb, pl] = self.pending_content_padding;
        renderer.set_content_padding(pt, pr, pb, pl);
        // Apply visual preferences if set before init
        renderer.border_enabled = self.pending_border_enabled;
        renderer.border_opacity = self.pending_border_opacity;
        renderer.animations_enabled = self.pending_animations_enabled;
        renderer.expression_effects = self.pending_expression_effects;
        renderer.celebrations_enabled = self.pending_celebrations;
        renderer.danger_effects = self.pending_danger_effects;
        renderer.text_animations = self.pending_text_animations;

        // Register embedded fonts in the browser via FontFace API so Canvas 2D can use them.
        // This makes font rendering deterministic — no reliance on system-installed fonts.
        web_sys::console::log_1(&"[WASM] Registering embedded fonts via FontFace API...".into());
        register_font_face("JetBrains Mono", FONT_REGULAR, "normal", "normal").await;
        register_font_face("JetBrains Mono", FONT_BOLD, "bold", "normal").await;
        register_font_face("JetBrains Mono", FONT_ITALIC, "normal", "italic").await;
        register_font_face("JetBrains Mono", FONT_BOLD_ITALIC, "bold", "italic").await;
        register_font_face("Heebo", FONT_HEBREW, "normal", "normal").await;
        register_font_face("Heebo", FONT_HEBREW_BOLD, "bold", "normal").await;
        web_sys::console::log_1(&"[WASM] FontFace registration complete".into());

        // Set up Canvas 2D fallback rasterizer for emoji and other system-font glyphs.
        // When cosmic-text can't find a character in loaded fonts (glyph_id == 0),
        // OffscreenCanvas renders it using the browser's access to system fonts
        // (Apple Color Emoji on macOS, Segoe UI Emoji on Windows, etc.).
        {
            let cell_w = renderer.atlas.metrics.cell_width;
            let cell_h = renderer.atlas.metrics.cell_height;
            let baseline = renderer.atlas.metrics.baseline_y;
            let fs = self.font_size_css * self.dpr;

            let rasterizer = Box::new(move |ch: char, _font_size: f32| -> Option<FallbackGlyph> {
                use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement};

                // Emoji are typically double-width; use 2 cells for width
                let w = (cell_w * 2.0).ceil() as u32;
                let h = cell_h.ceil() as u32;
                if w == 0 || h == 0 {
                    return None;
                }

                // Use DOM HTMLCanvasElement, NOT OffscreenCanvas.
                // On macOS, Chromium's OffscreenCanvas goes to its bundled Noto Color
                // Emoji for color glyphs instead of consulting CoreText for Apple Color
                // Emoji. DOM <canvas> in document context queries the full platform
                // text stack, which gives us the native Apple glyphs.
                let document = web_sys::window()?.document()?;
                let canvas: HtmlCanvasElement = document
                    .create_element("canvas")
                    .ok()?
                    .dyn_into()
                    .ok()?;
                canvas.set_width(w);
                canvas.set_height(h);
                let ctx: CanvasRenderingContext2d = canvas
                    .get_context("2d")
                    .ok()??
                    .dyn_into()
                    .ok()?;

                let font_str = format!(
                    "{}px 'Apple Color Emoji','Segoe UI Emoji','Noto Color Emoji',sans-serif",
                    fs
                );
                ctx.set_font(&font_str);
                ctx.set_text_baseline("alphabetic");
                ctx.fill_text(&ch.to_string(), 0.0, baseline as f64).ok()?;

                let img = ctx.get_image_data(0.0, 0.0, w as f64, h as f64).ok()?;
                let data = img.data().to_vec();

                // Verify at least one non-transparent pixel exists
                if data.iter().skip(3).step_by(4).all(|&a| a == 0) {
                    return None;
                }

                Some(FallbackGlyph {
                    data,
                    width: w,
                    height: h,
                    bearing_x: 0.0,
                    bearing_y: 0.0,
                })
            });
            renderer.atlas.set_fallback_rasterizer(Some(rasterizer));
            web_sys::console::log_1(&"[WASM] Canvas 2D fallback rasterizer enabled for emoji".into());
        }

        // Override cell metrics with Canvas 2D measurements.
        // We register JetBrains Mono via FontFace API, so Canvas 2D uses
        // the same fonts as cosmic-text. Metrics must come from the rasterizer font.
        {
            use web_sys::{OffscreenCanvas, OffscreenCanvasRenderingContext2d};
            let fs = self.font_size_css * self.dpr;
            let font_name = self.custom_font_name.clone().unwrap_or_else(|| "'JetBrains Mono', 'Heebo', monospace".to_string());
            let font_str = format!("{}px {}", fs, font_name);

            let measure_canvas = OffscreenCanvas::new(100, 100).unwrap();
            let measure_ctx: OffscreenCanvasRenderingContext2d = measure_canvas
                .get_context("2d").unwrap().unwrap().dyn_into().unwrap();

            // 1. Measure at CSS pixel resolution for cell height.
            //    Use DOM-measured char height (from JS hidden span) if available —
            //    this matches xterm.js CharSizeService exactly. Fall back to
            //    fontBoundingBox from Canvas measureText if not set.
            let css_font_str = format!("{}px {}", self.font_size_css, font_name);
            measure_ctx.set_font(&css_font_str);
            let css_tm = measure_ctx.measure_text("M").unwrap();
            let font_bbox_css = css_tm.font_bounding_box_ascent() + css_tm.font_bounding_box_descent();
            let char_height = if self.char_height_css > 0.0 {
                self.char_height_css as f64
            } else {
                font_bbox_css
            };

            // 2. Measure at device pixel resolution for glyph metrics (cell_width, baseline).
            measure_ctx.set_font(&font_str);
            let tm = measure_ctx.measure_text("M").unwrap();
            let cell_width = tm.width() as f32;
            let font_ascent = tm.actual_bounding_box_ascent() as f32;

            // 3. Cell height: scale char height to device pixels, THEN ceil to snap to
            //    device pixel grid. xterm.js WebGL renderer does:
            //    scaledCharHeight = ceil(charHeight * dpr),
            //    scaledCellHeight = floor(scaledCharHeight * lineHeight).
            let user_line_height = self.line_height_ratio / 1.15;
            let scaled_char_height = (char_height as f32 * self.dpr).ceil();
            let cell_height = (scaled_char_height * user_line_height).floor();
            let font_em_height = fs;
            let center_pad = (cell_height - font_em_height).max(0.0) / 2.0;
            // Round baseline to integer pixel — fractional baselines cause the Canvas 2D
            // rasterizer to anti-alias differently, making glyphs appear lighter.
            let baseline_y = (font_ascent + center_pad + 1.0).round();

            let old = &renderer.atlas.metrics;
            let source = if self.char_height_css > 0.0 { "DOM" } else { "fontBBox" };
            web_sys::console::log_1(&format!(
                "[WASM] Cell metrics: cosmic({:.1}x{:.1}) → Canvas2D({:.1}x{:.1}, bl={:.1}) [charH={:.2}css({}), fontBBox={:.2}, scaledChar={:.0}, font={:.0}px@{:.0}x, userLH={:.2}]",
                old.cell_width, old.cell_height,
                cell_width, cell_height, baseline_y,
                char_height, source, font_bbox_css, scaled_char_height,
                self.font_size_css, self.dpr, user_line_height
            ).into());

            renderer.atlas.set_metrics(immorterm_render::CellMetrics {
                cell_width,
                cell_height,
                baseline_y,
            });
        }

        // Set up Canvas 2D PRIMARY rasterizer for ALL monochrome glyphs.
        // Uses the browser's native font renderer (Skia/CoreText) so glyph shapes
        // are pixel-identical to VS Code's terminal. Renders white text on black
        // background and extracts the red channel as the alpha mask.
        {
            let cell_w = renderer.atlas.metrics.cell_width;
            let cell_h = renderer.atlas.metrics.cell_height;
            let baseline = renderer.atlas.metrics.baseline_y;
            let fs = self.font_size_css * self.dpr;
            let font_name = self.custom_font_name.clone().unwrap_or_else(|| "'JetBrains Mono', 'Heebo', monospace".to_string());
            let font_name_log = font_name.clone();

            let rasterizer = Box::new(move |ch: char, _font_size: f32, bold: bool, italic: bool| -> Option<MonoGlyph> {
                use web_sys::{OffscreenCanvas, OffscreenCanvasRenderingContext2d};

                let w = cell_w.ceil() as u32;
                let h = cell_h.ceil() as u32;
                if w == 0 || h == 0 {
                    return None;
                }

                let canvas = OffscreenCanvas::new(w, h).ok()?;
                let ctx: OffscreenCanvasRenderingContext2d = canvas
                    .get_context("2d")
                    .ok()??
                    .dyn_into()
                    .ok()?;

                // Black background — text adds brightness, R channel = coverage
                ctx.set_fill_style_str("black");
                ctx.fill_rect(0.0, 0.0, w as f64, h as f64);

                let weight = if bold { "bold" } else { "normal" };
                let style = if italic { "italic" } else { "normal" };
                let font_str = format!("{} {} {}px {}", style, weight, fs, font_name);
                ctx.set_font(&font_str);
                ctx.set_fill_style_str("white");
                ctx.set_text_baseline("alphabetic");

                let s = ch.to_string();
                let tm = ctx.measure_text(&s).ok()?;
                let char_width = tm.width();

                if char_width <= cell_w as f64 * 1.02 {
                    // Fits in cell — center it
                    let x_offset = ((cell_w as f64 - char_width) / 2.0).max(0.0);
                    ctx.fill_text(&s, x_offset, baseline as f64).ok()?;
                } else {
                    // Overflows — compress horizontally to fit
                    let scale_x = cell_w as f64 / char_width;
                    ctx.save();
                    let _ = ctx.translate(cell_w as f64 / 2.0, 0.0);
                    let _ = ctx.scale(scale_x, 1.0);
                    ctx.fill_text(&s, -char_width / 2.0, baseline as f64).ok()?;
                    ctx.restore();
                }

                let img = ctx.get_image_data(0.0, 0.0, w as f64, h as f64).ok()?;
                let rgba = img.data().to_vec();

                // Find tight bounding box of non-zero pixels (trim transparent border)
                let mut min_x = w;
                let mut min_y = h;
                let mut max_x = 0u32;
                let mut max_y = 0u32;
                for y in 0..h {
                    for x in 0..w {
                        let r = rgba[((y * w + x) * 4) as usize];
                        if r > 0 {
                            min_x = min_x.min(x);
                            min_y = min_y.min(y);
                            max_x = max_x.max(x);
                            max_y = max_y.max(y);
                        }
                    }
                }
                if max_x < min_x {
                    return None; // completely empty
                }

                // Extract trimmed alpha data from red channel
                let tw = max_x - min_x + 1;
                let th = max_y - min_y + 1;
                let mut alpha = Vec::with_capacity((tw * th) as usize);
                for y in min_y..=max_y {
                    for x in min_x..=max_x {
                        alpha.push(rgba[((y * w + x) * 4) as usize]);
                    }
                }

                Some(MonoGlyph {
                    data: alpha,
                    width: tw,
                    height: th,
                    offset_x: min_x as f32,
                    offset_y: min_y as f32,
                })
            });
            renderer.atlas.set_primary_rasterizer(Some(rasterizer));
            web_sys::console::log_1(&format!(
                "[WASM] Canvas 2D primary rasterizer enabled (font: '{}', size: {}px)",
                font_name_log, fs
            ).into());

            // Run rasterizer for multi-character text runs (Hebrew/Arabic words).
            // Renders the whole string in one Canvas 2D call, preserving natural
            // kerning and proportional spacing. The result is centered in N cells.
            let run_font_name = font_name_log.clone();
            let run_rasterizer = Box::new(move |text: &str, _font_size: f32, bold: bool, italic: bool, num_cells: u32| -> Option<MonoGlyph> {
                use web_sys::{OffscreenCanvas, OffscreenCanvasRenderingContext2d};

                let total_w = (cell_w * num_cells as f32).ceil() as u32;
                let h = cell_h.ceil() as u32;
                if total_w == 0 || h == 0 {
                    return None;
                }

                let canvas = OffscreenCanvas::new(total_w, h).ok()?;
                let ctx: OffscreenCanvasRenderingContext2d = canvas
                    .get_context("2d")
                    .ok()??
                    .dyn_into()
                    .ok()?;

                // Black background
                ctx.set_fill_style_str("black");
                ctx.fill_rect(0.0, 0.0, total_w as f64, h as f64);

                let weight = if bold { "bold" } else { "normal" };
                let style = if italic { "italic" } else { "normal" };
                let font_str =
                    format!("{} {} {}px {}", style, weight, fs, run_font_stack(text, &run_font_name));
                ctx.set_font(&font_str);
                ctx.set_fill_style_str("white");
                ctx.set_text_baseline("alphabetic");

                // Measure the natural text width and center it within the N-cell span
                let tm = ctx.measure_text(text).ok()?;
                let text_width = tm.width();
                let x_offset = ((total_w as f64 - text_width) / 2.0).max(0.0);
                ctx.fill_text(text, x_offset, baseline as f64).ok()?;

                let img = ctx.get_image_data(0.0, 0.0, total_w as f64, h as f64).ok()?;
                let rgba = img.data().to_vec();

                // Find tight bounding box
                let mut min_x = total_w;
                let mut min_y = h;
                let mut max_x = 0u32;
                let mut max_y = 0u32;
                for y in 0..h {
                    for x in 0..total_w {
                        let r = rgba[((y * total_w + x) * 4) as usize];
                        if r > 0 {
                            min_x = min_x.min(x);
                            min_y = min_y.min(y);
                            max_x = max_x.max(x);
                            max_y = max_y.max(y);
                        }
                    }
                }
                if max_x < min_x {
                    return None;
                }

                let tw = max_x - min_x + 1;
                let th = max_y - min_y + 1;
                let mut alpha = Vec::with_capacity((tw * th) as usize);
                for y in min_y..=max_y {
                    for x in min_x..=max_x {
                        alpha.push(rgba[((y * total_w + x) * 4) as usize]);
                    }
                }

                Some(MonoGlyph {
                    data: alpha,
                    width: tw,
                    height: th,
                    offset_x: min_x as f32,
                    offset_y: min_y as f32,
                })
            });
            renderer.atlas.set_run_rasterizer(Some(run_rasterizer));
        }

        // Calculate terminal dimensions from pixel size (accounting for padding)
        let (cw, ch) = renderer.cell_metrics();
        let content_w = (self.width as f32 - pl - pr).max(0.0);
        let content_h = (self.height as f32 - pt - pb).max(0.0);
        let cols = (content_w / cw).floor() as usize;
        let rows = (content_h / ch).floor() as usize;
        self.cols = cols.max(1);
        self.rows = rows.max(1);
        self.terminal.resize(self.cols, self.rows);

        self.gpu = Some(GpuState {
            device,
            queue,
            surface,
            surface_config: config,
        });
        self.renderer = Some(renderer);

        Ok(())
    }

    /// Load a full terminal state from a JSON snapshot (sent by daemon on subscribe_raw).
    /// Replaces the current terminal state entirely — used for initial sync and lag recovery.
    ///
    /// IMPORTANT: We do NOT adopt the snapshot's grid dimensions. The WASM client's
    /// grid size is determined by the canvas pixel size, not the daemon's terminal.
    /// After loading the snapshot content, we resize the terminal to match our canvas.
    ///
    /// For viewport-only snapshots (empty scrollback, sent during lag recovery), we
    /// preserve the WASM's local scrollback. The WASM terminal builds up scrollback
    /// from processing live PTY data — replacing it with empty scrollback would
    /// destroy the user's scroll position and history.
    pub fn load_snapshot(&mut self, json: &str, immorterm_id: &str) -> Result<(), JsValue> {
        let snap: immorterm_core::TerminalSnapshot = serde_json::from_str(json)
            .map_err(|e| JsValue::from_str(&format!("snapshot parse error: {}", e)))?;
        let prev_offset = self.scroll_offset;

        // Viewport-only snapshot: preserve WASM-local scrollback so scroll
        // position survives lag recovery during heavy output (e.g. Claude streaming).
        // ONLY preserve if the snapshot is for the SAME session — otherwise we'd
        // transplant a closed session's scrollback into the new session.
        let same_session = !self.immorterm_id.is_empty() && self.immorterm_id == immorterm_id;
        let preserve_sb = same_session && snap.scrollback.is_empty() && !self.terminal.scrollback.is_empty();
        let saved_sb = if preserve_sb {
            Some(std::mem::replace(
                &mut self.terminal.scrollback,
                immorterm_core::Scrollback::new(0),
            ))
        } else {
            None
        };

        self.terminal = immorterm_core::Terminal::from_snapshot(snap);
        // self.terminal.enable_marker_parsing(); // DISABLED: testing horizontal line artifacts

        // Transplant saved scrollback into the new terminal
        if let Some(sb) = saved_sb {
            self.terminal.scrollback = sb;
        }

        // Update session identity
        if !immorterm_id.is_empty() {
            self.immorterm_id = immorterm_id.to_string();
        }

        // DISABLED: Frankenstein third-reflow pass (memory 242bda75).
        // Previously: self.terminal.resize(self.cols, self.rows);
        // Reason: from_snapshot() already built the viewport at daemon's cols,
        // then we transplanted the pre-resize WASM scrollback (already reflowed
        // at canvas cols). Calling resize() here runs a THIRD reflow pass on
        // mismatched column state, producing duplicate rows in scrollback.
        // Daemon is authoritative for reflow; trust its snapshot dims.
        // self.terminal.resize(self.cols, self.rows);
        // Preserve scroll position only for same session; reset for session switch
        self.scroll_offset = if same_session {
            prev_offset.min(self.terminal.scrollback.len())
        } else {
            0
        };
        self.selection = Selection::default();
        // Drop staged comments on a session CHANGE — their line_ids belong
        // to the old session's scrollback.net_shift and would resolve to
        // meaningless rows in the new session. Same-session snapshot loads
        // (lag recovery) keep comments intact.
        if !same_session {
            self.comments.clear();
        }
        Ok(())
    }

    /// Process raw bytes (ANSI escape sequences) through the terminal emulator.
    pub fn process(&mut self, data: &[u8]) {
        self.terminal.process(data);
        self.last_activity_ms = js_sys::Date::now();
    }

    /// Process a string (convenience wrapper for process).
    pub fn process_str(&mut self, text: &str) {
        self.terminal.process(text.as_bytes());
    }

    /// Enable or disable the status bar.
    pub fn set_status_bar_enabled(&mut self, enabled: bool) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.status_bar_enabled = enabled;
            // Re-calculate rows to account for status bar
            if let Some(gpu) = self.gpu.as_ref() {
                let (cols, rows) = renderer.resize(&gpu.device, self.width, self.height);
                self.cols = cols;
                self.rows = rows;
                self.terminal.resize(cols, rows);
            }
        }
    }

    /// Set the status bar reveal amount (0.0 = fully hidden, 1.0 = fully visible).
    /// Used by JS animation driver for smooth show/hide transitions.
    pub fn set_status_bar_reveal(&mut self, reveal: f32) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.status_bar_reveal = reveal.clamp(0.0, 1.0);
        }
    }

    /// Set the status bar visibility mode: "always", "auto", or "hidden".
    /// - "always": row reserved, reveal = 1.0
    /// - "auto": row reserved, reveal starts at 0.0 (JS drives animation)
    /// - "hidden": row freed (triggers resize), reveal = 0.0
    pub fn set_status_bar_mode(&mut self, mode: &str) {
        self.status_bar_mode = mode.to_string();
        match mode {
            "always" => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.status_bar_reveal = 1.0;
                    if !renderer.status_bar_enabled {
                        renderer.status_bar_enabled = true;
                        if let Some(gpu) = self.gpu.as_ref() {
                            let (cols, rows) = renderer.resize(&gpu.device, self.width, self.height);
                            self.cols = cols;
                            self.rows = rows;
                            self.terminal.resize(cols, rows);
                        }
                    }
                }
            }
            "auto" => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.status_bar_reveal = 0.0;
                    if !renderer.status_bar_enabled {
                        renderer.status_bar_enabled = true;
                        if let Some(gpu) = self.gpu.as_ref() {
                            let (cols, rows) = renderer.resize(&gpu.device, self.width, self.height);
                            self.cols = cols;
                            self.rows = rows;
                            self.terminal.resize(cols, rows);
                        }
                    }
                }
            }
            "hidden" => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.status_bar_reveal = 0.0;
                    if renderer.status_bar_enabled {
                        renderer.status_bar_enabled = false;
                        if let Some(gpu) = self.gpu.as_ref() {
                            let (cols, rows) = renderer.resize(&gpu.device, self.width, self.height);
                            self.cols = cols;
                            self.rows = rows;
                            self.terminal.resize(cols, rows);
                        }
                    }
                }
            }
            _ => {} // ignore unknown modes
        }
    }

    /// Enable or disable the window border.
    pub fn set_border_enabled(&mut self, enabled: bool) {
        self.pending_border_enabled = enabled;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.border_enabled = enabled;
        }
    }

    /// Set border opacity (0.0 = invisible, 1.0 = fully opaque).
    pub fn set_border_opacity(&mut self, opacity: f32) {
        let clamped = opacity.clamp(0.0, 1.0);
        self.pending_border_opacity = clamped;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.border_opacity = clamped;
        }
    }

    /// Enable or disable status bar animations (shimmer, bloom, breathing, wave).
    pub fn set_animations_enabled(&mut self, enabled: bool) {
        self.pending_animations_enabled = enabled;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.animations_enabled = enabled;
        }
    }

    /// Enable or disable AI expression effects (mood colors, confidence alpha, color overrides).
    pub fn set_expression_effects(&mut self, enabled: bool) {
        self.pending_expression_effects = enabled;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.expression_effects = enabled;
        }
    }

    /// Enable or disable celebration particle effects (confetti, sparkle, fireworks).
    pub fn set_celebrations_enabled(&mut self, enabled: bool) {
        self.pending_celebrations = enabled;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.celebrations_enabled = enabled;
        }
    }

    /// Enable or disable danger visual effects (vignette, pulse, border accent).
    pub fn set_danger_effects(&mut self, enabled: bool) {
        self.pending_danger_effects = enabled;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.danger_effects = enabled;
        }
    }

    /// Apply an AI expression state (mood, confidence, danger, animation, celebration).
    /// Accepts a JSON string matching `ExpressionState`.  The renderer uses this as a
    /// global fallback for all visible cells, and future PTY bytes are also stamped
    /// with the recomputed `ExpressionMeta`.
    pub fn set_expression(&mut self, json: &str) -> Result<(), JsValue> {
        let state: immorterm_core::expression::ExpressionState =
            serde_json::from_str(json)
                .map_err(|e| JsValue::from_str(&format!("expression parse error: {}", e)))?;
        self.terminal.set_expression(state);
        Ok(())
    }

    /// Enable or disable per-character text animations (pulse, glow, wave, etc.).
    pub fn set_text_animations(&mut self, enabled: bool) {
        self.pending_text_animations = enabled;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.text_animations = enabled;
        }
    }

    /// Render a frame. Call this from requestAnimationFrame.
    /// Returns true if the frame was rendered, false if GPU not initialized.
    pub fn render(&mut self) -> bool {
        // Build selection lists before borrowing renderer (avoids borrow conflict)
        let pseudo_sels: Vec<Selection> = self.pseudo_cursors.iter()
            .filter(|s| s.is_active)
            .cloned()
            .collect();
        let regular_sels: Vec<Selection> = if self.selection.is_active {
            vec![self.selection.clone()]
        } else {
            vec![]
        };

        let (gpu, renderer) = match (self.gpu.as_ref(), self.renderer.as_mut()) {
            (Some(g), Some(r)) => (g, r),
            _ => return false,
        };

        let surface_texture = match gpu.surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                // Surface became stale (e.g., webview tab hidden then shown).
                // Reconfigure and skip this frame — next frame will succeed.
                gpu.surface.configure(&gpu.device, &gpu.surface_config);
                return false;
            }
            Err(_) => return false,
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Build status bar data if enabled
        let sb_data = if renderer.status_bar_enabled {
            let time = renderer.start_time.elapsed().as_secs_f32();
            let dot = if self.pending_animations_enabled {
                statusbar::animated_dot_char(time)
            } else {
                '\u{00B7}' // static middle dot when animations disabled
            };
            // Compute "last active" string from tracked PTY activity
            // Format as DD/MM HH:MM (matching C binary's strftime "%d/%m %H:%M")
            let last_active = if self.last_activity_ms > 0.0 {
                let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(self.last_activity_ms));
                format!("{:02}/{:02} {:02}:{:02}",
                    date.get_date(),
                    date.get_month() + 1,
                    date.get_hours(),
                    date.get_minutes())
            } else {
                "--/-- --:--".to_string()
            };
            // Compute marquee elapsed time (title_marquee_time stores the start time)
            if self.title_marquee_time == 0.0 {
                self.title_marquee_time = time; // initialize on first frame
            }
            let marquee_elapsed = time - self.title_marquee_time;

            // Compute scroll offset: when title is hovered, pass MAX to expand full title
            let (scroll_offset, scroll_fract) = if self.status_bar_hover == StatusBarTarget::Title {
                (usize::MAX, 0.0)
            } else {
                // Compute overflow on first frame or when cached value is 0
                // (recalculated when title/cols change via set_session_title reset)
                if self.title_marquee_overflow == 0 {
                    let probe = statusbar::build_sections_with_theme(
                        &self.project_name,
                        &self.session_title,
                        &self.ai_stats,
                        &last_active,
                        dot,
                        self.cols,
                        self.ai_ctx_pct,
                        &self.status_bar_theme,
                        0, 0.0,
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

            let mut data = statusbar::build_sections_with_theme(
                &self.project_name,
                &self.session_title,
                &self.ai_stats,
                &last_active,
                dot,
                self.cols,
                self.ai_ctx_pct,
                &self.status_bar_theme,
                scroll_offset,
                scroll_fract,
            );
            data.hovered_target = self.status_bar_hover;
            // Cache for hit-testing between frames
            self.last_status_bar = Some(data.clone());
            Some(data)
        } else {
            self.last_status_bar = None;
            None
        };

        let opts = RenderOptions {
            scroll_offset: self.scroll_offset,
            selections: &regular_sels,
            pseudo_selections: &pseudo_sels,
            status_bar: sb_data.as_ref(),
            popup: None,
            pane: None,
            clear: true,
        };

        renderer.render(&gpu.device, &gpu.queue, &view, &mut self.terminal, &opts);
        surface_texture.present();

        true
    }

    /// Handle a key press. Returns bytes to send to PTY (for remote mode),
    /// or processes locally in demo mode.
    ///
    /// For demo/local-echo mode, pass the result back to `process()`.
    pub fn handle_key(&mut self, key: &str, ctrl: bool, shift: bool, alt: bool) -> Vec<u8> {
        // ── Phase 1: Option (Alt/Meta) key handling ──
        // Option acts as Meta: prefix the key's normal output with ESC.
        // This makes Option+Backspace (delete word), Option+D (delete word
        // forward), Option+B/F (word left/right), etc. all work automatically
        // via readline/zsh ESC-prefixed keybindings.
        // Ctrl+Alt combos bypass this and fall through to xterm modifier encoding.
        if alt && !ctrl {
            let bytes: Option<Vec<u8>> = match key {
                "Backspace" => Some(vec![0x1b, 0x7f]),
                "Delete" => Some(b"\x1bd".to_vec()),
                "ArrowLeft" => Some(b"\x1bb".to_vec()),
                "ArrowRight" => Some(b"\x1bf".to_vec()),
                "Enter" => Some(b"\x1b\r".to_vec()),
                _ if key.chars().count() == 1 => {
                    let mut v = vec![0x1b];
                    v.extend_from_slice(key.as_bytes());
                    Some(v)
                }
                _ => None,
            };
            if let Some(data) = bytes {
                return data;
            }
        }

        // ── Phase 2: Standard key dispatch ──
        match key {
            "Enter" => {
                if shift {
                    // Kitty keyboard protocol: CSI 13;2u (Shift+Enter)
                    // Apps like Claude Code use this to distinguish newline from submit.
                    b"\x1b[13;2u".to_vec()
                } else {
                    b"\r".to_vec()
                }
            }
            "Backspace" => b"\x7f".to_vec(),
            "Tab" => {
                if shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }
            }
            "Escape" => b"\x1b".to_vec(),
            "ArrowUp" | "ArrowDown" | "ArrowRight" | "ArrowLeft" => {
                // Inside Claude Code's input box, Ink's TextInput discards
                // bare Up/Down when the buffer is one wrapped logical line
                // (no `\n` between visual rows). Route those keystrokes
                // through the Dijkstra planner instead — same machinery
                // click-to-cursor uses — which crosses rows via Right-at-end
                // / Left-at-start (which Ink does honor). Modified arrows
                // (Shift/Alt/Ctrl), Left/Right, and any Up/Down outside the
                // input box keep their raw xterm encoding.
                if !ctrl && !shift && !alt && (key == "ArrowUp" || key == "ArrowDown") {
                    let dir: i32 = if key == "ArrowUp" { -1 } else { 1 };
                    let seq = self.plan_arrow_in_input(dir);
                    if !seq.is_empty() {
                        return seq;
                    }
                }
                let letter = match key {
                    "ArrowUp" => 'A', "ArrowDown" => 'B',
                    "ArrowRight" => 'C', _ => 'D',
                };
                // xterm modifier encoding: 1 + (Shift=1, Alt=2, Ctrl=4)
                let modifier = 1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2);
                if modifier > 1 {
                    format!("\x1b[1;{}{}", modifier, letter).into_bytes()
                } else {
                    format!("\x1b[{}", letter).into_bytes()
                }
            }
            "Home" => if shift { b"\x1b[1;2H".to_vec() } else { b"\x1b[H".to_vec() },
            "End" => if shift { b"\x1b[1;2F".to_vec() } else { b"\x1b[F".to_vec() },
            "PageUp" => {
                if shift {
                    // Scroll up
                    let max = self.terminal.scrollback.len();
                    self.scroll_offset = (self.scroll_offset + self.rows / 2).min(max);
                    vec![]
                } else {
                    b"\x1b[5~".to_vec()
                }
            }
            "PageDown" => {
                if shift {
                    // Scroll down
                    self.scroll_offset = self.scroll_offset.saturating_sub(self.rows / 2);
                    vec![]
                } else {
                    b"\x1b[6~".to_vec()
                }
            }
            "Delete" => b"\x1b[3~".to_vec(),
            _ => {
                // key.chars().count() == 1 handles multi-byte chars (Hebrew, Arabic, CJK, etc.)
                // key.len() == 1 only matches ASCII — would silently drop non-ASCII input.
                let mut chars = key.chars();
                if let Some(ch) = chars.next() {
                    if chars.next().is_none() {
                        // Single character (ASCII or Unicode)
                        if ctrl && ch.is_ascii_alphabetic() {
                            // Ctrl+A = 0x01, Ctrl+Z = 0x1A
                            let ctrl_byte = (ch.to_ascii_lowercase() as u8) - b'a' + 1;
                            vec![ctrl_byte]
                        } else {
                            key.as_bytes().to_vec()
                        }
                    } else {
                        // Multi-character key name (e.g. "F13") — unknown, ignore
                        vec![]
                    }
                } else {
                    vec![]
                }
            }
        }
    }

    // ── Selection + Clipboard ──

    /// Start a text selection at the given CSS pixel coordinates.
    /// Call on mousedown.
    pub fn selection_start(&mut self, css_x: f32, css_y: f32) {
        let (col, row) = self.css_to_cell(css_x, css_y);
        // Convert display row → absolute content index so it matches the renderer
        let content_row = self.display_to_content(row);
        self.selection = Selection {
            anchor: (col, content_row),
            active: (col, content_row),
            is_active: true,
            block_mode: false,
        };
    }

    /// Double-click: select the word under the given CSS pixel coordinates.
    /// Word chars: alphanumeric, underscore, hyphen. Expands outward from click.
    pub fn select_word_at(&mut self, css_x: f32, css_y: f32) {
        let (col, row) = self.css_to_cell(css_x, css_y);
        let content_row = self.display_to_content(row);
        let sb_len = self.terminal.scrollback.len();
        let grid_row = if content_row < sb_len {
            self.terminal.scrollback.get(content_row)
        } else {
            self.terminal.grid.row(content_row - sb_len)
        };
        let cells = match grid_row {
            Some(r) => &r.cells,
            None => return,
        };
        if col >= cells.len() { return; }

        // Path/URL-aware word boundaries — double-click on `src/foo.rs:42`
        // selects the whole string, matching iTerm2/WezTerm convention.
        let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '~' | '+' | '@' | '%' | '#' | '?' | '=');
        if !is_word(cells[col].grapheme) { return; }

        // Expand left
        let mut left = col;
        while left > 0 && is_word(cells[left - 1].grapheme) { left -= 1; }
        // Expand right
        let mut right = col;
        while right + 1 < cells.len() && is_word(cells[right + 1].grapheme) { right += 1; }

        // Selection endpoints are inclusive — `right` is the last word char
        self.selection = Selection {
            anchor: (left, content_row),
            active: (right, content_row),
            is_active: true,
            block_mode: false,
        };
    }

    /// Select the entire visual line at the given CSS pixel coordinates.
    /// Called on triple-click. Selects from column 0 to the last non-space
    /// character, spanning soft-wrapped continuation rows.
    pub fn select_line_at(&mut self, css_x: f32, css_y: f32) {
        let (_col, row) = self.css_to_cell(css_x, css_y);
        let content_row = self.display_to_content(row);
        let sb_len = self.terminal.scrollback.len();

        let get_row = |cr: usize| -> Option<&immorterm_core::grid::Row> {
            if cr < sb_len {
                self.terminal.scrollback.get(cr)
            } else {
                self.terminal.grid.row(cr - sb_len)
            }
        };

        // Walk backward across soft-wrapped rows to find the logical line start
        let mut start = content_row;
        while start > 0 {
            if let Some(prev) = get_row(start - 1)
                && prev.is_soft_wrapped()
            {
                start -= 1;
            } else {
                break;
            }
        }

        // Walk forward across soft-wrapped rows to find the logical line end
        let mut end = content_row;
        loop {
            if let Some(r) = get_row(end)
                && r.is_soft_wrapped()
            {
                end += 1;
            } else {
                break;
            }
        }

        // Find last non-space column on the end row
        let end_col = get_row(end)
            .map(|r| {
                let mut last = 0;
                for (i, c) in r.cells.iter().enumerate() {
                    if c.grapheme != ' ' && c.grapheme != '\0' {
                        last = i;
                    }
                }
                last
            })
            .unwrap_or(0);

        self.selection = Selection {
            anchor: (0, start),
            active: (end_col, end),
            is_active: true,
            block_mode: false,
        };
    }

    /// Start a block (rectangular/column) selection at the given CSS pixel coordinates.
    /// Alt+drag triggers this mode — selects a rectangle, not full lines.
    pub fn selection_start_block(&mut self, css_x: f32, css_y: f32) {
        let (col, row) = self.css_to_cell(css_x, css_y);
        let content_row = self.display_to_content(row);
        self.selection = Selection {
            anchor: (col, content_row),
            active: (col, content_row),
            is_active: true,
            block_mode: true,
        };
    }

    /// Update the selection to the given CSS pixel coordinates.
    /// Call on mousemove while mouse is down.
    pub fn selection_update(&mut self, css_x: f32, css_y: f32) {
        if !self.selection.is_active {
            return;
        }
        let (col, row) = self.css_to_cell(css_x, css_y);
        let content_row = self.display_to_content(row);
        self.selection.active = (col, content_row);
    }

    /// Extend (or start) selection by keyboard.
    /// Directions: "left","right","up","down","home","end","word_left","word_right".
    /// If no selection is active, anchors at the current cursor position.
    pub fn selection_extend(&mut self, direction: &str) {
        let sb_len = self.terminal.scrollback.len();

        // If no selection, anchor at cursor position
        if !self.selection.is_active {
            let (anchor_col, anchor_content) =
                if !self.terminal.cursor.visible || !self.terminal.modes.cursor_visible {
                    // Cursor is hidden — TUI app is drawing its own visual cursor.
                    // Scan grid for reverse-video (INVERSE) cell as the visual cursor.
                    if let Some((vc_col, vc_row)) = self.find_visual_cursor() {
                        (vc_col, sb_len + vc_row)
                    } else {
                        // Fallback: use terminal cursor anyway
                        (self.terminal.cursor.col, sb_len + self.terminal.cursor.row)
                    }
                } else {
                    (self.terminal.cursor.col, sb_len + self.terminal.cursor.row)
                };
            self.selection = Selection {
                anchor: (anchor_col, anchor_content),
                active: (anchor_col, anchor_content),
                is_active: true,
                block_mode: false,
            };
        }

        let (col, row) = self.selection.active;
        let max_col = self.cols.saturating_sub(1);
        let max_row = sb_len + self.rows.saturating_sub(1);

        // BiDi-aware visual movement: convert logical→visual, step, convert back.
        // For non-BiDi rows, logical==visual so this is a no-op transformation.
        let bidi = self.renderer.as_ref()
            .and_then(|r| r.bidi_cache_for(row));

        self.selection.active = match direction {
            "left" | "right" => {
                let visual_col = bidi
                    .and_then(|b| b.logical_to_visual.get(col).copied())
                    .unwrap_or(col);

                let new_visual = if direction == "left" {
                    if visual_col > 0 { Some(visual_col - 1) } else { None }
                } else if visual_col < max_col {
                    Some(visual_col + 1)
                } else {
                    None
                };

                match new_visual {
                    Some(vc) => {
                        let logical = bidi
                            .and_then(|b| b.visual_to_logical.get(vc).copied())
                            .unwrap_or(vc);
                        (logical, row)
                    }
                    None => {
                        // Wrap to previous/next row
                        if direction == "left" {
                            if row > 0 { (max_col, row - 1) } else { (0, 0) }
                        } else if row < max_row {
                            (0, row + 1)
                        } else {
                            (max_col, max_row)
                        }
                    }
                }
            }
            "up" => (col, row.saturating_sub(1)),
            "down" => (col, (row + 1).min(max_row)),
            "home" => {
                // Home goes to the visual start of the row
                let visual_0_logical = bidi
                    .and_then(|b| b.visual_to_logical.first().copied())
                    .unwrap_or(0);
                (visual_0_logical, row)
            }
            "end" => {
                // End goes to the visual end of the row
                let visual_end_logical = bidi
                    .and_then(|b| b.visual_to_logical.last().copied())
                    .unwrap_or(max_col);
                (visual_end_logical, row)
            }
            "word_left" => self.move_selection_word_left(col, row, max_col),
            "word_right" => self.move_selection_word_right(col, row, max_col, max_row),
            _ => (col, row),
        };
    }

    /// Find the visual cursor position when the terminal cursor is hidden.
    /// TUI apps (Claude Code, vim, etc.) hide the hardware cursor and draw their
    /// own as a reverse-video (INVERSE) character. Scan from the bottom of the
    /// grid upward to find the last INVERSE cell — that's the visual cursor.
    /// Returns (col, grid_row) or None if no INVERSE cell found.
    fn find_visual_cursor(&self) -> Option<(usize, usize)> {
        // Scan from bottom of grid upward — the cursor is typically near the bottom
        for grid_row in (0..self.rows).rev() {
            if let Some(row) = self.terminal.grid.row(grid_row) {
                // Find the rightmost INVERSE cell on this row (cursor is usually
                // at the end of typed text)
                for col in (0..row.cells.len()).rev() {
                    if row.cells[col].attrs.contains(CellAttrs::INVERSE)
                        && row.cells[col].grapheme != ' '
                    {
                        return Some((col, grid_row));
                    }
                }
                // Also check for INVERSE space (cursor on an empty position)
                for col in (0..row.cells.len()).rev() {
                    if row.cells[col].attrs.contains(CellAttrs::INVERSE) {
                        return Some((col, grid_row));
                    }
                }
            }
        }
        None
    }

    /// Get the character at an absolute content position.
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

    /// Move selection active point one word to the left.
    fn move_selection_word_left(
        &self,
        mut col: usize,
        mut row: usize,
        last_col: usize,
    ) -> (usize, usize) {
        let is_word_char = |ch: char| ch.is_alphanumeric() || ch == '_';

        // Step back one character first
        if col > 0 {
            col -= 1;
        } else if row > 0 {
            row -= 1;
            col = last_col;
        } else {
            return (0, 0);
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

        // Skip same-class characters to find the first char of the word.
        // Inclusive selection (contains: col >= sc && col <= ec) means the
        // active endpoint IS included — landing on the first char of the
        // word makes that word the right-most extent of the selection, which
        // is the macOS Shift+Option+Left convention.
        let on_word = is_word_char(self.cell_char_at(col, row));
        while col > 0 || row > 0 {
            let (prev_col, prev_row) = if col > 0 {
                (col - 1, row)
            } else if row > 0 {
                (last_col, row - 1)
            } else {
                break;
            };
            let ch = self.cell_char_at(prev_col, prev_row);
            if on_word != is_word_char(ch) || ch.is_whitespace() {
                break;
            }
            col = prev_col;
            row = prev_row;
        }

        (col, row)
    }

    /// Move selection active point one word to the right.
    /// Symmetric to `move_selection_word_left`: checks the NEXT cell and
    /// advances only while same-class, so the active endpoint lands on the
    /// LAST char of the word. With inclusive selection semantics
    /// (contains: col >= sc && col <= ec) this makes the word the selection's
    /// right extent — no trailing space, no leak into the next word.
    fn move_selection_word_right(
        &self,
        mut col: usize,
        mut row: usize,
        last_col: usize,
        max_row: usize,
    ) -> (usize, usize) {
        let is_word_char = |ch: char| ch.is_alphanumeric() || ch == '_';

        // Step forward one character first (mirror of word_left's step back).
        // Without this, calling from the LAST char of a word would see
        // whitespace as the next cell, break immediately, and return the
        // same position — preventing repeated Shift+Option+Right presses
        // from advancing past the first word.
        if col < last_col {
            col += 1;
        } else if row < max_row {
            row += 1;
            col = 0;
        } else {
            return (col, row);
        }

        // If on whitespace (either we just stepped into the gap between
        // words, or the caller parked us there), skip forward to the next
        // non-whitespace cell so the same-class advance below operates on
        // a real word char.
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

        // Advance while the NEXT cell is same-class (mirror of word_left's
        // prev-cell check). Stops ON the last char of the current word.
        let on_word = is_word_char(self.cell_char_at(col, row));
        while col < last_col || row < max_row {
            let (next_col, next_row) = if col < last_col {
                (col + 1, row)
            } else if row < max_row {
                (0, row + 1)
            } else {
                break;
            };
            let ch = self.cell_char_at(next_col, next_row);
            if on_word != is_word_char(ch) || ch.is_whitespace() {
                break;
            }
            col = next_col;
            row = next_row;
        }

        (col, row)
    }

    /// Clear the current selection and all pseudo-cursors.
    pub fn selection_clear(&mut self) {
        self.selection = Selection::default();
        self.pseudo_cursors.clear();
    }

    /// Select all text in the Claude Code input area (❯ prompt to end of input).
    /// Returns true if a selection was made, false if no prompt was found.
    pub fn select_all_input(&mut self) -> bool {
        let (prompt_row, input_start_col) = match self.find_prompt_row() {
            Some(p) => p,
            None => return false,
        };
        let input_end_row = self.find_input_end_row(prompt_row);
        let sb_len = self.terminal.scrollback.len();

        // Find the end at the last cell the user *actually typed*, skipping
        // Claude Code's placeholder / ghost-suggestion text. Claude renders
        // that suggestion with the DIM attribute (SGR 2) and draws its block
        // cursor as an INVERSE cell (SGR 7) over the suggestion's first char.
        // Verified against real Claude PTY output for an empty input:
        //   `❯ \x1b[7mT\x1b[27m\x1b[2mry "refactor …"\x1b[22m`
        // (first char inverse, remainder dim). Capturing those cells would let
        // a plain-Enter comment-send paste & submit Claude's own suggestion
        // when the user hasn't typed anything (or has typed only a prefix
        // Claude is auto-completing). So treat trailing DIM/INVERSE cells as
        // non-input: the selection ends after the last real (non-dim,
        // non-inverse, non-blank) cell, and an input that contains only
        // placeholder/ghost text selects nothing.
        let mut end: Option<(usize, usize)> = None;
        for r in prompt_row..=input_end_row {
            let row = match self.terminal.grid.row(r) {
                Some(row) => row,
                None => continue,
            };
            let start = if r == prompt_row { input_start_col } else { 0 };
            let scan_end = row.content_end_col.min(row.cells.len());
            for col in start..scan_end {
                if let Some(cell) = row.cells.get(col) {
                    // Ghost = Claude's DIM auto-suggestion. Its first char is
                    // drawn INVERSE (the block cursor over it) FOLLOWED by DIM
                    // suggestion text. So an inverse cell is only ghost when the
                    // NEXT cell is dim. A bare inverse cursor sitting on the
                    // user's real last typed char (next cell blank, not dim)
                    // must NOT be excluded — otherwise Cmd+A select-all ends one
                    // char short (or selects nothing) and the follow-up delete
                    // leaves text behind.
                    let next_is_dim = row
                        .cells
                        .get(col + 1)
                        .is_some_and(|n| n.attrs.contains(CellAttrs::DIM));
                    let is_ghost = cell.attrs.contains(CellAttrs::DIM)
                        || (cell.attrs.contains(CellAttrs::INVERSE) && next_is_dim);
                    if cell.grapheme != ' ' && !is_ghost {
                        // get_selected_text treats the active column as
                        // inclusive, so point at the last real cell itself,
                        // not one past it (which could be the inverse cursor
                        // cell or a dim ghost char).
                        end = Some((col, r));
                    }
                }
            }
        }

        let (end_col, end_row) = match end {
            Some(e) => e,
            None => return false,
        };

        self.selection = Selection {
            anchor: (input_start_col, sb_len + prompt_row),
            active: (end_col, sb_len + end_row),
            is_active: true,
            block_mode: false,
        };
        true
    }

    /// Whether the running program has enabled mouse tracking (DECSET 1000/1002/1003).
    /// When true, mouse clicks should be reported to the PTY instead of starting selection.
    pub fn mouse_tracking_enabled(&self) -> bool {
        self.terminal.modes.mouse_tracking != immorterm_core::terminal::MouseMode::None
    }

    /// Encode a mouse event for the PTY based on the terminal's mouse format.
    /// `button`: 0=left, 1=middle, 2=right. `pressed`: true=press, false=release.
    /// `css_x`/`css_y`: click position in CSS pixels (converted to cell coordinates).
    /// Returns the escape sequence to send to the PTY, or empty if mouse tracking is off.
    pub fn encode_mouse_event(&self, button: u8, pressed: bool, css_x: f32, css_y: f32) -> Vec<u8> {
        use immorterm_core::terminal::{MouseMode, MouseFormat};

        if self.terminal.modes.mouse_tracking == MouseMode::None {
            return Vec::new();
        }

        let (col, row) = self.css_to_cell(css_x, css_y);
        // Terminal mouse coordinates are 1-based
        let cx = col + 1;
        let cy = row + 1;

        match self.terminal.modes.mouse_format {
            MouseFormat::Sgr => {
                // SGR format: \x1b[<button;col;row M (press) or m (release)
                let suffix = if pressed { 'M' } else { 'm' };
                let cb = button;
                format!("\x1b[<{};{};{}{}", cb, cx, cy, suffix).into_bytes()
            }
            MouseFormat::Normal => {
                if !pressed {
                    // Normal mode: release = button 3
                    let mut seq = vec![0x1b, b'[', b'M'];
                    seq.push(b' ' + 3); // release
                    seq.push(b' ' + cx.min(222) as u8);
                    seq.push(b' ' + cy.min(222) as u8);
                    seq
                } else {
                    let mut seq = vec![0x1b, b'[', b'M'];
                    seq.push(b' ' + button);
                    seq.push(b' ' + cx.min(222) as u8);
                    seq.push(b' ' + cy.min(222) as u8);
                    seq
                }
            }
            MouseFormat::Utf8 | MouseFormat::Urxvt => {
                // Fallback to SGR for these (most compatible)
                let suffix = if pressed { 'M' } else { 'm' };
                format!("\x1b[<{};{};{}{}", button, cx, cy, suffix).into_bytes()
            }
        }
    }

    /// Find the prompt row (the row containing `❯`) by scanning from the bottom.
    /// Returns (grid_row, col_after_prompt) — the row index and the column
    /// right after `❯ ` where user input begins.
    fn find_prompt_row(&self) -> Option<(usize, usize)> {
        let grid = &self.terminal.grid;
        // Scan from bottom up — prompt is near the bottom
        for r in (0..grid.num_rows()).rev() {
            if let Some(row) = grid.row(r) {
                for (c, cell) in row.cells.iter().enumerate() {
                    if cell.grapheme == '❯' {
                        // Input starts after "❯ " (prompt + space)
                        return Some((r, c + 2));
                    }
                }
            }
        }
        None
    }

    /// Find the end row of the input area. The input box is bounded below by a
    /// horizontal separator row (row of ─ / ╌ / ━ chars). Scans from `start_row + 1`
    /// downward for such a row; returns the row just before it.
    /// If no separator is found, returns the last grid row.
    fn find_input_end_row(&self, start_row: usize) -> usize {
        let grid = &self.terminal.grid;
        let num_rows = grid.num_rows();
        for r in (start_row + 1)..num_rows {
            if let Some(row) = grid.row(r) {
                // Count horizontal-rule characters in this row
                let rule_chars = row.cells.iter().filter(|c| {
                    matches!(c.grapheme, '─' | '╌' | '━' | '═' | '┄' | '┈')
                }).count();
                // If row is mostly rule chars, treat as separator
                if rule_chars * 2 >= row.cells.len() {
                    return r.saturating_sub(1);
                }
            }
        }
        num_rows.saturating_sub(1)
    }

    /// Debug probe for click-to-cursor: dumps the full planner state as JSON.
    /// Call this RIGHT BEFORE sending the Dijkstra byte sequence, then inspect
    /// the terminal after the keys are processed to compare actual vs. planned.
    /// Identifies mismatches in: prompt detection, input bounds, visual cursor,
    /// row text snapshot (including trailing whitespace), and planned path.
    pub fn debug_click_trace(&self, css_x: f32, css_y: f32) -> String {
        // Same input conversion as click_to_cursor_sequence
        let (target_col_click, display_row) = self.css_to_cell(css_x, css_y);
        let sb_len = self.terminal.scrollback.len();
        let target_content = (sb_len + display_row).saturating_sub(self.scroll_offset);
        if target_content < sb_len {
            return "{\"error\":\"target_in_scrollback\"}".to_string();
        }
        let target_grid_row = target_content - sb_len;

        let prompt = self.find_prompt_row();
        let (prompt_row, input_start_col) = match prompt {
            Some(p) => p,
            None => return "{\"error\":\"no_prompt\"}".to_string(),
        };
        let input_end_row = self.find_input_end_row(prompt_row);
        let vc = self.find_visual_cursor();
        let (cur_col, cur_row) = match vc {
            Some(p) => p,
            None => return format!(
                "{{\"error\":\"no_visual_cursor\",\"prompt_row\":{},\"input_start_col\":{},\"input_end_row\":{}}}",
                prompt_row, input_start_col, input_end_row
            ),
        };

        // Build per-row bounds + text, same as click_to_cursor_sequence.
        let n_rows = input_end_row - prompt_row + 1;
        let mut rows_json = String::from("[");
        for (i, r) in (prompt_row..=input_end_row).enumerate() {
            let start = input_start_col;
            let (end, text, raw_len, cec, non_default_end, sample) = if let Some(row) = self.terminal.grid.row(r) {
                let raw: String = row.cells.iter().map(|c| c.grapheme).collect();
                let rlen = row.cells.len();
                let end = row.cells.iter()
                    .rposition(|c| c.grapheme != ' ')
                    .map(|p| p + 1)
                    .unwrap_or(start)
                    .max(start);
                // Last non-default cell position
                let nde = row.cells.iter()
                    .rposition(|c| !c.is_default())
                    .map(|p| p + 1)
                    .unwrap_or(0);
                // Sample 5 cells around the rposition boundary to see attrs
                let sample_start = end.saturating_sub(2);
                let sample_end = (end + 3).min(rlen);
                let mut sample_cells = String::new();
                for ci in sample_start..sample_end {
                    if let Some(c) = row.cells.get(ci) {
                        let ch = if c.grapheme == '"' { '?' } else if (c.grapheme as u32) < 0x20 { '.' } else { c.grapheme };
                        sample_cells.push_str(&format!(
                            "{{\"col\":{},\"ch\":\"{}\",\"fg\":\"{:?}\",\"bg\":\"{:?}\",\"def\":{}}}",
                            ci, ch, c.fg, c.bg, c.is_default()
                        ));
                        if ci + 1 < sample_end { sample_cells.push(','); }
                    }
                }
                (end, raw, rlen, row.content_end_col, nde, sample_cells)
            } else {
                (start, String::new(), 0, 0, 0, String::new())
            };
            // Escape text for JSON (basic — quote, backslash, control chars)
            let text_esc: String = text.chars()
                .map(|c| match c {
                    '"' => "\\\"".to_string(),
                    '\\' => "\\\\".to_string(),
                    c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32),
                    c => c.to_string(),
                })
                .collect();
            if i > 0 { rows_json.push(','); }
            rows_json.push_str(&format!(
                "{{\"r\":{},\"start\":{},\"rpos_end\":{},\"non_default_end\":{},\"content_end_col\":{},\"raw_len\":{},\"cells\":[{}],\"text\":\"{}\"}}",
                r, start, end, non_default_end, cec, raw_len, sample, text_esc
            ));
        }
        rows_json.push(']');

        // Run Dijkstra plan from the visual cursor to the target.
        // correction_seq uses visual_cursor as start, matching what the
        // real click-to-cursor path does on its first invocation.
        let bytes = self.click_to_cursor_correction_seq(target_col_click, target_grid_row);
        let bytes_hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");

        format!(
            "{{\"prompt_row\":{},\"input_start_col\":{},\"input_end_row\":{},\"n_rows\":{},\
             \"visual_cursor\":{{\"row\":{},\"col\":{}}},\
             \"target\":{{\"row\":{},\"col\":{}}},\
             \"bytes_hex\":\"{}\",\"byte_count\":{},\
             \"rows\":{}}}",
            prompt_row, input_start_col, input_end_row, n_rows,
            cur_row, cur_col, target_grid_row, target_col_click,
            bytes_hex, bytes.len(), rows_json
        )
    }

    /// Debug: return cursor position and click target for diagnostics.
    pub fn debug_click_info(&self, css_x: f32, css_y: f32) -> String {
        let (target_col, display_row) = self.css_to_cell(css_x, css_y);
        let sb_len = self.terminal.scrollback.len();
        let target_grid_row = (sb_len + display_row).saturating_sub(self.scroll_offset).saturating_sub(sb_len);
        let cursor_row = self.terminal.cursor.row;
        let cursor_col = self.terminal.cursor.col;
        let prompt = self.find_prompt_row();

        format!(
            "cursor=({},{}) target=({},{}) grid_row={} prompt={:?}",
            cursor_col, cursor_row, target_col, display_row,
            target_grid_row, prompt
        )
    }

    /// Detect a clickable link (URL or file path) at the given CSS coords.
    /// Returns JSON: `{"kind":"url"|"file"|"osc8","text":"...","row":N,"start":N,"end":N,"line":N?,"col":N?}`
    /// or empty string if no link at that cell. OSC 8 native hyperlinks take
    /// precedence over text-scanned URLs/paths when a cell has both.
    pub fn link_at(&self, css_x: f32, css_y: f32) -> String {
        use immorterm_core::links::LinkKind;
        let (click_col, display_row) = self.css_to_cell(css_x, css_y);
        let content_row = self.display_to_content(display_row);
        let sb_len = self.terminal.scrollback.len();

        // Selection-aware preview: if the user has a committed text selection and
        // is hovering inside it, treat the hand-picked substring as the link
        // target instead of auto-detecting under the cursor. Lets the user narrow
        // a preview to a chosen span (e.g. select `/tmp/var` out of
        // `/tmp/var/image.png` and hover it to browse the directory).
        if self.selection.is_active && self.selection.contains(click_col, content_row) {
            let text = self.get_selected_text();
            let trimmed = text.trim();
            // Only hijack the hover for path/URL-like selections — that's the
            // feature's purpose (select a partial path, hover to browse). For any
            // other selected text (e.g. a `[Image #N]` marker the user happened to
            // select), fall through to normal auto-detection so image / OSC8 /
            // task hovers still work. A lingering selection must not swallow them.
            let is_url = trimmed.starts_with("http://") || trimmed.starts_with("https://");
            let looks_pathy = is_url || trimmed.contains('/');
            if !trimmed.is_empty() && !trimmed.contains('\n') && looks_pathy {
                let kind = if is_url { "url" } else { "file" };
                return format!(
                    "{{\"kind\":\"{}\",\"text\":{},\"row\":{},\"start\":{},\"end\":{},\"selection\":true}}",
                    kind,
                    serde_json::to_string(trimmed).unwrap_or_default(),
                    display_row, 0, click_col + 1
                );
            }
        }

        let get_row = |cr: usize| -> Option<&immorterm_core::grid::Row> {
            if cr < sb_len {
                self.terminal.scrollback.get(cr)
            } else {
                self.terminal.grid.row(cr - sb_len)
            }
        };

        // OSC 8 fast path: if the clicked cell carries a native hyperlink id,
        // resolve it directly and skip the text scanner. Walk left/right on the
        // same row to find the contiguous span that shares the id, so the
        // underline covers the whole link.
        if let Some(row) = get_row(content_row)
            && let Some(cell) = row.cells.get(click_col)
            && cell.hyperlink_id != 0
            && let Some(uri) = self.terminal.hyperlink_uri(cell.hyperlink_id)
        {
            let id = cell.hyperlink_id;
            let mut span_start = click_col;
            while span_start > 0
                && row.cells.get(span_start - 1).is_some_and(|c| c.hyperlink_id == id)
            {
                span_start -= 1;
            }
            let mut span_end = click_col + 1;
            while row.cells.get(span_end).is_some_and(|c| c.hyperlink_id == id) {
                span_end += 1;
            }
            return format!(
                "{{\"kind\":\"osc8\",\"text\":{},\"row\":{},\"start\":{},\"end\":{}}}",
                serde_json::to_string(uri).unwrap_or_default(),
                display_row, span_start, span_end
            );
        }

        // A row is a "continuation candidate" if its last cell is non-blank —
        // either soft-wrapped by the terminal, OR full-width written by an app
        // like Ink/Claude Code that uses explicit \r\n instead of soft-wrap.
        let last_nonblank_len = |r: &immorterm_core::grid::Row| -> usize {
            let mut n = 0usize;
            for (i, c) in r.cells.iter().enumerate() {
                if c.grapheme != ' ' && c.grapheme != '\0' {
                    n = i + 1;
                }
            }
            n
        };
        let continues_forward = |r: &immorterm_core::grid::Row| -> bool {
            r.is_soft_wrapped() || last_nonblank_len(r) == r.cells.len()
        };

        // Walk backward while the PREVIOUS row looks like it continues into this one.
        let mut start = content_row;
        while start > 0 {
            if let Some(prev) = get_row(start - 1)
                && continues_forward(prev)
            {
                start -= 1;
                continue;
            }
            break;
        }

        // Stitch forward across continuation rows, skipping wide-char
        // continuation cells (width == 0) that would inject garbage into
        // the combined text and break the link scanner.
        let mut combined = String::new();
        let mut row_char_offsets: Vec<usize> = Vec::new();
        let mut row_col_to_char: Vec<Vec<usize>> = Vec::new();
        for cur in (start..).take(16) {
            let r = match get_row(cur) { Some(r) => r, None => break };
            row_char_offsets.push(combined.chars().count());
            let nb = last_nonblank_len(r);
            let cont = continues_forward(r);
            let take = if cont { r.cells.len() } else { nb };
            let mut col_map = Vec::with_capacity(r.cells.len());
            let mut char_idx = combined.chars().count();
            for c in r.cells.iter().take(take) {
                col_map.push(char_idx);
                if c.width != 0 {
                    combined.push(c.grapheme);
                    char_idx += 1;
                }
            }
            // Pad to full row width so any col index is valid
            while col_map.len() < r.cells.len() {
                col_map.push(char_idx);
            }
            row_col_to_char.push(col_map);
            if !cont { break; }
        }
        if combined.is_empty() { return String::new(); }

        let click_row_idx = content_row.saturating_sub(start);
        if click_row_idx >= row_col_to_char.len() { return String::new(); }
        let click_char_offset = row_col_to_char[click_row_idx]
            .get(click_col)
            .copied()
            .unwrap_or(0);

        let mut spans = Vec::new();
        immorterm_core::links::scan_row(&combined, 0, &mut spans);
        let hit = spans.iter().find(|s| {
            (s.start as usize) <= click_char_offset && click_char_offset < (s.end as usize)
        });

        match hit {
            Some(span) => match &span.kind {
                LinkKind::Url(u) => format!(
                    "{{\"kind\":\"url\",\"text\":{},\"row\":{},\"start\":{},\"end\":{}}}",
                    serde_json::to_string(u).unwrap_or_default(),
                    display_row, span.start, span.end
                ),
                LinkKind::File { path, line, col: lcol } => {
                    let line_s = line.map(|n| n.to_string()).unwrap_or_else(|| "null".into());
                    let col_s = lcol.map(|n| n.to_string()).unwrap_or_else(|| "null".into());
                    format!(
                        "{{\"kind\":\"file\",\"text\":{},\"row\":{},\"start\":{},\"end\":{},\"line\":{},\"col\":{}}}",
                        serde_json::to_string(path).unwrap_or_default(),
                        display_row, span.start, span.end, line_s, col_s
                    )
                }
                LinkKind::HexColor(hex) => format!(
                    "{{\"kind\":\"hex-color\",\"text\":{},\"row\":{},\"start\":{},\"end\":{}}}",
                    serde_json::to_string(hex).unwrap_or_default(),
                    display_row, span.start, span.end
                ),
                LinkKind::ClaudeImage(n) => format!(
                    "{{\"kind\":\"claude-image\",\"text\":\"[Image #{n}]\",\"n\":{n},\"row\":{},\"start\":{},\"end\":{}}}",
                    display_row, span.start, span.end
                ),
            },
            None => {
                // Task summary line: "N tasks (X done, ...)" or "… +N completed"
                // Shows an at-a-glance overview of all session tasks on hover.
                if let Some(row) = get_row(content_row) {
                    let row_text: String = row.cells.iter()
                        .filter(|c| c.width > 0)
                        .map(|c| c.grapheme)
                        .collect();
                    let trimmed = row_text.trim();
                    let is_summary = trimmed.contains(" tasks (")
                        && trimmed.chars().next().is_some_and(|c| c.is_ascii_digit());
                    let is_expand = trimmed.starts_with('…') && trimmed.contains("completed");
                    if is_summary || is_expand {
                        return format!(
                            "{{\"kind\":\"task-summary\",\"text\":{},\"row\":{},\"start\":0,\"end\":{}}}",
                            serde_json::to_string(trimmed).unwrap_or_default(),
                            display_row, row.cells.len()
                        );
                    }
                }

                // Task-line detection: scan the first ~8 columns for a status
                // icon (◼/◻/✔/■/□/✓/●/○) followed by a space. Skips leading
                // decorative chars like `⎿` that Claude Code uses for indentation.
                // The extension host resolves the subject against ImmorTerm Memory
                // — no match means no preview.
                if let Some(row) = get_row(content_row) {
                    let scan_limit = row.cells.len().min(8);
                    let mut icon_pos = None;
                    for i in 0..scan_limit {
                        if matches!(row.cells[i].grapheme,
                            '◼' | '◻' | '✔' | '■' | '□' | '✓' | '●' | '○' | '◉')
                        {
                            icon_pos = Some(i);
                            break;
                        }
                    }
                    if let Some(i) = icon_pos
                        && i + 1 < row.cells.len()
                        && row.cells[i + 1].grapheme == ' '
                    {
                        let text_start = i + 2;
                        let mut subject = String::new();
                        for c in row.cells[text_start..].iter() {
                            if c.width > 0 { subject.push(c.grapheme); }
                        }
                        let subject = subject.trim_end();
                        if !subject.is_empty() {
                            return format!(
                                "{{\"kind\":\"task\",\"text\":{},\"row\":{},\"start\":{},\"end\":{}}}",
                                serde_json::to_string(subject).unwrap_or_default(),
                                display_row, i, row.cells.len()
                            );
                        }
                    }
                }
                String::new()
            }
        }
    }

    /// Generate arrow keys to move the input cursor to the clicked position.
    /// Supports multi-row input: emits Up/Down for row delta, Left/Right for col
    /// delta. Uses the visual (INVERSE) cursor as the true current position since
    /// Ink parks the terminal cursor in the status bar between renders.
    /// Also returns the (row, col) target so JS can draw a phantom cursor.
    pub fn click_to_cursor_sequence(&self, css_x: f32, css_y: f32) -> Vec<u8> {
        let (target_col_click, display_row) = self.css_to_cell(css_x, css_y);
        let sb_len = self.terminal.scrollback.len();
        let target_content = (sb_len + display_row).saturating_sub(self.scroll_offset);

        // Can't navigate into scrollback
        if target_content < sb_len {
            return Vec::new();
        }
        let target_grid_row = target_content - sb_len;

        // Find the prompt row — this is our upper bound
        let (prompt_row, input_start_col) = match self.find_prompt_row() {
            Some(p) => p,
            None => return Vec::new(),
        };

        // Find the lower bound of the input area (separator row - 1)
        let input_end_row = self.find_input_end_row(prompt_row);

        // Clamp target row to [prompt_row, input_end_row]
        if target_grid_row < prompt_row || target_grid_row > input_end_row {
            return Vec::new();
        }

        // Get current cursor position — prefer the visual (INVERSE) cursor
        // since Ink parks the hardware cursor in the status bar.
        let (cur_col, cur_row) = match self.find_visual_cursor() {
            Some(pos) => pos,
            None => {
                // Fall back to terminal cursor if visible
                let cr = self.terminal.cursor.row;
                let cc = self.terminal.cursor.col;
                if cr >= prompt_row && cr <= input_end_row {
                    (cc, cr)
                } else {
                    return Vec::new();
                }
            }
        };

        // Visual cursor must be inside input area
        if cur_row < prompt_row || cur_row > input_end_row {
            return Vec::new();
        }

        // Build per-row data for input rows [prompt_row..=input_end_row].
        //
        // Row boundary: rposition(non-space) for content_end (Ctrl+E, word scan).
        // row_width = content_end on most rows. On the CURSOR ROW, extend to
        // the visual cursor position (includes trailing spaces the user typed).
        let n_rows = input_end_row - prompt_row + 1;
        let mut row_start: Vec<usize> = Vec::with_capacity(n_rows);
        let mut content_end: Vec<usize> = Vec::with_capacity(n_rows);
        let mut row_width: Vec<usize> = Vec::with_capacity(n_rows);
        let mut row_text: Vec<Vec<char>> = Vec::with_capacity(n_rows);
        for r in prompt_row..=input_end_row {
            let start = input_start_col;
            let (ce, text) = if let Some(row) = self.terminal.grid.row(r) {
                let ce = row.cells.iter()
                    .rposition(|c| c.grapheme != ' ')
                    .map(|p| p + 1)
                    .unwrap_or(start)
                    .max(start);
                let chars: Vec<char> = row.cells[start..ce].iter().map(|c| c.grapheme).collect();
                (ce, chars)
            } else {
                (start, Vec::new())
            };
            let rw = if r == cur_row { ce.max(cur_col) } else { ce };
            row_start.push(start);
            content_end.push(ce);
            row_width.push(rw);
            row_text.push(text);
        }

        // Clamp target to this row's width
        let row_min_col = input_start_col;
        let goal_ri = target_grid_row - prompt_row;
        let row_max_col = row_width[goal_ri];
        let target_col = target_col_click.clamp(row_min_col, row_max_col);

        if target_grid_row == cur_row && target_col == cur_col {
            return Vec::new();
        }

        let start_ri = cur_row - prompt_row;
        let start_col = cur_col.clamp(row_start[start_ri], row_width[start_ri]);
        let goal_col = target_col.clamp(row_start[goal_ri], row_width[goal_ri]);

        let image_spans = Self::build_image_spans(&row_text, &row_start);
        Self::plan_dijkstra(start_ri, start_col, goal_ri, goal_col, n_rows, &row_start, &content_end, &row_width, &row_text, &image_spans)
    }

    /// Delegates to [`immorterm_core::cursor_nav::plan_dijkstra`] — extracted for testability.
    ///
    /// All WASM-side callers navigate inside Claude Code's input box, so the
    /// `WrapEdge` row-crossing mode is hard-coded here: Up/Down moves are
    /// disabled (Ink discards them inside a wrapped logical line) and the
    /// planner crosses rows via Right-at-end / Left-at-start instead.
    #[allow(clippy::too_many_arguments)]
    fn plan_dijkstra(
        start_ri: usize, start_col: usize,
        goal_ri: usize, goal_col: usize,
        n_rows: usize,
        row_start: &[usize], content_end: &[usize], row_width: &[usize], row_text: &[Vec<char>],
        image_spans: &[Vec<(usize, usize)>],
    ) -> Vec<u8> {
        immorterm_core::cursor_nav::plan_dijkstra(
            start_ri, start_col, goal_ri, goal_col, n_rows,
            row_start, content_end, row_width, row_text, image_spans,
            immorterm_core::cursor_nav::RowCrossingMode::WrapEdge,
        )
    }

    /// Scan each row's text for `[Image #N]` placeholders and return their
    /// `[start_col, end_col_exclusive)` ranges. Ink renders these as 10-cell
    /// glyph blocks but stores them as a single buffer char — the planner
    /// needs to know so it emits one Right/Left arrow to traverse the block
    /// instead of one per cell.
    fn build_image_spans(row_text: &[Vec<char>], row_start: &[usize]) -> Vec<Vec<(usize, usize)>> {
        row_text.iter().enumerate().map(|(ri, chars)| {
            let s: String = chars.iter().collect();
            let mut spans = Vec::new();
            immorterm_core::links::scan_row(&s, 0, &mut spans);
            spans.into_iter()
                .filter_map(|sp| match sp.kind {
                    immorterm_core::links::LinkKind::ClaudeImage(_) => {
                        // Spans are in row-text char offsets (from row_start); shift
                        // into absolute grid columns so the planner can compare.
                        let rs = row_start[ri];
                        Some((rs + sp.start as usize, rs + sp.end as usize))
                    }
                    _ => None,
                })
                .collect()
        }).collect()
    }

    /// Plan a one-row Up/Down move inside Claude Code's input box.
    ///
    /// Ink's TextInput discards bare `\x1b[A`/`\x1b[B` when the cursor sits
    /// inside a *wrapped* logical line — the buffer has no next/previous
    /// logical line to navigate to. The Dijkstra planner with `WrapEdge`
    /// row-crossing crosses the row boundary via Right-at-end / Left-at-start
    /// (which Ink does honor: a single Right at the end of visual row 1
    /// lands the cursor at the start of visual row 2 in the wrapped buffer).
    ///
    /// Returns an empty Vec when:
    /// - No `❯` prompt is visible (not in Claude Code → caller sends raw
    ///   arrow byte for plain-shell history navigation).
    /// - The visual cursor is outside `[prompt_row, input_end_row]`.
    /// - The move would leave the input box (Up at top → history; Down at
    ///   bottom → history).
    fn plan_arrow_in_input(&self, dir: i32) -> Vec<u8> {
        let (prompt_row, input_start_col) = match self.find_prompt_row() {
            Some(p) => p,
            None => return Vec::new(),
        };
        let input_end_row = self.find_input_end_row(prompt_row);
        let (cur_col, cur_row) = match self.find_visual_cursor() {
            Some(p) => p,
            None => return Vec::new(),
        };
        if cur_row < prompt_row || cur_row > input_end_row {
            return Vec::new();
        }
        // Compute target row; bail (raw arrow → history) if outside input.
        let target_row = if dir < 0 {
            if cur_row == prompt_row { return Vec::new(); }
            cur_row - 1
        } else {
            if cur_row == input_end_row { return Vec::new(); }
            cur_row + 1
        };

        // Build per-row arrays — same shape as click_to_cursor_correction_seq.
        let n_rows = input_end_row - prompt_row + 1;
        let mut row_start: Vec<usize> = Vec::with_capacity(n_rows);
        let mut content_end: Vec<usize> = Vec::with_capacity(n_rows);
        let mut row_width: Vec<usize> = Vec::with_capacity(n_rows);
        let mut row_text: Vec<Vec<char>> = Vec::with_capacity(n_rows);
        for r in prompt_row..=input_end_row {
            let start = input_start_col;
            let (ce, text) = if let Some(row) = self.terminal.grid.row(r) {
                let ce = row.cells.iter()
                    .rposition(|c| c.grapheme != ' ')
                    .map(|p| p + 1)
                    .unwrap_or(start)
                    .max(start);
                let chars: Vec<char> = row.cells[start..ce].iter().map(|c| c.grapheme).collect();
                (ce, chars)
            } else {
                (start, Vec::new())
            };
            let rw = if r == cur_row { ce.max(cur_col) } else { ce };
            row_start.push(start);
            content_end.push(ce);
            row_width.push(rw);
            row_text.push(text);
        }
        let start_ri = cur_row - prompt_row;
        let start_col = cur_col.clamp(row_start[start_ri], row_width[start_ri]);
        let goal_ri = target_row - prompt_row;
        let goal_col = cur_col.clamp(row_start[goal_ri], row_width[goal_ri]);
        let image_spans = Self::build_image_spans(&row_text, &row_start);
        Self::plan_dijkstra(start_ri, start_col, goal_ri, goal_col, n_rows, &row_start, &content_end, &row_width, &row_text, &image_spans)
    }

    /// Correction planner: re-plan the shortest path from the CURRENT visual
    /// cursor position to the given target cell, using the same Dijkstra
    /// planner as the initial click. This is called by the rAF correction
    /// loop so drift corrections can leverage word jumps / Home / End instead
    /// of spamming raw arrow keys.
    pub fn click_to_cursor_correction_seq(&self, target_col: usize, target_grid_row: usize) -> Vec<u8> {
        let (prompt_row, input_start_col) = match self.find_prompt_row() {
            Some(p) => p,
            None => return Vec::new(),
        };
        let input_end_row = self.find_input_end_row(prompt_row);
        if target_grid_row < prompt_row || target_grid_row > input_end_row {
            return Vec::new();
        }
        let (cur_col, cur_row) = match self.find_visual_cursor() {
            Some(p) => p,
            None => return Vec::new(),
        };
        if cur_row < prompt_row || cur_row > input_end_row {
            return Vec::new();
        }
        let n_rows = input_end_row - prompt_row + 1;
        let mut row_start: Vec<usize> = Vec::with_capacity(n_rows);
        let mut content_end: Vec<usize> = Vec::with_capacity(n_rows);
        let mut row_width: Vec<usize> = Vec::with_capacity(n_rows);
        let mut row_text: Vec<Vec<char>> = Vec::with_capacity(n_rows);
        for r in prompt_row..=input_end_row {
            let start = input_start_col;
            let (ce, text) = if let Some(row) = self.terminal.grid.row(r) {
                let ce = row.cells.iter()
                    .rposition(|c| c.grapheme != ' ')
                    .map(|p| p + 1)
                    .unwrap_or(start)
                    .max(start);
                let chars: Vec<char> = row.cells[start..ce].iter().map(|c| c.grapheme).collect();
                (ce, chars)
            } else {
                (start, Vec::new())
            };
            let rw = if r == cur_row { ce.max(cur_col) } else { ce };
            row_start.push(start);
            content_end.push(ce);
            row_width.push(rw);
            row_text.push(text);
        }
        let start_ri = cur_row - prompt_row;
        let start_col = cur_col.clamp(row_start[start_ri], row_width[start_ri]);
        let goal_ri = target_grid_row - prompt_row;
        let goal_col = target_col.clamp(row_start[goal_ri], row_width[goal_ri]);
        let image_spans = Self::build_image_spans(&row_text, &row_start);
        Self::plan_dijkstra(start_ri, start_col, goal_ri, goal_col, n_rows, &row_start, &content_end, &row_width, &row_text, &image_spans)
    }

    /// Returns the current visual cursor position (the INVERSE cell Ink draws)
    /// as a 2-element array [col, display_row]. Returns [-1, -1] if no cursor
    /// is visible. `display_row` is relative to the visible viewport (accounting
    /// for scroll_offset), so JS can position overlays directly in CSS coords.
    pub fn visual_cursor_display(&self) -> Vec<i32> {
        match self.find_visual_cursor() {
            Some((col, grid_row)) => {
                let disp_row = grid_row as i32 - self.scroll_offset as i32;
                vec![col as i32, disp_row]
            }
            None => vec![-1, -1],
        }
    }

    /// Return the grapheme at `(grid_row, col)` as a String, or empty string
    /// if out of bounds. Used by the mask overlay to render the masked character
    /// in white on the mask's black background (since the underlying INVERSE
    /// cursor can't be visually suppressed in the GPU canvas).
    pub fn cell_grapheme_at(&self, grid_row: usize, col: usize) -> String {
        self.terminal.grid.row(grid_row)
            .and_then(|r| r.cells.get(col))
            .map(|c| c.grapheme.to_string())
            .unwrap_or_default()
    }

    /// Paste-undo probe: snapshot the prompt input region for the
    /// Cmd/Ctrl+Z paste-undo flow. JS calls this (a) right before sending a
    /// paste, (b) after the echo settles, and (c) on the undo keypress — and
    /// diffs the snapshots itself; Rust stays stateless so session switches /
    /// snapshot reloads can't leave a stale undo record behind.
    ///
    /// Returns JSON:
    ///   {"ok":true,"prompt_row":N,"input_start_col":N,"input_end_row":N,
    ///    "cursor_col":N,"cursor_row":N,        // grid coords, -1 if no cursor
    ///    "rows":["…","…"]}                      // per-row real input text
    /// or {"ok":false,"reason":"no_prompt"}.
    ///
    /// Row text rules (match the click-to-cursor planner + the ghost-suggestion
    /// fix in select_all_input):
    ///   - every row starts at `input_start_col` (wrapped rows align under ❯)
    ///   - each row is truncated after its last REAL cell: non-blank, non-DIM,
    ///     non-INVERSE. This drops Claude Code's dim ghost-suggestion and the
    ///     INVERSE block cursor it draws over the ghost's first char, so the
    ///     before/after diff never sees ghost text as "pasted content".
    pub fn paste_undo_probe(&self) -> String {
        let (prompt_row, input_start_col) = match self.find_prompt_row() {
            Some(p) => p,
            None => return "{\"ok\":false,\"reason\":\"no_prompt\"}".to_string(),
        };
        let input_end_row = self.find_input_end_row(prompt_row);
        let (cursor_col, cursor_row) = match self.find_visual_cursor() {
            Some((c, r)) => (c as i64, r as i64),
            None => (-1, -1),
        };
        let mut rows_json = String::from("[");
        for (i, r) in (prompt_row..=input_end_row).enumerate() {
            let text: String = if let Some(row) = self.terminal.grid.row(r) {
                let scan_end = row.cells.len();
                // Last real cell: non-blank, non-dim, non-inverse.
                let real_end = row.cells[..scan_end].iter()
                    .rposition(|c| {
                        c.grapheme != ' '
                            && c.grapheme != '\0'
                            && !c.attrs.contains(CellAttrs::DIM)
                            && !c.attrs.contains(CellAttrs::INVERSE)
                    })
                    .map(|p| p + 1)
                    .unwrap_or(input_start_col);
                let lo = input_start_col.min(scan_end);
                let hi = real_end.max(lo).min(scan_end);
                row.cells[lo..hi].iter()
                    .filter(|c| c.grapheme != '\0')
                    .map(|c| c.grapheme)
                    .collect()
            } else {
                String::new()
            };
            if i > 0 {
                rows_json.push(',');
            }
            rows_json.push_str(&serde_json::to_string(&text).unwrap_or_default());
        }
        rows_json.push(']');
        format!(
            "{{\"ok\":true,\"prompt_row\":{},\"input_start_col\":{},\"input_end_row\":{},\"cursor_col\":{},\"cursor_row\":{},\"rows\":{}}}",
            prompt_row, input_start_col, input_end_row, cursor_col, cursor_row, rows_json,
        )
    }

    /// Like `click_to_cursor_sequence` but also returns the target cell coords.
    /// Returns a JSON string: `{"seq":[...], "target_col":N, "target_row":N, "cur_col":N, "cur_row":N}`
    /// so JS can draw a phantom cursor at the target immediately.
    pub fn click_to_cursor_plan(&self, css_x: f32, css_y: f32) -> String {
        let (target_col_click, display_row) = self.css_to_cell(css_x, css_y);
        let sb_len = self.terminal.scrollback.len();
        let target_content = (sb_len + display_row).saturating_sub(self.scroll_offset);
        if target_content < sb_len {
            return String::from("{}");
        }
        let target_grid_row = target_content - sb_len;
        let (prompt_row, input_start_col) = match self.find_prompt_row() {
            Some(p) => p,
            None => return String::from("{}"),
        };
        let input_end_row = self.find_input_end_row(prompt_row);
        if target_grid_row < prompt_row || target_grid_row > input_end_row {
            return String::from("{}");
        }
        let (cur_col, cur_row) = match self.find_visual_cursor() {
            Some(pos) => pos,
            None => return String::from("{}"),
        };
        // rposition for non-space content; extend to cursor on cursor row
        let row_min_col = input_start_col;
        let rpos = self.terminal.grid.row(target_grid_row)
            .map(|r| r.cells.iter()
                .rposition(|c| c.grapheme != ' ')
                .map(|p| p + 1)
                .unwrap_or(row_min_col)
                .max(row_min_col))
            .unwrap_or(row_min_col);
        let row_max_col = if target_grid_row == cur_row { rpos.max(cur_col) } else { rpos };
        let target_col_raw = target_col_click.clamp(row_min_col, row_max_col);
        // Snap target out of any `[Image #N]` interior — the cursor can't rest
        // inside the glyph block (Ink stores it as 1 char), so the phantom
        // would mislead the user. Pick the nearer edge.
        let target_col = if let Some(row) = self.terminal.grid.row(target_grid_row) {
            let row_chars: Vec<char> = row.cells[row_min_col..row_max_col.min(row.cells.len())]
                .iter().map(|c| c.grapheme).collect();
            let row_chars_str: String = row_chars.iter().collect();
            let mut spans = Vec::new();
            immorterm_core::links::scan_row(&row_chars_str, 0, &mut spans);
            let mut snapped = target_col_raw;
            for sp in spans {
                if matches!(sp.kind, immorterm_core::links::LinkKind::ClaudeImage(_)) {
                    let s = row_min_col + sp.start as usize;
                    let e = row_min_col + sp.end as usize;
                    if target_col_raw > s && target_col_raw < e {
                        snapped = if target_col_raw - s <= e - target_col_raw { s } else { e };
                        break;
                    }
                }
            }
            snapped
        } else { target_col_raw };

        let display_target_row = (sb_len + target_grid_row).saturating_sub(sb_len + self.scroll_offset).saturating_add(self.scroll_offset);
        // Simpler: display row = target_grid_row - scroll_offset (scrolled viewport)
        let _ = display_target_row;
        let disp_row = target_grid_row as i32 - self.scroll_offset as i32;
        let disp_cur_row = cur_row as i32 - self.scroll_offset as i32;

        let row_delta = (target_grid_row as i32) - (cur_row as i32);
        let col_delta = (target_col as i32) - (cur_col as i32);
        let seq_len = row_delta.unsigned_abs() + col_delta.unsigned_abs();

        format!(
            "{{\"target_col\":{},\"target_row\":{},\"cur_col\":{},\"cur_row\":{},\"disp_row\":{},\"disp_cur_row\":{},\"seq_len\":{}}}",
            target_col, target_grid_row, cur_col, cur_row, disp_row, disp_cur_row, seq_len
        )
    }

    /// Generate the keystrokes needed to delete the currently selected text.
    ///
    /// Strategy (Cmd+A → Delete / Backspace on multi-row Ink input):
    /// 1. Use the Dijkstra planner (same as `click_to_cursor_sequence`) to
    ///    navigate from the current visual cursor to the END OF CONTENT on
    ///    the last input row. We use `rposition(non-space)` for the goal
    ///    column so we land exactly where Ink's internal cursor ends —
    ///    `content_end_col` (grid field) includes trailing spaces which
    ///    Ink's buffer model doesn't track, causing overshoot.
    /// 2. Append `total_chars` Backspaces. `total_chars` is the sum of
    ///    visible, non-trailing-space characters across all input rows
    ///    (+ 1 per hard-wrap row boundary). Extras are safe — readline
    ///    no-ops Backspace at input start.
    ///
    /// Prior failures (don't revive):
    /// - Raw `\x05\x15` (Ctrl+E + Ctrl+U): Ink's readline kills only the
    ///   current VISUAL line, not the whole multi-row buffer.
    /// - `\x1b[B × N + \x05 + \x7f × N`: Ink doesn't cascade multiple
    ///   Down arrows in a single batch past the current row — Down at
    ///   end-of-input routes elsewhere (history) instead of clamping.
    pub fn delete_selection_sequence(&self) -> Vec<u8> {
        if !self.selection.is_active {
            return Vec::new();
        }
        let sb_len = self.terminal.scrollback.len();
        let ((sc, sr), (ec, er)) = self.selection.range();

        // Only works for on-screen selections (not scrollback)
        if sr < sb_len || er < sb_len {
            return Vec::new();
        }

        // Locate the Ink input area.
        let (prompt_row, input_start_col) = match self.find_prompt_row() {
            Some(p) => p,
            None => return Vec::new(),
        };
        let input_end_row = self.find_input_end_row(prompt_row);

        // Selection must fall inside the input area (not stray output).
        let sel_start_row = sr - sb_len;
        let sel_end_row = er - sb_len;
        if sel_end_row < prompt_row || sel_start_row > input_end_row {
            return Vec::new();
        }

        // Current visual cursor — the INVERSE cell Ink renders (hardware
        // cursor is parked in the status bar). Scan ONLY the input-area rows:
        // newer Claude builds draw INVERSE cells in UI below the input
        // (hints/menus/footers), and the old whole-grid bottom-up
        // find_visual_cursor() picked those up — the out-of-input range guard
        // then bailed and the delete became a silent no-op (Cmd+A highlighted,
        // Backspace did nothing). If no cursor cell is rendered inside the
        // input at all (e.g. type-ahead while Claude is busy), fall back to
        // Ink's resting position: the end of the input content.
        let mut vc: Option<(usize, usize)> = None;
        'cursor: for r in (prompt_row..=input_end_row).rev() {
            if let Some(row) = self.terminal.grid.row(r) {
                // Same per-row precedence as find_visual_cursor — rightmost
                // INVERSE non-space (cursor over a char) first, then any
                // INVERSE (cursor on an empty position) — bounded to input rows.
                for col in (0..row.cells.len()).rev() {
                    if row.cells[col].attrs.contains(CellAttrs::INVERSE)
                        && row.cells[col].grapheme != ' '
                    {
                        vc = Some((col, r));
                        break 'cursor;
                    }
                }
                for col in (0..row.cells.len()).rev() {
                    if row.cells[col].attrs.contains(CellAttrs::INVERSE) {
                        vc = Some((col, r));
                        break 'cursor;
                    }
                }
            }
        }
        let (cur_col, cur_row) = vc.unwrap_or_else(|| {
            let mut r_last = prompt_row;
            let mut c_end = input_start_col;
            for r in prompt_row..=input_end_row {
                if let Some(row) = self.terminal.grid.row(r) {
                    let start = if r == prompt_row { input_start_col } else { 0 };
                    if let Some(p) = row.cells.iter().rposition(|c| c.grapheme != ' ')
                        && p + 1 > start
                    {
                        r_last = r;
                        c_end = p + 1;
                    }
                }
            }
            (c_end, r_last)
        });

        // Clamp selection boundaries to the input area.
        let sel_sr = sel_start_row.max(prompt_row);
        let sel_er = sel_end_row.min(input_end_row);
        let sel_sc = if sel_sr == prompt_row { sc.max(input_start_col) } else { sc };

        // Determine if this is a full select-all (covers entire input).
        let is_select_all = sel_sr == prompt_row
            && sel_er == input_end_row
            && sel_sc <= input_start_col;

        // Build per-row metrics identical to `click_to_cursor_sequence`:
        // rposition(non-space) for content_end, extend row_width to cursor
        // column on the cursor's row so the start node is reachable.
        let n_rows = input_end_row - prompt_row + 1;
        let mut row_start: Vec<usize> = Vec::with_capacity(n_rows);
        let mut content_end: Vec<usize> = Vec::with_capacity(n_rows);
        let mut row_width: Vec<usize> = Vec::with_capacity(n_rows);
        let mut row_text: Vec<Vec<char>> = Vec::with_capacity(n_rows);
        let mut total_chars: usize = 0;
        for r in prompt_row..=input_end_row {
            let start = if r == prompt_row { input_start_col } else { 0 };
            let (ce, text, hard_break) = if let Some(row) = self.terminal.grid.row(r) {
                let ce = row.cells.iter()
                    .rposition(|c| c.grapheme != ' ')
                    .map(|p| p + 1)
                    .unwrap_or(start)
                    .max(start);
                let chars: Vec<char> = row.cells[start..ce].iter().map(|c| c.grapheme).collect();
                let hard = !row.is_soft_wrapped();
                (ce, chars, hard)
            } else {
                (start, Vec::new(), false)
            };
            total_chars += ce.saturating_sub(start);
            if r < input_end_row && hard_break {
                total_chars += 1;
            }
            let rw = if r == cur_row { ce.max(cur_col) } else { ce };
            row_start.push(start);
            content_end.push(ce);
            row_width.push(rw);
            row_text.push(text);
        }
        if total_chars == 0 {
            return Vec::new();
        }

        let image_spans = Self::build_image_spans(&row_text, &row_start);

        if is_select_all {
            // === SELECT-ALL: navigate to end of all content, backspace everything ===
            let start_ri = cur_row - prompt_row;
            let goal_ri = input_end_row - prompt_row;
            let start_col = cur_col.clamp(row_start[start_ri], row_width[start_ri]);
            let goal_col = content_end[goal_ri];

            let mut seq = Self::plan_dijkstra(
                start_ri, start_col, goal_ri, goal_col, n_rows,
                &row_start, &content_end, &row_width, &row_text,
                &image_spans,
            );
            // Belt-and-suspenders Ctrl+E so Ink's end-of-line is reached.
            seq.push(0x05);
            seq.extend(std::iter::repeat_n(0x7f, total_chars));
            seq
        } else {
            // === PARTIAL SELECTION ===
            // `ec` (selection's active col) is INCLUSIVE per Selection::contains
            // (`col >= sc && col <= ec`), but `row_ce` / `content_end` is
            // EXCLUSIVE (rposition + 1). Treat ec as inclusive consistently by
            // using `ec + 1` everywhere we need an exclusive end column —
            // otherwise drag-selecting "hello" and releasing on the 'o' cell
            // gives sel_chars = N-1 and lands the cursor AT 'o' (not past it),
            // so N-1 backspaces leave the last char alive.
            let ec_excl = ec.saturating_add(1);
            let mut sel_chars: usize = 0;
            for r in sel_sr..=sel_er {
                let ri = r - prompt_row;
                let row_s = row_start[ri];
                let row_ce = content_end[ri];
                let col_start = if r == sel_sr { sel_sc.max(row_s) } else { row_s };
                let col_end = if r == sel_er { ec_excl.min(row_ce) } else { row_ce };
                // Count CHARS, not cells: wide chars (CJK/emoji) and multi-codepoint
                // grapheme clusters span 2 cells but are 1 readline char — and curly
                // quotes / typographic punctuation pasted from rich-text apps can
                // arrive as cluster + width=0 continuation. Counting cells would
                // send too many backspaces and eat chars before the selection.
                // Mirrors `extract_selection_text`'s `cell.width > 0` filter.
                if let Some(row) = self.terminal.grid.row(r) {
                    sel_chars += row.cells[col_start..col_end.min(row.cells.len())]
                        .iter()
                        .filter(|c| c.width > 0)
                        .count();
                } else {
                    sel_chars += col_end.saturating_sub(col_start);
                }
                if r < sel_er
                    && let Some(row) = self.terminal.grid.row(r)
                    && !row.is_soft_wrapped()
                {
                    sel_chars += 1;
                }
            }
            if sel_chars == 0 {
                return Vec::new();
            }

            // Always navigate to selection END and backspace — proven reliable.
            // Never use forward-delete (inconsistent in Ink).
            let cursor_at_sel_end = cur_row == sel_er && cur_col == ec_excl;

            if cursor_at_sel_end {
                // Already at end → just backspace.
                vec![0x7f; sel_chars]
            } else {
                // Navigate to selection end, then backspace.
                let sel_end_ri = sel_er - prompt_row;
                let sel_end_col = ec_excl.clamp(row_start[sel_end_ri], content_end[sel_end_ri]);
                row_width[sel_end_ri] = row_width[sel_end_ri].max(sel_end_col);

                let start_ri = cur_row - prompt_row;
                let start_col = cur_col.clamp(row_start[start_ri], row_width[start_ri]);

                let mut seq = Self::plan_dijkstra(
                    start_ri, start_col, sel_end_ri, sel_end_col, n_rows,
                    &row_start, &content_end, &row_width, &row_text,
                    &image_spans,
                );
                seq.extend(std::iter::repeat_n(0x7f, sel_chars));
                seq
            }
        }
    }

    /// Check if there is an active selection (regular or pseudo-cursor).
    pub fn has_selection(&self) -> bool {
        self.selection.is_active || !self.pseudo_cursors.is_empty()
    }

    /// Get the selected text as a string. Returns empty string if no selection.
    /// In multi-cursor mode, returns all pseudo-cursor selections joined by newlines.
    /// In block mode, extracts the rectangular column range from each row.
    /// Use this for clipboard copy (Ctrl+C / Cmd+C).
    pub fn get_selected_text(&self) -> String {
        // Multi-cursor mode: return pseudo-cursor text
        if !self.pseudo_cursors.is_empty() {
            return self.get_pseudo_cursor_text();
        }
        if !self.selection.is_active {
            return String::new();
        }

        let sb_len = self.terminal.scrollback.len();
        let grid = &self.terminal.grid;

        if self.selection.block_mode {
            // Block (rectangular) selection: fixed column range on every row
            let min_col = self.selection.anchor.0.min(self.selection.active.0);
            let max_col = self.selection.anchor.0.max(self.selection.active.0);
            let min_row = self.selection.anchor.1.min(self.selection.active.1);
            let max_row = self.selection.anchor.1.max(self.selection.active.1);
            let mut text = String::new();

            for content_idx in min_row..=max_row {
                let row = if content_idx < sb_len {
                    self.terminal.scrollback.get(content_idx)
                } else {
                    grid.row(content_idx - sb_len)
                };

                if let Some(row) = row {
                    let end_col = max_col.min(row.cells.len().saturating_sub(1));
                    for col in min_col..=end_col {
                        if let Some(cell) = row.cells.get(col)
                            && cell.width > 0
                        {
                            text.push(cell.grapheme);
                        }
                    }
                    // Only insert newline for hard breaks; skip for soft-wrapped rows
                    if content_idx < max_row && !row.wrapped {
                        text.push('\n');
                    }
                }
            }

            // Trim trailing whitespace per line
            return text
                .lines()
                .map(|l| l.trim_end())
                .collect::<Vec<_>>()
                .join("\n");
        }

        // Normal (line) selection
        let ((sc, sr), (ec, er)) = self.selection.range();
        let mut text = String::new();
        let mut skip_leading_spaces = false;

        for content_idx in sr..=er {
            let row = if content_idx < sb_len {
                self.terminal.scrollback.get(content_idx)
            } else {
                grid.row(content_idx - sb_len)
            };

            if let Some(row) = row {
                let start_col = if content_idx == sr { sc } else { 0 };
                let end_col = if content_idx == er {
                    ec.min(row.cells.len().saturating_sub(1))
                } else {
                    row.cells.len().saturating_sub(1)
                };

                let mut row_started = false;
                for col in start_col..=end_col {
                    if let Some(cell) = row.cells.get(col)
                        && cell.width > 0
                    {
                        if skip_leading_spaces && !row_started && cell.grapheme == ' ' {
                            continue;
                        }
                        row_started = true;
                        skip_leading_spaces = false;
                        text.push(cell.grapheme);
                    }
                }
                skip_leading_spaces = false;
                if content_idx < er {
                    let trimmed = text.trim_end_matches(' ').len();
                    text.truncate(trimmed);

                    // Soft-wrap detection: explicit flags first, then fill heuristic.
                    // Override with structural analysis (blank lines, list markers,
                    // box-drawing separators) to catch false positives.
                    let mut treat_as_soft_wrap = row_started && row.is_soft_wrapped();
                    if treat_as_soft_wrap {
                        let next_idx = content_idx + 1;
                        let next_row = if next_idx < sb_len {
                            self.terminal.scrollback.get(next_idx)
                        } else {
                            grid.row(next_idx - sb_len)
                        };
                        if has_hard_break_signals(row, next_row) {
                            treat_as_soft_wrap = false;
                        }
                    }

                    if treat_as_soft_wrap {
                        text.push(' ');
                        skip_leading_spaces = true;
                    } else {
                        text.push('\n');
                    }
                }
            }
        }
        text.trim_end().to_string()
    }

    /// Return HTML of the selected text with inline styles for colors and formatting.
    /// Used for rich-text clipboard (text/html MIME type) so pastes into Slack,
    /// Notion, etc. preserve bold, colors, and code styling.
    pub fn get_selected_html(&self) -> String {
        if !self.selection.is_active {
            return String::new();
        }

        let sb_len = self.terminal.scrollback.len();
        let grid = &self.terminal.grid;

        // Collect styled spans per row, then join with proper line breaks.
        // Each "span" is a run of chars sharing the same visual style.
        #[derive(Clone, PartialEq)]
        struct Style {
            fg: immorterm_core::cell::Color,
            bg: immorterm_core::cell::Color,
            bold: bool,
            dim: bool,
            italic: bool,
            underline: bool,
            strikethrough: bool,
        }
        impl Style {
            fn from_cell(cell: &immorterm_core::cell::Cell) -> Self {
                use immorterm_core::cell::CellAttrs;
                Self {
                    fg: cell.fg,
                    bg: cell.bg,
                    bold: cell.attrs.contains(CellAttrs::BOLD),
                    dim: cell.attrs.contains(CellAttrs::DIM),
                    italic: cell.attrs.contains(CellAttrs::ITALIC),
                    underline: cell.attrs.contains(CellAttrs::UNDERLINE)
                        || cell.attrs.contains(CellAttrs::DOUBLE_UNDERLINE)
                        || cell.attrs.contains(CellAttrs::CURLY_UNDERLINE)
                        || cell.attrs.contains(CellAttrs::DOTTED_UNDERLINE)
                        || cell.attrs.contains(CellAttrs::DASHED_UNDERLINE),
                    strikethrough: cell.attrs.contains(CellAttrs::STRIKETHROUGH),
                }
            }

            fn is_default(&self) -> bool {
                matches!(self.fg, immorterm_core::cell::Color::Default)
                    && matches!(self.bg, immorterm_core::cell::Color::Default)
                    && !self.bold
                    && !self.dim
                    && !self.italic
                    && !self.underline
                    && !self.strikethrough
            }
        }

        /// Convert a Color enum to a CSS color string.
        fn color_to_css(color: &immorterm_core::cell::Color) -> Option<String> {
            match color {
                immorterm_core::cell::Color::Default => None,
                immorterm_core::cell::Color::Rgb(r, g, b) => {
                    Some(format!("#{:02x}{:02x}{:02x}", r, g, b))
                }
                immorterm_core::cell::Color::Indexed(idx) => {
                    let (r, g, b) = match *idx {
                        0 => (0x3B, 0x42, 0x52),
                        1 => (0xBF, 0x61, 0x6A),
                        2 => (0xA3, 0xBE, 0x8C),
                        3 => (0xEB, 0xCB, 0x8B),
                        4 => (0x81, 0xA1, 0xC1),
                        5 => (0xB4, 0x8E, 0xAD),
                        6 => (0x88, 0xC0, 0xD0),
                        7 => (0xE5, 0xE9, 0xF0),
                        8 => (0x4C, 0x56, 0x6A),
                        9 => (0xBF, 0x61, 0x6A),
                        10 => (0xA3, 0xBE, 0x8C),
                        11 => (0xEB, 0xCB, 0x8B),
                        12 => (0x81, 0xA1, 0xC1),
                        13 => (0xB4, 0x8E, 0xAD),
                        14 => (0x8F, 0xBC, 0xBB),
                        15 => (0xEC, 0xEF, 0xF4),
                        16..=231 => {
                            let i = idx - 16;
                            let b_val = i % 6;
                            let g_val = (i / 6) % 6;
                            let r_val = i / 36;
                            let to_byte = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
                            (to_byte(r_val), to_byte(g_val), to_byte(b_val))
                        }
                        232..=255 => {
                            let level = 8 + 10 * (idx - 232);
                            (level, level, level)
                        }
                    };
                    Some(format!("#{:02x}{:02x}{:02x}", r, g, b))
                }
            }
        }

        fn open_span(style: &Style) -> String {
            if style.is_default() {
                return String::new();
            }
            let mut css = String::new();
            if let Some(c) = color_to_css(&style.fg) {
                css.push_str(&format!("color:{};", c));
            }
            if let Some(c) = color_to_css(&style.bg) {
                css.push_str(&format!("background-color:{};", c));
            }
            if style.bold {
                css.push_str("font-weight:bold;");
            }
            if style.dim {
                css.push_str("opacity:0.6;");
            }
            if style.italic {
                css.push_str("font-style:italic;");
            }
            let mut decorations = Vec::new();
            if style.underline { decorations.push("underline"); }
            if style.strikethrough { decorations.push("line-through"); }
            if !decorations.is_empty() {
                css.push_str(&format!("text-decoration:{};", decorations.join(" ")));
            }
            if css.is_empty() {
                String::new()
            } else {
                format!("<span style=\"{}\">", css)
            }
        }

        fn html_escape(ch: char) -> String {
            match ch {
                '<' => "&lt;".to_string(),
                '>' => "&gt;".to_string(),
                '&' => "&amp;".to_string(),
                '"' => "&quot;".to_string(),
                _ => ch.to_string(),
            }
        }

        // Build HTML content. We track the current style and emit spans on style changes.
        let mut html = String::from(
            "<pre style=\"font-family:'JetBrains Mono','Fira Code','Cascadia Code',monospace;\
             font-size:13px;line-height:1.4;\">"
        );
        let mut current_style: Option<Style> = None;
        let mut span_open = false;

        // Helper: close current span if open
        let close_span = |html: &mut String, span_open: &mut bool| {
            if *span_open {
                html.push_str("</span>");
                *span_open = false;
            }
        };

        // Helper: emit a cell's char with style tracking
        let emit_cell = |html: &mut String, cell: &immorterm_core::cell::Cell, current_style: &mut Option<Style>, span_open: &mut bool| {
            let style = Style::from_cell(cell);
            let need_new_span = !matches!(current_style, Some(cs) if *cs == style);
            if need_new_span {
                close_span(html, span_open);
                let tag = open_span(&style);
                if !tag.is_empty() {
                    html.push_str(&tag);
                    *span_open = true;
                }
                *current_style = Some(style);
            }
            html.push_str(&html_escape(cell.grapheme));
        };

        if self.selection.block_mode {
            let min_col = self.selection.anchor.0.min(self.selection.active.0);
            let max_col = self.selection.anchor.0.max(self.selection.active.0);
            let min_row = self.selection.anchor.1.min(self.selection.active.1);
            let max_row = self.selection.anchor.1.max(self.selection.active.1);

            for content_idx in min_row..=max_row {
                let row = if content_idx < sb_len {
                    self.terminal.scrollback.get(content_idx)
                } else {
                    grid.row(content_idx - sb_len)
                };
                if let Some(row) = row {
                    let end_col = max_col.min(row.cells.len().saturating_sub(1));
                    for col in min_col..=end_col {
                        if let Some(cell) = row.cells.get(col)
                            && cell.width > 0
                        {
                            emit_cell(&mut html, cell, &mut current_style, &mut span_open);
                        }
                    }
                    if content_idx < max_row && !row.wrapped {
                        close_span(&mut html, &mut span_open);
                        current_style = None;
                        html.push('\n');
                    }
                }
            }
        } else {
            // Normal (line) selection — mirrors get_selected_text logic
            let ((sc, sr), (ec, er)) = self.selection.range();
            let mut skip_leading_spaces = false;

            for content_idx in sr..=er {
                let row = if content_idx < sb_len {
                    self.terminal.scrollback.get(content_idx)
                } else {
                    grid.row(content_idx - sb_len)
                };

                if let Some(row) = row {
                    let start_col = if content_idx == sr { sc } else { 0 };
                    let end_col = if content_idx == er {
                        ec.min(row.cells.len().saturating_sub(1))
                    } else {
                        row.cells.len().saturating_sub(1)
                    };

                    let mut row_started = false;
                    for col in start_col..=end_col {
                        if let Some(cell) = row.cells.get(col)
                            && cell.width > 0
                        {
                            if skip_leading_spaces && !row_started && cell.grapheme == ' ' {
                                continue;
                            }
                            row_started = true;
                            skip_leading_spaces = false;
                            emit_cell(&mut html, cell, &mut current_style, &mut span_open);
                        }
                    }
                    skip_leading_spaces = false;
                    if content_idx < er {
                        // Trim trailing spaces from the HTML (close span, trim, re-assess)
                        close_span(&mut html, &mut span_open);
                        current_style = None;
                        // Remove trailing space chars from the HTML buffer
                        let trimmed_len = html.trim_end_matches(' ').len();
                        html.truncate(trimmed_len);

                        let mut treat_as_soft_wrap = row_started && row.is_soft_wrapped();
                        if treat_as_soft_wrap {
                            let next_idx = content_idx + 1;
                            let next_row = if next_idx < sb_len {
                                self.terminal.scrollback.get(next_idx)
                            } else {
                                grid.row(next_idx - sb_len)
                            };
                            if has_hard_break_signals(row, next_row) {
                                treat_as_soft_wrap = false;
                            }
                        }

                        if treat_as_soft_wrap {
                            html.push(' ');
                            skip_leading_spaces = true;
                        } else {
                            html.push('\n');
                        }
                    }
                }
            }
        }

        close_span(&mut html, &mut span_open);
        html.push_str("</pre>");
        html
    }

    /// Debug: dump wrapped flags for the current selection's rows.
    /// Returns JSON like: [{"row":0,"wrapped":true,"len":80,"text":"ABCDEF..."}]
    pub fn debug_selection_wrapped(&self) -> String {
        if !self.selection.is_active {
            return "[]".to_string();
        }
        let sb_len = self.terminal.scrollback.len();
        let grid = &self.terminal.grid;
        let ((sc, sr), (ec, er)) = self.selection.range();

        let mut entries = Vec::new();
        for content_idx in sr..=er {
            let row = if content_idx < sb_len {
                self.terminal.scrollback.get(content_idx)
            } else {
                grid.row(content_idx - sb_len)
            };
            if let Some(row) = row {
                let start_col = if content_idx == sr { sc } else { 0 };
                let end_col = if content_idx == er {
                    ec.min(row.cells.len().saturating_sub(1))
                } else {
                    row.cells.len().saturating_sub(1)
                };
                let preview: String = row.cells[start_col..=end_col.min(row.cells.len() - 1)]
                    .iter()
                    .filter(|c| c.width > 0)
                    .map(|c| c.grapheme)
                    .collect::<String>();
                let preview_trimmed = preview.trim_end();
                let trimmed_len = preview_trimmed.len();
                entries.push(format!(
                    "{{\"idx\":{},\"wrapped\":{},\"soft_wrapped\":{},\"content_end_col\":{},\"cells\":{},\"chars\":{},\"preview\":\"{}\"}}",
                    content_idx,
                    row.wrapped,
                    row.soft_wrapped,
                    row.content_end_col,
                    row.cells.len(),
                    trimmed_len,
                    if trimmed_len > 30 {
                        format!("{}…{}", &preview_trimmed[..15], &preview_trimmed[trimmed_len - 15..])
                    } else {
                        preview_trimmed.to_string()
                    }
                ));
            }
        }
        format!("[{}]", entries.join(","))
    }

    // ── Multi-Cursor (Pseudo-Cursor) Mode ──


    /// Extract text from a single Selection range.
    fn extract_selection_text(&self, sel: &Selection) -> String {
        if !sel.is_active {
            return String::new();
        }
        let ((sc, sr), (ec, er)) = sel.range();
        let mut text = String::new();
        let sb_len = self.terminal.scrollback.len();
        let grid = &self.terminal.grid;
        let mut skip_leading_spaces = false;

        for content_idx in sr..=er {
            let row = if content_idx < sb_len {
                self.terminal.scrollback.get(content_idx)
            } else {
                grid.row(content_idx - sb_len)
            };
            if let Some(row) = row {
                let start_col = if content_idx == sr { sc } else { 0 };
                let end_col = if content_idx == er {
                    ec.min(row.cells.len().saturating_sub(1))
                } else {
                    row.cells.len().saturating_sub(1)
                };
                let mut row_started = false;
                for col in start_col..=end_col {
                    if let Some(cell) = row.cells.get(col)
                        && cell.width > 0
                    {
                        if skip_leading_spaces && !row_started && cell.grapheme == ' ' {
                            continue;
                        }
                        row_started = true;
                        skip_leading_spaces = false;
                        text.push(cell.grapheme);
                    }
                }
                skip_leading_spaces = false;
                if content_idx < er {
                    let trimmed = text.trim_end_matches(' ').len();
                    text.truncate(trimmed);

                    // Detect soft-wrap: either the terminal flagged it (regular terminals)
                    // or content fills ≥75% of the row (Ink/React pre-wrapped text).
                    if row_started && row.is_soft_wrapped() {
                        text.push(' ');
                        skip_leading_spaces = true;
                    } else {
                        text.push('\n');
                    }
                }
            }
        }
        text.trim_end().to_string()
    }

    /// Add a pseudo-cursor at the given CSS pixel coordinates (Alt+Click).
    pub fn pseudo_cursor_add(&mut self, css_x: f32, css_y: f32) {
        let (col, row) = self.css_to_cell(css_x, css_y);
        let content_row = self.display_to_content(row);
        self.pseudo_cursors.push(Selection {
            anchor: (col, content_row),
            active: (col, content_row),
            is_active: true,
            block_mode: false,
        });
    }

    /// Add a pseudo-cursor at a specific column and content row (for vertical multi-cursor).
    pub fn pseudo_cursor_add_at(&mut self, col: usize, content_row: usize) {
        self.pseudo_cursors.push(Selection {
            anchor: (col, content_row),
            active: (col, content_row),
            is_active: true,
            block_mode: false,
        });
    }

    /// Seed a pseudo-cursor at the visual cursor position (for Alt+Alt entry).
    /// Uses find_visual_cursor() or falls back to terminal cursor.
    pub fn pseudo_cursor_add_at_visual_cursor(&mut self) {
        let sb_len = self.terminal.scrollback.len();
        let (col, content_row) =
            if !self.terminal.cursor.visible || !self.terminal.modes.cursor_visible {
                if let Some((vc_col, vc_row)) = self.find_visual_cursor() {
                    (vc_col, sb_len + vc_row)
                } else {
                    (self.terminal.cursor.col, sb_len + self.terminal.cursor.row)
                }
            } else {
                (self.terminal.cursor.col, sb_len + self.terminal.cursor.row)
            };
        self.pseudo_cursors.push(Selection {
            anchor: (col, content_row),
            active: (col, content_row),
            is_active: true,
            block_mode: false,
        });
    }

    /// Add a vertical pseudo-cursor above or below the last pseudo-cursor.
    /// Used by Alt+Alt+Arrow: keeps the same column, moves one row up/down.
    pub fn pseudo_cursor_add_vertical(&mut self, direction: &str) {
        let sb_len = self.terminal.scrollback.len();
        let max_row = sb_len + self.rows.saturating_sub(1);

        // Base position: the last pseudo-cursor's anchor
        let (col, row) = if let Some(last) = self.pseudo_cursors.last() {
            last.anchor
        } else {
            return;
        };

        let new_row = match direction {
            "up" => row.saturating_sub(1),
            "down" => (row + 1).min(max_row),
            _ => return,
        };

        // Don't add duplicate at same position
        if self.pseudo_cursors.iter().any(|s| s.anchor == (col, new_row)) {
            return;
        }

        self.pseudo_cursors.push(Selection {
            anchor: (col, new_row),
            active: (col, new_row),
            is_active: true,
            block_mode: false,
        });
    }

    /// Extend all pseudo-cursor selections in the given direction.
    /// Same directions as selection_extend: left, right, up, down, word_left, word_right, home, end.
    pub fn pseudo_cursor_extend_all(&mut self, direction: &str) {
        let sb_len = self.terminal.scrollback.len();
        let max_col = self.cols.saturating_sub(1);
        let max_row = sb_len + self.rows.saturating_sub(1);

        // For word movement, collect active positions first, then compute new positions
        // to avoid borrowing self.pseudo_cursors and self simultaneously
        if direction == "word_left" || direction == "word_right" {
            let actives: Vec<(usize, (usize, usize), bool)> = self
                .pseudo_cursors
                .iter()
                .enumerate()
                .map(|(i, sel)| (i, sel.active, sel.is_active))
                .collect();
            for (i, (col, row), is_active) in actives {
                if !is_active {
                    continue;
                }
                self.pseudo_cursors[i].active = if direction == "word_left" {
                    self.move_selection_word_left(col, row, max_col)
                } else {
                    self.move_selection_word_right(col, row, max_col, max_row)
                };
            }
            return;
        }

        for sel in &mut self.pseudo_cursors {
            if !sel.is_active {
                continue;
            }
            let (col, row) = sel.active;
            sel.active = match direction {
                "left" => {
                    if col > 0 {
                        (col - 1, row)
                    } else if row > 0 {
                        (max_col, row - 1)
                    } else {
                        (0, 0)
                    }
                }
                "right" => {
                    if col < max_col {
                        (col + 1, row)
                    } else if row < max_row {
                        (0, row + 1)
                    } else {
                        (max_col, max_row)
                    }
                }
                "up" => (col, row.saturating_sub(1)),
                "down" => (col, (row + 1).min(max_row)),
                "home" => (0, row),
                "end" => (max_col, row),
                _ => (col, row),
            };
        }
    }

    /// Clear all pseudo-cursors (exit multi-cursor mode).
    pub fn pseudo_cursor_clear(&mut self) {
        self.pseudo_cursors.clear();
    }

    /// Replace pseudo-cursor list with N range pseudo-selections, one per
    /// `[start_row, start_col, end_row, end_col]` quad in `flat`. Used by
    /// the Cmd+E auto-comment flow to visualize the bullet titles detected
    /// by `pub_detect_claude_bullets`. Existing pseudo-cursors are dropped.
    /// `flat.len()` must be a multiple of 4 — invalid input is ignored.
    pub fn pub_pseudo_select_ranges(&mut self, flat: Vec<u32>) {
        if !flat.len().is_multiple_of(4) {
            return;
        }
        self.pseudo_cursors.clear();
        let mut i = 0usize;
        while i + 3 < flat.len() {
            let sr = flat[i] as usize;
            let sc = flat[i + 1] as usize;
            let er = flat[i + 2] as usize;
            let ec = flat[i + 3] as usize;
            self.pseudo_cursors.push(Selection {
                anchor: (sc, sr),
                active: (ec, er),
                is_active: true,
                block_mode: false,
            });
            i += 4;
        }
    }

    /// Check if multi-cursor mode is active (any pseudo-cursors placed).
    pub fn has_pseudo_cursors(&self) -> bool {
        !self.pseudo_cursors.is_empty()
    }

    /// Get number of pseudo-cursors.
    pub fn pseudo_cursor_count(&self) -> usize {
        self.pseudo_cursors.len()
    }

    /// Get text from all pseudo-cursor selections, joined by newlines.
    /// Each pseudo-cursor's selected text becomes one entry.
    pub fn get_pseudo_cursor_text(&self) -> String {
        // Sort pseudo-cursors by position (top-to-bottom, left-to-right)
        let mut sorted: Vec<&Selection> = self
            .pseudo_cursors
            .iter()
            .filter(|s| s.is_active)
            .collect();
        sorted.sort_by_key(|s| {
            let ((_, sr), _) = s.range();
            let ((sc, _), _) = s.range();
            (sr, sc)
        });

        sorted
            .iter()
            .map(|s| self.extract_selection_text(s))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Rebuild the renderer with a new DPR (changes font size).
    /// Called when VS Code zoom level changes (Cmd++/Cmd+-).
    /// Recreates the glyph atlas at the new font size without touching GPU device or terminal state.
    pub fn reinit_renderer(&mut self, new_dpr: f32) -> Result<Vec<usize>, JsValue> {
        self.dpr = new_dpr.max(1.0);
        let gpu = self.gpu.as_ref().ok_or("no GPU state")?;

        let font_size = self.font_size_css * self.dpr;
        let lh = self.line_height_ratio;
        let mut renderer = if self.custom_font_data.is_empty() {
            let fonts: &[&[u8]] = &[FONT_REGULAR, FONT_ITALIC, FONT_BOLD, FONT_BOLD_ITALIC, FONT_SYMBOLS, FONT_NERD, FONT_MISC, FONT_HEBREW, FONT_HEBREW_BOLD];
            TerminalRenderer::with_line_height(&gpu.device, &gpu.queue, gpu.surface_config.format, Some(fonts), font_size, lh)
        } else {
            let mut font_refs: Vec<&[u8]> = self.custom_font_data.iter().map(|v| v.as_slice()).collect();
            font_refs.push(FONT_SYMBOLS);
            font_refs.push(FONT_NERD);
            font_refs.push(FONT_MISC);

            let mut r = TerminalRenderer::with_line_height(&gpu.device, &gpu.queue, gpu.surface_config.format, Some(&font_refs), font_size, lh);
            if let Some(ref name) = self.custom_font_name {
                r.atlas.set_custom_family(name.clone());
            }
            r
        };
        // Preserve status bar mode across reinit (hidden = row freed, others = row reserved)
        renderer.status_bar_enabled = self.status_bar_mode != "hidden";
        renderer.status_bar_reveal = if self.status_bar_mode == "always" { 1.0 } else { 0.0 };
        // Preserve VS Code theme colors across reinit
        if let Some(theme) = self.pending_theme.clone() {
            renderer.theme = theme;
        }
        if let Some(weight) = self.pending_font_weight {
            renderer.atlas.set_base_weight(weight);
        }
        // Preserve content padding across reinit
        let [pt, pr, pb, pl] = self.pending_content_padding;
        renderer.set_content_padding(pt, pr, pb, pl);
        // Preserve visual preferences across reinit
        renderer.border_enabled = self.pending_border_enabled;
        renderer.border_opacity = self.pending_border_opacity;
        renderer.animations_enabled = self.pending_animations_enabled;
        renderer.expression_effects = self.pending_expression_effects;
        renderer.celebrations_enabled = self.pending_celebrations;
        renderer.danger_effects = self.pending_danger_effects;
        renderer.text_animations = self.pending_text_animations;

        // Re-create Canvas 2D fallback rasterizer with new font metrics
        {
            let cell_w = renderer.atlas.metrics.cell_width;
            let cell_h = renderer.atlas.metrics.cell_height;
            let baseline = renderer.atlas.metrics.baseline_y;
            let fs = self.font_size_css * self.dpr;

            let rasterizer = Box::new(move |ch: char, _font_size: f32| -> Option<FallbackGlyph> {
                use web_sys::{OffscreenCanvas, OffscreenCanvasRenderingContext2d};

                let w = (cell_w * 2.0).ceil() as u32;
                let h = cell_h.ceil() as u32;
                if w == 0 || h == 0 {
                    return None;
                }

                let canvas = OffscreenCanvas::new(w, h).ok()?;
                let ctx: OffscreenCanvasRenderingContext2d = canvas
                    .get_context("2d")
                    .ok()??
                    .dyn_into()
                    .ok()?;

                let font_str = format!(
                    "{}px 'Apple Color Emoji','Segoe UI Emoji','Noto Color Emoji',sans-serif",
                    fs
                );
                ctx.set_font(&font_str);
                ctx.set_text_baseline("alphabetic");
                ctx.fill_text(&ch.to_string(), 0.0, baseline as f64).ok()?;

                let img = ctx.get_image_data(0.0, 0.0, w as f64, h as f64).ok()?;
                let data = img.data().to_vec();

                if data.iter().skip(3).step_by(4).all(|&a| a == 0) {
                    return None;
                }

                Some(FallbackGlyph {
                    data,
                    width: w,
                    height: h,
                    bearing_x: 0.0,
                    bearing_y: 0.0,
                })
            });
            renderer.atlas.set_fallback_rasterizer(Some(rasterizer));
        }

        // Override cell metrics with Canvas 2D measurements (same as init_gpu)
        {
            use web_sys::{OffscreenCanvas, OffscreenCanvasRenderingContext2d};
            let fs = self.font_size_css * self.dpr;
            let font_name = self.custom_font_name.clone().unwrap_or_else(|| "'JetBrains Mono', 'Heebo', monospace".to_string());
            let font_str = format!("{}px {}", fs, font_name);

            let measure_canvas = OffscreenCanvas::new(100, 100).unwrap();
            let measure_ctx: OffscreenCanvasRenderingContext2d = measure_canvas
                .get_context("2d").unwrap().unwrap().dyn_into().unwrap();

            // Measure at CSS pixels for cell height (use DOM char height if available)
            let css_font_str = format!("{}px {}", self.font_size_css, font_name);
            measure_ctx.set_font(&css_font_str);
            let css_tm = measure_ctx.measure_text("M").unwrap();
            let font_bbox_css = css_tm.font_bounding_box_ascent() + css_tm.font_bounding_box_descent();
            let char_height = if self.char_height_css > 0.0 {
                self.char_height_css as f64
            } else {
                font_bbox_css
            };

            // Measure at device pixels for glyph metrics
            measure_ctx.set_font(&font_str);
            let tm = measure_ctx.measure_text("M").unwrap();
            let cell_width = tm.width() as f32;
            let font_ascent = tm.actual_bounding_box_ascent() as f32;

            let user_line_height = self.line_height_ratio / 1.15;
            let scaled_char_height = (char_height as f32 * self.dpr).ceil();
            let cell_height = (scaled_char_height * user_line_height).floor();
            let font_em_height = fs;
            let center_pad = (cell_height - font_em_height).max(0.0) / 2.0;
            let baseline_y = (font_ascent + center_pad + 1.0).round();

            renderer.atlas.set_metrics(immorterm_render::CellMetrics {
                cell_width,
                cell_height,
                baseline_y,
            });
        }

        // Re-create Canvas 2D primary rasterizer with new font metrics
        {
            let cell_w = renderer.atlas.metrics.cell_width;
            let cell_h = renderer.atlas.metrics.cell_height;
            let baseline = renderer.atlas.metrics.baseline_y;
            let fs = self.font_size_css * self.dpr;
            let font_name = self.custom_font_name.clone().unwrap_or_else(|| "'JetBrains Mono', 'Heebo', monospace".to_string());

            let rasterizer = Box::new(move |ch: char, _font_size: f32, bold: bool, italic: bool| -> Option<MonoGlyph> {
                use web_sys::{OffscreenCanvas, OffscreenCanvasRenderingContext2d};

                let w = cell_w.ceil() as u32;
                let h = cell_h.ceil() as u32;
                if w == 0 || h == 0 {
                    return None;
                }

                let canvas = OffscreenCanvas::new(w, h).ok()?;
                let ctx: OffscreenCanvasRenderingContext2d = canvas
                    .get_context("2d")
                    .ok()??
                    .dyn_into()
                    .ok()?;

                ctx.set_fill_style_str("black");
                ctx.fill_rect(0.0, 0.0, w as f64, h as f64);

                let weight = if bold { "bold" } else { "normal" };
                let style = if italic { "italic" } else { "normal" };
                let font_str = format!("{} {} {}px {}", style, weight, fs, font_name);
                ctx.set_font(&font_str);
                ctx.set_fill_style_str("white");
                ctx.set_text_baseline("alphabetic");

                let s = ch.to_string();
                let tm = ctx.measure_text(&s).ok()?;
                let char_width = tm.width();

                if char_width <= cell_w as f64 * 1.02 {
                    let x_offset = ((cell_w as f64 - char_width) / 2.0).max(0.0);
                    ctx.fill_text(&s, x_offset, baseline as f64).ok()?;
                } else {
                    let scale_x = cell_w as f64 / char_width;
                    ctx.save();
                    let _ = ctx.translate(cell_w as f64 / 2.0, 0.0);
                    let _ = ctx.scale(scale_x, 1.0);
                    ctx.fill_text(&s, -char_width / 2.0, baseline as f64).ok()?;
                    ctx.restore();
                }

                let img = ctx.get_image_data(0.0, 0.0, w as f64, h as f64).ok()?;
                let rgba = img.data().to_vec();

                let mut min_x = w;
                let mut min_y = h;
                let mut max_x = 0u32;
                let mut max_y = 0u32;
                for y in 0..h {
                    for x in 0..w {
                        let r = rgba[((y * w + x) * 4) as usize];
                        if r > 0 {
                            min_x = min_x.min(x);
                            min_y = min_y.min(y);
                            max_x = max_x.max(x);
                            max_y = max_y.max(y);
                        }
                    }
                }
                if max_x < min_x {
                    return None;
                }

                let tw = max_x - min_x + 1;
                let th = max_y - min_y + 1;
                let mut alpha = Vec::with_capacity((tw * th) as usize);
                for y in min_y..=max_y {
                    for x in min_x..=max_x {
                        alpha.push(rgba[((y * w + x) * 4) as usize]);
                    }
                }

                Some(MonoGlyph {
                    data: alpha,
                    width: tw,
                    height: th,
                    offset_x: min_x as f32,
                    offset_y: min_y as f32,
                })
            });
            renderer.atlas.set_primary_rasterizer(Some(rasterizer));
        }

        // Re-create run rasterizer for multi-character RTL text runs
        {
            let cell_w = renderer.atlas.metrics.cell_width;
            let cell_h = renderer.atlas.metrics.cell_height;
            let baseline = renderer.atlas.metrics.baseline_y;
            let fs = self.font_size_css * self.dpr;
            let run_font_name = self.custom_font_name.clone().unwrap_or_else(|| "'JetBrains Mono', 'Heebo', monospace".to_string());

            let run_rasterizer = Box::new(move |text: &str, _font_size: f32, bold: bool, italic: bool, num_cells: u32| -> Option<MonoGlyph> {
                use web_sys::{OffscreenCanvas, OffscreenCanvasRenderingContext2d};

                let total_w = (cell_w * num_cells as f32).ceil() as u32;
                let h = cell_h.ceil() as u32;
                if total_w == 0 || h == 0 {
                    return None;
                }

                let canvas = OffscreenCanvas::new(total_w, h).ok()?;
                let ctx: OffscreenCanvasRenderingContext2d = canvas
                    .get_context("2d")
                    .ok()??
                    .dyn_into()
                    .ok()?;

                ctx.set_fill_style_str("black");
                ctx.fill_rect(0.0, 0.0, total_w as f64, h as f64);

                let weight = if bold { "bold" } else { "normal" };
                let style = if italic { "italic" } else { "normal" };
                let font_str =
                    format!("{} {} {}px {}", style, weight, fs, run_font_stack(text, &run_font_name));
                ctx.set_font(&font_str);
                ctx.set_fill_style_str("white");
                ctx.set_text_baseline("alphabetic");

                let tm = ctx.measure_text(text).ok()?;
                let text_width = tm.width();
                let x_offset = ((total_w as f64 - text_width) / 2.0).max(0.0);
                ctx.fill_text(text, x_offset, baseline as f64).ok()?;

                let img = ctx.get_image_data(0.0, 0.0, total_w as f64, h as f64).ok()?;
                let rgba = img.data().to_vec();

                let mut min_x = total_w;
                let mut min_y = h;
                let mut max_x = 0u32;
                let mut max_y = 0u32;
                for y in 0..h {
                    for x in 0..total_w {
                        let r = rgba[((y * total_w + x) * 4) as usize];
                        if r > 0 {
                            min_x = min_x.min(x);
                            min_y = min_y.min(y);
                            max_x = max_x.max(x);
                            max_y = max_y.max(y);
                        }
                    }
                }
                if max_x < min_x {
                    return None;
                }

                let tw = max_x - min_x + 1;
                let th = max_y - min_y + 1;
                let mut alpha = Vec::with_capacity((tw * th) as usize);
                for y in min_y..=max_y {
                    for x in min_x..=max_x {
                        alpha.push(rgba[((y * total_w + x) * 4) as usize]);
                    }
                }

                Some(MonoGlyph {
                    data: alpha,
                    width: tw,
                    height: th,
                    offset_x: min_x as f32,
                    offset_y: min_y as f32,
                })
            });
            renderer.atlas.set_run_rasterizer(Some(run_rasterizer));
        }

        let (cols, rows) = renderer.resize(&gpu.device, self.width, self.height);
        self.cols = cols;
        self.rows = rows;
        self.terminal.resize(cols, rows);
        self.renderer = Some(renderer);
        Ok(vec![cols, rows])
    }

    /// Handle resize. Returns new terminal dimensions as [cols, rows].
    pub fn resize(&mut self, width: u32, height: u32) -> Vec<usize> {
        self.width = width;
        self.height = height;

        if let (Some(gpu), Some(renderer)) = (self.gpu.as_mut(), self.renderer.as_mut()) {
            gpu.surface_config.width = width.max(1);
            gpu.surface_config.height = height.max(1);
            gpu.surface.configure(&gpu.device, &gpu.surface_config);

            let (cols, rows) = renderer.resize(&gpu.device, width, height);
            let old_cols = self.cols;
            let sb_before = self.terminal.scrollback.len();
            let grid_before = self.terminal.grid.row_count();
            let chars_before = self.count_content_chars();

            self.cols = cols;
            self.rows = rows;
            self.terminal.resize(cols, rows);

            let sb_after = self.terminal.scrollback.len();
            let grid_after = self.terminal.grid.row_count();
            let chars_after = self.count_content_chars();

            if old_cols != cols {
                self.title_marquee_overflow = 0; // recalculate for new width
                web_sys::console::log_1(&format!(
                    "[wasm-reflow] cols {}→{}, sb {}→{}, grid {}→{}, total {}→{}, chars {}→{} ({})",
                    old_cols, cols,
                    sb_before, sb_after,
                    grid_before, grid_after,
                    sb_before + grid_before, sb_after + grid_after,
                    chars_before, chars_after,
                    if chars_after == chars_before { "OK" }
                    else if chars_after < chars_before { "LOST" }
                    else { "GAINED" }
                ).into());
            }
        }

        vec![self.cols, self.rows]
    }

    /// Get current terminal dimensions.
    pub fn dimensions(&self) -> Vec<usize> {
        vec![self.cols, self.rows]
    }

    /// Get scroll offset.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Count non-default cells across scrollback + grid (content integrity metric).
    fn count_content_chars(&self) -> usize {
        let default = immorterm_core::cell::Cell::default();
        let sb: usize = self.terminal.scrollback.iter()
            .map(|row| row.cells.iter().filter(|c| **c != default).count())
            .sum();
        let grid: usize = (0..self.terminal.grid.row_count())
            .filter_map(|i| self.terminal.grid.row(i))
            .map(|row| row.cells.iter().filter(|c| **c != default).count())
            .sum();
        sb + grid
    }

    /// Drain the pending bell flag (BEL 0x07).
    /// Returns true if a bell was received since the last call.
    /// Called by the frontend after `process()` to show sidebar 🔔 badge.
    pub fn take_bell(&mut self) -> bool {
        self.terminal.take_bell()
    }

    /// Current scrollback buffer length (for scroll-anchored AI primitives).
    pub fn scrollback_len(&self) -> usize {
        self.terminal.scrollback.len()
    }

    /// Hard-reset the local scrollback buffer. Used by the manual escape hatch
    /// (Cmd+Shift+Alt+R) when WASM-local scrollback accumulated duplicates that
    /// the daemon can't authoritatively replace via load_snapshot (e.g. the
    /// preserve-when-empty path during alt-screen). Caller is responsible for
    /// re-fetching from the daemon afterwards if it wants fresh content.
    pub fn clear_scrollback(&mut self) {
        self.terminal.scrollback.clear();
        self.scroll_offset = 0;
        self.scroll_deficit = 0;
    }

    /// Scroll by delta rows (positive = up, negative = down).
    /// Returns `true` if the client needs more scrollback rows from the daemon
    /// (i.e., user scrolled past the locally available buffer).
    pub fn scroll(&mut self, delta: i32) -> bool {
        let local_max = self.terminal.scrollback.len();
        if delta > 0 {
            let desired = self.scroll_offset + delta as usize;
            if desired > local_max {
                self.scroll_deficit += desired - local_max;
                self.scroll_offset = local_max; // show what we have
                return true; // signal: need more rows from daemon
            }
            self.scroll_offset = desired;
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub((-delta) as usize);
        }
        self.scroll_deficit = 0; // successful scroll clears deficit
        false
    }

    /// Set scroll offset to an absolute value (clamped to scrollback length).
    pub fn set_scroll_offset(&mut self, offset: usize) {
        self.scroll_offset = offset.min(self.terminal.scrollback.len());
        self.scroll_deficit = 0;
    }

    /// Reset scroll to live view (bottom).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.scroll_deficit = 0;
    }

    /// Prepend scrollback rows received from the daemon (on-demand fetch).
    /// Rows are inserted at the front (oldest end). The renderer formula
    /// `content_idx = (sb_len + display_row) - scroll_offset` self-adjusts
    /// as sb_len grows, so scroll_offset does NOT need stabilization.
    /// Any pending scroll deficit (user scrolled past local buffer) is applied.
    pub fn prepend_scrollback(&mut self, json: &str) -> Result<(), JsValue> {
        let rows: Vec<immorterm_core::grid::Row> = serde_json::from_str(json)
            .map_err(|e| JsValue::from_str(&format!("scrollback parse: {}", e)))?;
        // Reflow rows if they arrive at a different width than the current terminal
        let rows = immorterm_core::terminal::reflow_scrollback_rows(rows, self.terminal.cols());
        for row in rows.into_iter().rev() {
            self.terminal.scrollback.push_front(row);
        }
        // Apply any pending deficit from scroll() that was clamped
        if self.scroll_deficit > 0 {
            let max = self.terminal.scrollback.len();
            let target = self.scroll_offset + self.scroll_deficit;
            self.scroll_offset = target.min(max);
            self.scroll_deficit = 0;
        }
        Ok(())
    }

    /// Set mouse proximity to scroll indicator (0.0 = far, 1.0 = on it).
    pub fn set_scroll_indicator_proximity(&mut self, proximity: f32) {
        if let Some(r) = self.renderer.as_mut() {
            r.set_scroll_indicator_proximity(proximity);
        }
    }

    // ── Status Bar Hover ──

    /// Hit-test the status bar at a given column.
    /// Returns "brand", "ai_stats", "theme", "title", or "none".
    /// Uses cached status bar data from the last render frame.
    pub fn status_bar_hit_test(&self, col: usize) -> String {
        if let Some(ref data) = self.last_status_bar {
            match statusbar::hit_test(data, col) {
                StatusBarTarget::Brand => "brand".into(),
                StatusBarTarget::AiStats => "ai_stats".into(),
                StatusBarTarget::ThemeArea => "theme".into(),
                StatusBarTarget::Scratch => "scratch".into(),
                StatusBarTarget::Title => "title".into(),
                StatusBarTarget::Project => "project".into(),
                StatusBarTarget::None => "none".into(),
            }
        } else {
            "none".into()
        }
    }

    /// Set which status bar target is currently hovered.
    /// Pass "brand", "ai_stats", "theme", "title", or "none".
    /// This affects the visual highlight on the next render frame.
    pub fn set_status_bar_hover(&mut self, target: &str) {
        self.status_bar_hover = match target {
            "brand" => StatusBarTarget::Brand,
            "ai_stats" => StatusBarTarget::AiStats,
            "theme" => StatusBarTarget::ThemeArea,
            "scratch" => StatusBarTarget::Scratch,
            "title" => StatusBarTarget::Title,
            _ => StatusBarTarget::None,
        };
    }

    /// Set the font size in CSS pixels (e.g. from VS Code terminal.integrated.fontSize).
    /// Must be called before init_gpu() for the initial size. After init, use reinit_renderer().
    pub fn set_font_size(&mut self, size: f32) {
        if size > 0.0 {
            self.font_size_css = size;
        }
    }

    /// Set the line height ratio from VS Code's `terminal.integrated.lineHeight`.
    /// The actual ratio used is `1.15 × value` (base 1.15 matches font ascent+descent metrics).
    /// VS Code default is 1 (meaning our base 1.15 is used as-is).
    /// Must be called before init_gpu().
    pub fn set_line_height(&mut self, value: f32) {
        if value > 0.0 {
            self.line_height_ratio = 1.15 * value;
        }
    }

    /// Set the DOM-measured character height in CSS pixels.
    /// This must match what xterm.js uses: a hidden `<span>` with `lineHeight: normal`.
    /// When set (> 0), overrides the Canvas measureText fontBoundingBox for cell height.
    /// Call before init_gpu() or before reinit_renderer() when the font changes.
    pub fn set_char_height_css(&mut self, value: f32) {
        if value > 0.0 {
            self.char_height_css = value;
        }
    }

    /// Set the base font weight for non-bold text (from VS Code `terminal.integrated.fontWeight`).
    /// Value is a CSS font-weight number: 100-900. Default is 400 (normal).
    /// Can be called before or after init_gpu() — after init, clears the glyph cache.
    pub fn set_font_weight(&mut self, weight: u16) {
        if (100..=900).contains(&weight) {
            if let Some(ref mut renderer) = self.renderer {
                renderer.atlas.set_base_weight(weight);
            }
            self.pending_font_weight = Some(weight);
        }
    }

    /// Set content padding in physical pixels (top, right, bottom, left).
    /// Insets terminal content while the window border remains at canvas edges.
    /// Can be called before or after init_gpu(). Values are in device pixels
    /// (multiply CSS pixels by devicePixelRatio before calling).
    pub fn set_content_padding(&mut self, top: f32, right: f32, bottom: f32, left: f32) {
        self.pending_content_padding = [top, right, bottom, left];
        if let Some(ref mut renderer) = self.renderer {
            renderer.set_content_padding(top, right, bottom, left);
        }
    }

    /// Set custom font data (TTF/OTF/TTC bytes) to use instead of embedded JetBrains Mono.
    /// Call this before init_gpu(). The symbols font is always appended automatically.
    /// For TTC files (font collections), a single call with the .ttc file is enough.
    pub fn set_custom_font(&mut self, data: &[u8]) {
        if !data.is_empty() {
            web_sys::console::log_1(&format!(
                "[WASM] set_custom_font: received {} bytes",
                data.len()
            ).into());
            self.custom_font_data = vec![data.to_vec()];
        } else {
            web_sys::console::log_1(&"[WASM] set_custom_font: received EMPTY data".into());
        }
    }

    /// Set the font family name explicitly (e.g. "Menlo").
    /// Call this after set_custom_font() and before init_gpu().
    pub fn set_custom_font_name(&mut self, name: &str) {
        let font_chain = format!("'{}', 'Heebo', monospace", name);
        web_sys::console::log_1(&format!("[WASM] set_custom_font_name: '{}' → chain: '{}'", name, font_chain).into());
        self.custom_font_name = Some(font_chain);
    }

    /// Set terminal colors from VS Code theme (CSS variable values).
    /// Each color is passed as r, g, b floats in 0.0–1.0 range.
    /// Call before init_gpu() to set initial colors, or after to update live.
    #[allow(clippy::too_many_arguments)] // WASM FFI boundary — can't use structs
    pub fn set_terminal_colors(
        &mut self,
        bg_r: f32, bg_g: f32, bg_b: f32,
        fg_r: f32, fg_g: f32, fg_b: f32,
        cursor_r: f32, cursor_g: f32, cursor_b: f32,
    ) {
        let theme = self.pending_theme.get_or_insert_with(Theme::default);
        theme.bg = [bg_r, bg_g, bg_b, 1.0];
        theme.fg = [fg_r, fg_g, fg_b, 1.0];
        theme.cursor = [cursor_r, cursor_g, cursor_b, 1.0];
        if let Some(ref mut renderer) = self.renderer {
            renderer.theme = theme.clone();
        }
    }

    /// Set the selection highlight color (with alpha for semi-transparency).
    pub fn set_selection_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        let theme = self.pending_theme.get_or_insert_with(Theme::default);
        theme.selection = [r, g, b, a];
        if let Some(ref mut renderer) = self.renderer {
            renderer.theme.selection = [r, g, b, a];
        }
    }

    /// Set the 16 ANSI color overrides from VS Code theme.
    /// `colors` must be a flat array of 48 floats (16 colors × 3 channels: RGB).
    /// Order: black, red, green, yellow, blue, magenta, cyan, white,
    ///        bright-black, bright-red, ..., bright-white.
    pub fn set_ansi_colors(&mut self, colors: &[f32]) {
        if colors.len() < 48 {
            return;
        }
        let mut ansi = [[0.0f32; 4]; 16];
        for i in 0..16 {
            ansi[i] = [colors[i * 3], colors[i * 3 + 1], colors[i * 3 + 2], 1.0];
        }
        let theme = self.pending_theme.get_or_insert_with(Theme::default);
        theme.ansi_overrides = Some(ansi);
        if let Some(ref mut renderer) = self.renderer {
            renderer.theme.ansi_overrides = Some(ansi);
        }
    }

    /// Set text alignment for BiDi rendering: "left", "right", "center", "auto".
    pub fn set_text_alignment(&mut self, alignment: &str) {
        let val = match alignment {
            "left" => immorterm_render::TextAlignment::Left,
            "right" => immorterm_render::TextAlignment::Right,
            "center" => immorterm_render::TextAlignment::Center,
            "auto" => immorterm_render::TextAlignment::Auto,
            _ => return,
        };
        if let Some(ref mut renderer) = self.renderer {
            renderer.text_alignment = val;
        }
    }

    /// Set paragraph direction for BiDi rendering: "ltr", "rtl", "auto".
    pub fn set_paragraph_direction(&mut self, direction: &str) {
        let val = match direction {
            "ltr" => immorterm_render::ParagraphDirection::Ltr,
            "rtl" => immorterm_render::ParagraphDirection::Rtl,
            "auto" => immorterm_render::ParagraphDirection::Auto,
            _ => return,
        };
        if let Some(ref mut renderer) = self.renderer {
            renderer.paragraph_direction = val;
        }
    }

    /// Get current text alignment as a string.
    pub fn text_alignment(&self) -> String {
        let val = self.renderer.as_ref()
            .map(|r| r.text_alignment)
            .unwrap_or_default();
        match val {
            immorterm_render::TextAlignment::Left => "left",
            immorterm_render::TextAlignment::Right => "right",
            immorterm_render::TextAlignment::Center => "center",
            immorterm_render::TextAlignment::Auto => "auto",
        }.to_string()
    }

    /// Get current paragraph direction as a string.
    pub fn paragraph_direction(&self) -> String {
        let val = self.renderer.as_ref()
            .map(|r| r.paragraph_direction)
            .unwrap_or_default();
        match val {
            immorterm_render::ParagraphDirection::Ltr => "ltr",
            immorterm_render::ParagraphDirection::Rtl => "rtl",
            immorterm_render::ParagraphDirection::Auto => "auto",
        }.to_string()
    }

    /// Set the project name shown in the status bar (left side).
    pub fn set_project_name(&mut self, name: &str) {
        self.project_name = name.to_string();
    }

    /// Get the current terminal title (set by OSC 0/2 sequences from programs).
    /// The webview can poll this after processing PTY bytes to detect title changes.
    pub fn title(&self) -> String {
        self.terminal.title.clone()
    }

    /// Current working directory emitted by the shell via OSC 7. Empty
    /// until the first prompt after shell-init.zsh runs. JS polls this
    /// from gpu-terminal.html to drive the plain→project upgrade banner
    /// when the user cd's into a trusted project.
    pub fn cwd(&self) -> String {
        self.terminal.cwd.clone()
    }

    /// Set the session title shown in the status bar (after project / separator).
    /// Resets the marquee scroll timer so the new title starts from the beginning.
    pub fn set_session_title(&mut self, title: &str) {
        if self.session_title != title {
            self.title_marquee_time = 0.0;
            self.title_marquee_overflow = 0; // recalculate on next frame
        }
        self.session_title = title.to_string();
    }

    /// Set the immorterm_id for the currently active session.
    /// Used by `load_snapshot` to detect session changes.
    pub fn set_immorterm_id(&mut self, id: &str) {
        self.immorterm_id = id.to_string();
    }

    /// Set AI stats string for the status bar (e.g. "Opus 4 · $1.23 · 45% ctx").
    /// Called from JS when the daemon reports Claude Code stats.
    pub fn set_ai_stats(&mut self, stats: &str) {
        self.ai_stats = stats.to_string();
    }

    /// Set CTX usage percentage for the status bar progress bar.
    /// 0.0 = no bar, 1–100 = colored fill behind center section.
    pub fn set_ai_ctx_pct(&mut self, pct: f32) {
        self.ai_ctx_pct = pct;
    }

    /// Replace the AI layer primitives with a fresh set from the daemon.
    ///
    /// The daemon broadcasts the full primitives list (with already-animated values)
    /// via WebSocket. The JS calls this to push the state into the WASM terminal so
    /// rects, lines, and small text render on the GPU canvas — not just HTML overlays.
    ///
    /// `daemon_sb_len` is the daemon's scrollback length at broadcast time, used to
    /// remap `scrollback_at_creation` for scroll-anchored primitives (the daemon and
    /// WASM terminals may have different scrollback lengths due to different dimensions).
    pub fn update_ai_primitives(&mut self, json: &str, daemon_sb_len: usize) {
        let mut primitives: Vec<immorterm_core::ai_layer::AiPrimitive> =
            match serde_json::from_str(json) {
                Ok(p) => p,
                Err(_) => return,
            };

        let wasm_sb_len = self.terminal.scrollback.len();

        // Remap scroll-anchored primitives from daemon scrollback space to WASM space.
        // daemon: lines_since_creation = daemon_sb_len - scrollback_at_creation
        // wasm:   scrollback_at_creation = wasm_sb_len - lines_since_creation
        for prim in &mut primitives {
            if let immorterm_core::ai_layer::AnchorMode::Scroll {
                scrollback_at_creation,
            } = &mut prim.anchor
            {
                let lines_since = daemon_sb_len.saturating_sub(*scrollback_at_creation);
                *scrollback_at_creation = wasm_sb_len.saturating_sub(lines_since);
            }
        }

        self.terminal.ai_layer.primitives = primitives;
        // Clear stale WASM-side animations — the daemon ticks animations and sends
        // already-interpolated property values, so local animations are unnecessary.
        self.terminal.ai_layer.animations.clear();
        self.terminal.ai_layer.dirty = true;
    }

    /// Return the active terminal's AI layer primitives as a JSON array string.
    /// Used by JS to re-sync DOM overlays after a session restore (tab switch).
    pub fn get_ai_primitives_json(&self) -> String {
        serde_json::to_string(&self.terminal.ai_layer.primitives).unwrap_or_else(|_| "[]".into())
    }

    /// Set the status bar theme by name.
    /// Valid names: "Midnight Purple", "Ocean Blue", "Forest Green", "Sunset Red", "Monochrome".
    pub fn set_theme(&mut self, name: &str) {
        if let Some(theme) = THEME_PRESETS.iter().find(|t| t.name == name) {
            self.status_bar_theme = *theme;
            // Derive pseudo-cursor selection color from theme accent.
            // Darken the accent to ~40% brightness so it's visible as a background
            // behind white text, then use moderate alpha for translucency.
            let a = theme.fg_accent;
            let pseudo_sel = [a[0] * 0.4, a[1] * 0.4, a[2] * 0.4, 0.65];
            let cursor = [a[0], a[1], a[2], 1.0];
            let t = self.pending_theme.get_or_insert_with(Theme::default);
            t.pseudo_selection = pseudo_sel;
            // Cursor follows the theme's brand accent (was a fixed Nord orange
            // #D08770). On standalone/Tauri there's no VS Code
            // set_terminal_colors() call to theme it, so without this the cursor
            // stayed orange regardless of the selected immorterm-ai theme.
            t.cursor = cursor;
            if let Some(ref mut renderer) = self.renderer {
                renderer.theme.pseudo_selection = pseudo_sel;
                renderer.theme.cursor = cursor;
            }
        }
    }

    /// Get the number of visible content rows (excludes status bar).
    /// Useful for JS to detect if mouse is on the status bar row.
    pub fn visible_rows(&self) -> usize {
        self.rows
    }

    /// Emoji cells in the visible viewport, packed as a flat Uint32Array.
    /// Layout: [row, col, span, len, cp_1, ..., cp_len, row, col, ...].
    /// `span` is the glyph's visual width in cells (1 or 2): narrow chars
    /// only get 2 when the neighbor cell is blank; otherwise JS scales the
    /// glyph to fit 1 cell. `len` is the number of codepoints (1 for most
    /// emoji, 2 for country flags built from a Regional Indicator pair,
    /// 3 for keycap sequences — literal 1️⃣ or substituted circled ①).
    ///
    /// JS unpacks variable-length entries and positions `<span>` overlays
    /// on top of the GPU canvas. The GPU atlas renders emoji cells as
    /// zero-size, so the overlay is the only visible emoji glyph.
    pub fn visible_emoji_cells(&self) -> Vec<u32> {
        use immorterm_render::atlas::{circled_number_value, is_emoji_codepoint};
        let mut out = Vec::new();
        let sb_len = self.terminal.scrollback.len();
        // Iterate DISPLAY rows (0..self.rows) and resolve each to the content
        // row actually painted at that viewport position. Emitting display_row
        // keeps the DOM overlay locked to what the GPU canvas is rendering —
        // both while scrolled into scrollback and as live content scrolls up.
        for display_row in 0..self.rows {
            let content_idx = (sb_len + display_row).saturating_sub(self.scroll_offset);
            let row = if content_idx < sb_len {
                self.terminal.scrollback.get(content_idx)
            } else {
                self.terminal.grid.row(content_idx - sb_len)
            };
            let Some(row) = row else { continue };
            let cells = &row.cells;
            let mut col = 0;
            while col < cells.len() {
                let cell = &cells[col];
                if cell.width == 0 {
                    col += 1;
                    continue;
                }
                let cp = cell.grapheme as u32;

                // Visual span in cells: wide chars keep their 2 columns;
                // narrow chars (circled numbers, keycap bases, dual-
                // presentation emoji like ❤) borrow the neighbor cell when
                // it is blank so the glyph renders full-size — otherwise
                // they stay 1 cell and the JS overlay scales the glyph down
                // to avoid covering the neighbor (e.g. the dash in "①–⑩").
                let neighbor_blank = cells
                    .get(col + 1)
                    .is_none_or(|c| matches!(c.grapheme, ' ' | '\0'));
                let span: u32 = if cell.width >= 2 || neighbor_blank { 2 } else { 1 };

                // Literal keycap sequence in the stream (1️⃣ = digit + VS16
                // + U+20E3): the base char is an ASCII digit, so it never
                // matches is_emoji_codepoint. write_char brands the cell with
                // the KEYCAP attr — unlike the combining-marks side table,
                // attrs travel with the row into scrollback.
                if cell.attrs.contains(CellAttrs::KEYCAP)
                    && matches!(cell.grapheme, '0'..='9' | '#' | '*')
                {
                    out.push(display_row as u32);
                    out.push(col as u32);
                    out.push(span);
                    out.push(3); // codepoint count
                    out.push(cp);
                    out.push(0xFE0F); // VS16 — emoji presentation
                    out.push(0x20E3); // combining enclosing keycap
                    col += 1;
                    continue;
                }

                // Circled numbers (①, ❶, ➀ …) have no color glyph in the
                // system emoji font — substitute a keycap: 0–9 → digit +
                // VS16 + U+20E3 (e.g. 1️⃣), 10 → 🔟 (U+1F51F), 11–20 → the
                // original codepoint, which JS renders as a CSS-drawn keycap
                // lookalike (no emoji equivalent exists past 10).
                if let Some(n) = circled_number_value(cell.grapheme) {
                    out.push(display_row as u32);
                    out.push(col as u32);
                    out.push(span);
                    if n >= 11 {
                        out.push(1); // codepoint count
                        out.push(cp); // JS detects circled-11..20 → CSS keycap
                    } else if n == 10 {
                        out.push(1); // codepoint count
                        out.push(0x1F51F); // 🔟
                    } else {
                        out.push(3); // codepoint count
                        out.push(0x30 + n as u32); // ASCII digit
                        out.push(0xFE0F); // VS16 — emoji presentation
                        out.push(0x20E3); // combining enclosing keycap
                    }
                    col += 1;
                    continue;
                }

                if !is_emoji_codepoint(cell.grapheme) {
                    col += 1;
                    continue;
                }

                // Country flag: two adjacent Regional Indicator codepoints
                // (U+1F1E6..=U+1F1FF) combine into a single flag glyph.
                if matches!(cp, 0x1F1E6..=0x1F1FF) && col + 1 < cells.len() {
                    let next_cp = cells[col + 1].grapheme as u32;
                    if matches!(next_cp, 0x1F1E6..=0x1F1FF) {
                        out.push(display_row as u32);
                        out.push(col as u32);
                        out.push(2); // span
                        out.push(2); // codepoint count
                        out.push(cp);
                        out.push(next_cp);
                        col += 2;
                        continue;
                    }
                }
                // Single-codepoint emoji
                out.push(display_row as u32);
                out.push(col as u32);
                out.push(span);
                out.push(1); // codepoint count
                out.push(cp);
                col += 1;
            }
        }
        out
    }

    /// Get cell dimensions in device pixels: [width, height].
    /// To convert to CSS pixels, divide by window.devicePixelRatio in JS.
    /// Use these for accurate mouse-to-cell conversion.
    pub fn cell_size_device(&self) -> Vec<f32> {
        match &self.renderer {
            Some(r) => vec![r.atlas.metrics.cell_width, r.atlas.metrics.cell_height],
            None => vec![0.0, 0.0],
        }
    }

    /// Debug: dump grid contents + BiDi mapping for cursor row.
    /// Call from JS console: `terminal.debug_bidi_row()` to diagnose Hebrew rendering.
    pub fn debug_bidi_row(&self) -> String {
        let row_idx = self.terminal.cursor.row;
        let row = match self.terminal.grid.row(row_idx) {
            Some(r) => r,
            None => return format!("no row at {}", row_idx),
        };

        // Grid contents: first 40 chars with their widths
        let mut chars_str = String::new();
        let mut widths_str = String::new();
        let mut non_ascii_positions = Vec::new();
        for (i, cell) in row.cells.iter().enumerate().take(40) {
            chars_str.push(cell.grapheme);
            widths_str.push_str(&format!("{}", cell.width));
            if cell.grapheme as u32 > 127 && cell.grapheme != ' ' {
                non_ascii_positions.push((i, cell.grapheme, cell.width));
            }
        }

        // BiDi cache for this row
        let renderer = match &self.renderer {
            Some(r) => r,
            None => return format!("row={} grid='{}'  (no renderer)", row_idx, chars_str.trim_end()),
        };
        let sb_len = self.terminal.scrollback.len();
        let content_idx = sb_len + row_idx;
        let bidi_info = match renderer.bidi_cache_for(content_idx) {
            Some(b) => {
                let l2v: Vec<String> = non_ascii_positions.iter()
                    .map(|&(col, ch, w)| {
                        let vis = b.logical_to_visual.get(col).copied().unwrap_or(9999);
                        format!("'{}' log={} vis={} w={}", ch, col, vis, w)
                    })
                    .collect();
                format!(
                    "dir={:?} align_off={:.1}px has_rtl={} rtl_runs={:?} mapping=[{}]",
                    b.resolved_direction, b.alignment_offset_px, b.has_rtl,
                    b.rtl_runs, l2v.join(", ")
                )
            }
            None => "no bidi cache".to_string(),
        };

        format!(
            "row={} grid[0..40]='{}' widths='{}' non_ascii={:?} bidi: {}",
            row_idx,
            chars_str.trim_end(),
            widths_str,
            non_ascii_positions.iter().map(|&(i, c, w)| format!("{}:'{}' w{}", i, c, w)).collect::<Vec<_>>(),
            bidi_info,
        )
    }

    /// Debug: return cursor position info for troubleshooting selection.
    pub fn debug_cursor(&self) -> String {
        let sb_len = self.terminal.scrollback.len();
        let cursor_content = sb_len + self.terminal.cursor.row;
        // Sample text around cursor row
        let cursor_row_text: String = if let Some(row) = self.terminal.grid.row(self.terminal.cursor.row) {
            row.cells.iter().take(60).map(|c| c.grapheme).collect::<String>().trim_end().to_string()
        } else {
            "<no row>".to_string()
        };
        // Also show prev row for context
        let prev_row_text: String = if self.terminal.cursor.row > 0 {
            if let Some(row) = self.terminal.grid.row(self.terminal.cursor.row - 1) {
                row.cells.iter().take(60).map(|c| c.grapheme).collect::<String>().trim_end().to_string()
            } else { "<no row>".to_string() }
        } else { "<row 0>".to_string() };
        // Check saved cursor (DECSC)
        let saved_info = if let Some(ref saved) = self.terminal.cursor.saved {
            format!("saved=({},{}) ", saved.col, saved.row)
        } else {
            "saved=none ".to_string()
        };
        format!(
            "cursor: col={} row={} vis={}/{} {}| grid: {}x{} | sb: {} | content: {} | sel: active={} a=({},{}) p=({},{}) | row[{}]: '{}' | row[{}]: '{}'",
            self.terminal.cursor.col,
            self.terminal.cursor.row,
            self.terminal.cursor.visible,
            self.terminal.modes.cursor_visible,
            saved_info,
            self.rows, self.cols,
            sb_len,
            cursor_content,
            self.selection.is_active,
            self.selection.anchor.0, self.selection.anchor.1,
            self.selection.active.0, self.selection.active.1,
            self.terminal.cursor.row.saturating_sub(1), prev_row_text,
            self.terminal.cursor.row, cursor_row_text,
        )
    }

    /// Debug: dump the renderer's current theme state to console.
    /// Call from JS as `terminal.debug_theme()` to verify ANSI overrides are applied.
    pub fn debug_theme(&self) -> String {
        let renderer = match &self.renderer {
            Some(r) => r,
            None => return "renderer not initialized".to_string(),
        };
        let t = &renderer.theme;
        let mut out = format!(
            "bg: [{:.3},{:.3},{:.3}] fg: [{:.3},{:.3},{:.3}] cursor: [{:.3},{:.3},{:.3}]\n",
            t.bg[0], t.bg[1], t.bg[2], t.fg[0], t.fg[1], t.fg[2], t.cursor[0], t.cursor[1], t.cursor[2],
        );
        match &t.ansi_overrides {
            Some(overrides) => {
                out.push_str("ansi_overrides: SET\n");
                let names = [
                    "Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White",
                    "BrBlack", "BrRed", "BrGreen", "BrYellow", "BrBlue", "BrMagenta", "BrCyan", "BrWhite",
                ];
                for (i, name) in names.iter().enumerate() {
                    let c = overrides[i];
                    out.push_str(&format!(
                        "  [{:>2}] {:>10}: #{:02X}{:02X}{:02X}\n",
                        i, name,
                        (c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8,
                    ));
                }
            }
            None => out.push_str("ansi_overrides: NONE (using default Nord palette)\n"),
        }
        out
    }

    // ── Background Session State Management ──
    //
    // Each terminal session has its own Terminal state. The active session's
    // state lives in self.terminal; background sessions are stored in
    // background_states[]. Switching sessions = swapping Terminal structs
    // in memory (zero serialization, instant).

    /// Save the active terminal state to a background slot and replace it
    /// with a fresh blank terminal. Returns the slot ID for later restore.
    /// Call this when switching AWAY from a session.
    pub fn save_active(&mut self) -> u32 {
        let state = BackgroundState {
            terminal: std::mem::replace(
                &mut self.terminal,
                Terminal::new(self.cols, self.rows),
            ),
            scroll_offset: std::mem::replace(&mut self.scroll_offset, 0),
            selection: std::mem::take(&mut self.selection),
            last_activity_ms: std::mem::replace(&mut self.last_activity_ms, 0.0),
            ai_stats: std::mem::take(&mut self.ai_stats),
            ai_ctx_pct: std::mem::replace(&mut self.ai_ctx_pct, 0.0),
            session_title: std::mem::take(&mut self.session_title),
            immorterm_id: std::mem::take(&mut self.immorterm_id),
            title_marquee_time: std::mem::replace(&mut self.title_marquee_time, 0.0),
            title_marquee_overflow: std::mem::replace(&mut self.title_marquee_overflow, 0),
            // Inline comments are per-session — stored in the background
            // state so they don't leak into other tabs. line_ids are tied
            // to the owning terminal's `scrollback.net_shift`, so mixing
            // them across sessions causes drifted underlines.
            comments: std::mem::take(&mut self.comments),
        };

        // Find a free slot or push a new one
        let id = self.background_states.iter().position(|s| s.is_none())
            .unwrap_or_else(|| {
                self.background_states.push(None);
                self.background_states.len() - 1
            });
        self.background_states[id] = Some(state);
        self.scroll_deficit = 0;
        id as u32
    }

    /// Restore a background session as the active terminal.
    /// The current active state is DISCARDED — caller must save_active() first
    /// if they want to keep it.
    pub fn restore(&mut self, id: u32) -> Result<(), JsValue> {
        let state = self.background_states.get_mut(id as usize)
            .and_then(|s| s.take())
            .ok_or_else(|| JsValue::from_str(&format!("no background state at slot {}", id)))?;

        self.terminal = state.terminal;
        self.scroll_offset = state.scroll_offset;
        self.scroll_deficit = 0;
        self.selection = state.selection;
        self.last_activity_ms = state.last_activity_ms;
        self.ai_stats = state.ai_stats;
        self.ai_ctx_pct = state.ai_ctx_pct;
        self.session_title = state.session_title;
        self.immorterm_id = state.immorterm_id;
        self.title_marquee_time = state.title_marquee_time;
        self.title_marquee_overflow = state.title_marquee_overflow;
        self.comments = state.comments;
        // Clear BiDi cache — old session's RTL/LTR entries don't apply to the restored session
        if let Some(ref mut r) = self.renderer {
            r.clear_bidi_cache();
        }
        Ok(())
    }

    /// Resize all background session terminals to match current cols/rows.
    /// Call after `resize()` so background sessions reflow their grids to
    /// the new dimensions — otherwise they stay at stale sizes and look
    /// distorted when restored.
    pub fn resize_backgrounds(&mut self) {
        for slot in &mut self.background_states {
            if let Some(state) = slot.as_mut() {
                state.terminal.resize(self.cols, self.rows);
            }
        }
    }

    /// Drop a background session's terminal state, freeing its memory.
    /// Called when a session switches from raw to control-only mode.
    pub fn drop_background(&mut self, id: u32) -> Result<(), JsValue> {
        let slot = self.background_states.get_mut(id as usize)
            .ok_or_else(|| JsValue::from_str(&format!("no background slot {}", id)))?;
        *slot = None;
        Ok(())
    }

    /// Process raw PTY bytes into a background session's terminal state.
    /// Called for every WS binary message from a non-active session.
    pub fn process_background(&mut self, id: u32, data: &[u8]) -> Result<bool, JsValue> {
        let state = self.background_states.get_mut(id as usize)
            .and_then(|s| s.as_mut())
            .ok_or_else(|| JsValue::from_str(&format!("no background state at slot {}", id)))?;
        state.terminal.process(data);
        state.last_activity_ms = js_sys::Date::now();
        // Return true if the terminal bell fired during this chunk
        Ok(state.terminal.take_bell())
    }

    /// Load a daemon snapshot into a background session's terminal.
    /// Used for initial connect and reconnect of non-active sessions.
    pub fn load_snapshot_background(&mut self, id: u32, json: &str) -> Result<(), JsValue> {
        let state = self.background_states.get_mut(id as usize)
            .and_then(|s| s.as_mut())
            .ok_or_else(|| JsValue::from_str(&format!("no background state at slot {}", id)))?;
        let snap: immorterm_core::TerminalSnapshot = serde_json::from_str(json)
            .map_err(|e| JsValue::from_str(&format!("snapshot parse error: {}", e)))?;
        let prev_offset = state.scroll_offset;

        // Preserve scrollback for viewport-only snapshots (same as load_snapshot)
        let preserve_sb = snap.scrollback.is_empty() && !state.terminal.scrollback.is_empty();
        let saved_sb = if preserve_sb {
            Some(std::mem::replace(
                &mut state.terminal.scrollback,
                immorterm_core::Scrollback::new(0),
            ))
        } else {
            None
        };

        state.terminal = immorterm_core::Terminal::from_snapshot(snap);
        // state.terminal.enable_marker_parsing(); // DISABLED: testing horizontal line artifacts
        if let Some(sb) = saved_sb {
            state.terminal.scrollback = sb;
        }
        state.terminal.resize(self.cols, self.rows);
        state.scroll_offset = if preserve_sb {
            prev_offset.min(state.terminal.scrollback.len())
        } else {
            0
        };
        state.selection = Selection::default();
        Ok(())
    }

    /// Create a new background state from a daemon snapshot.
    /// Used when a new session connects while another is active.
    /// Returns the slot ID.
    pub fn create_background(&mut self, json: &str) -> Result<u32, JsValue> {
        let snap: immorterm_core::TerminalSnapshot = serde_json::from_str(json)
            .map_err(|e| JsValue::from_str(&format!("snapshot parse error: {}", e)))?;
        let mut terminal = immorterm_core::Terminal::from_snapshot(snap);
        // terminal.enable_marker_parsing(); // DISABLED: testing horizontal line artifacts
        terminal.resize(self.cols, self.rows);

        let state = BackgroundState {
            terminal,
            scroll_offset: 0,
            selection: Selection::default(),
            last_activity_ms: js_sys::Date::now(),
            ai_stats: String::new(),
            ai_ctx_pct: 0.0,
            session_title: String::new(),
            immorterm_id: String::new(),
            title_marquee_time: 0.0,
            title_marquee_overflow: 0,
            comments: Comments::new(),
        };

        let id = self.background_states.iter().position(|s| s.is_none())
            .unwrap_or_else(|| {
                self.background_states.push(None);
                self.background_states.len() - 1
            });
        self.background_states[id] = Some(state);
        Ok(id as u32)
    }

    /// Destroy a background state, freeing its memory.
    /// Call when a session is removed/closed.
    pub fn destroy_background(&mut self, id: u32) {
        if let Some(slot) = self.background_states.get_mut(id as usize) {
            *slot = None;
        }
    }

    /// Update AI stats for a background session.
    pub fn set_background_ai_stats(&mut self, id: u32, stats: &str) {
        if let Some(Some(state)) = self.background_states.get_mut(id as usize) {
            state.ai_stats = stats.to_string();
        }
    }

    /// Update session title for a background session.
    pub fn set_background_session_title(&mut self, id: u32, title: &str) {
        if let Some(Some(state)) = self.background_states.get_mut(id as usize) {
            state.session_title = title.to_string();
        }
    }
}

// Private helper methods (not exposed to JS)
impl WasmTerminalInner {
    /// Convert CSS pixel coordinates to terminal cell (col, row).
    /// Returns **logical** column — for BiDi rows, the visual column from pixel
    /// position is mapped back to the logical column via the BiDi cache.
    fn css_to_cell(&self, css_x: f32, css_y: f32) -> (usize, usize) {
        let renderer = match &self.renderer {
            Some(r) => r,
            None => return (0, 0),
        };

        let cw = renderer.atlas.metrics.cell_width;
        let ch = renderer.atlas.metrics.cell_height;

        // CSS coords → physical pixels, then subtract content padding
        let px = (css_x * self.dpr - self.pending_content_padding[3]).max(0.0);
        let py = (css_y * self.dpr - self.pending_content_padding[0]).max(0.0);

        let row = (py / ch).floor() as usize;
        let row = row.min(self.rows.saturating_sub(1));

        // Compute visual column (accounting for alignment offset)
        let content_row = self.display_to_content(row);
        let bidi = renderer.bidi_cache_for(content_row);
        let align_offset = bidi.map(|b| b.alignment_offset_px).unwrap_or(0.0);
        let visual_col = ((px - align_offset).max(0.0) / cw).floor() as usize;
        let visual_col = visual_col.min(self.cols.saturating_sub(1));

        // Map visual → logical column via BiDi cache
        let logical_col = bidi
            .and_then(|b| b.visual_to_logical.get(visual_col).copied())
            .unwrap_or(visual_col);

        (logical_col, row)
    }

    /// Convert a display row (0..rows-1) to an absolute content index.
    /// Matches the renderer's formula: content_idx = sb_len + display_row - scroll_offset
    fn display_to_content(&self, display_row: usize) -> usize {
        let sb_len = self.terminal.scrollback.len();
        (sb_len + display_row).saturating_sub(self.scroll_offset)
    }

    // ─── Inline comments ────────────────────────────────────────────────
    //
    // A comment anchors to a content row that the user selected from. Its
    // `line_id` is `scrollback.total_evicted + content_idx` at creation
    // time — stable across scrollback eviction because both sides shift
    // in lockstep. Resolving `line_id → display_row` is the inverse: find
    // the `content_idx` that still matches the snapshot `line_text`, then
    // convert to a display row using the renderer formula.

    /// Read a content row's text (cells → String, stripping zero-width
    /// continuation cells). Used for anchor snapshotting and orphan checks.
    fn read_content_row_text(&self, content_idx: usize) -> String {
        let sb_len = self.terminal.scrollback.len();
        let row = if content_idx < sb_len {
            self.terminal.scrollback.get(content_idx)
        } else {
            self.terminal.grid.row(content_idx - sb_len)
        };
        let Some(row) = row else { return String::new() };
        let mut s = String::with_capacity(row.cells.len());
        for cell in &row.cells {
            if cell.width == 0 {
                continue;
            }
            s.push(cell.grapheme);
        }
        s.trim_end().to_string()
    }

    /// Current absolute line_id for a given content_idx.
    ///
    /// Derived so that future state changes that move existing rows around
    /// (eviction → net_shift decreases; daemon prepends → net_shift
    /// increases) preserve the line_id. Inverting this at lookup time
    /// yields the row's CURRENT content_idx regardless of what happened
    /// in between.
    fn content_idx_to_line_id(&self, content_idx: usize) -> i64 {
        (content_idx as i64) - self.terminal.scrollback.net_shift()
    }

    /// Inverse: absolute line_id → current content_idx. None if the row
    /// has been evicted (resolves to a negative index).
    fn line_id_to_content_idx(&self, line_id: i64) -> Option<usize> {
        let idx = line_id + self.terminal.scrollback.net_shift();
        if idx < 0 {
            return None;
        }
        Some(idx as usize)
    }

    /// Convert an absolute content_idx to a visible display row, or None
    /// if it's scrolled off-screen. Inverse of `display_to_content`.
    fn content_to_display(&self, content_idx: usize) -> Option<usize> {
        let sb_len = self.terminal.scrollback.len();
        // content_idx = sb_len + display_row - scroll_offset
        // → display_row = content_idx + scroll_offset - sb_len
        let lhs = content_idx as i64 + self.scroll_offset as i64;
        let rhs = sb_len as i64;
        if lhs < rhs {
            return None;
        }
        let display_row = (lhs - rhs) as usize;
        if display_row < self.rows {
            Some(display_row)
        } else {
            None
        }
    }
}

// ─── Inline comments: public API on WasmTerminalInner ──────────────────────
// These are wrapped and exposed to JS by the outer `WasmTerminal` impl below.
impl WasmTerminalInner {
    /// Get the current selection's content-coordinate range as
    /// `[start_row, start_col, end_row, end_col]`. Empty vec if no selection
    /// or in pseudo-cursor mode. Row values are absolute content indices.
    pub fn pub_selection_content_range(&self) -> Vec<u32> {
        if !self.selection.is_active || !self.pseudo_cursors.is_empty() {
            return Vec::new();
        }
        let ((sc, sr), (ec, er)) = self.selection.range();
        vec![sr as u32, sc as u32, er as u32, ec as u32]
    }

    /// Scan content rows bottom-up from the live cursor for bullet rows
    /// belonging to the most recent assistant turn, and return the title
    /// spans of any bullet block (≥2 siblings at the same indent). Returns
    /// a flat array of `[start_row, start_col, end_row, end_col]` quads in
    /// absolute content coords — same format as `pub_selection_content_range`.
    ///
    /// Bullet markers detected: `-`, `*`, `•`, `\d+.`, `\d+)`. The title
    /// span starts at the first non-space cell after the marker. If that
    /// cell has `CellAttrs::BOLD`, the span ends at the last consecutive
    /// bold cell; otherwise it ends at the last non-space cell on the row.
    ///
    /// The scan stops as soon as a sentinel "user prompt" row is encountered
    /// (a row whose first non-space char is `❯`) so we never cross the
    /// previous turn's boundary. The scan walks all content rows back to
    /// the previous prompt — not just the visible viewport — so bullets
    /// that scrolled off-screen during a long reply are still detected.
    ///
    /// **Sentinel is REQUIRED**: if the scan exhausts `SCAN_CAP` rows
    /// without finding a `❯` user-prompt row, the entire result is
    /// discarded and an empty Vec is returned. This prevents the worst
    /// failure mode where a missed sentinel would let us harvest bullets
    /// from arbitrarily old turns in scrollback.
    ///
    /// Returns an empty Vec if fewer than 2 sibling bullets are detected —
    /// avoids firing on a single stray `1.` in build output.
    pub fn pub_detect_claude_bullets(&self) -> Vec<u32> {
        struct BulletCandidate {
            content_row: usize,
            indent: usize,
            title_start: usize,
            title_end: usize, // exclusive
        }

        // Hard upper bound on rows we'll walk back. Smaller than scrollback
        // so a missed sentinel can't drag us into prior turns. Long replies
        // (~500 grid rows) still fit; if Claude's reply is genuinely longer,
        // we'd rather refuse than risk crossing an unrecognized boundary.
        const SCAN_CAP: usize = 500;

        let sb_len = self.terminal.scrollback.len();
        let grid_rows = self.terminal.rows();

        // Helper: fetch a row by absolute content index.
        let get_row = |content_row: usize| -> Option<&Row> {
            if content_row < sb_len {
                self.terminal.scrollback.get(content_row)
            } else {
                self.terminal.grid.row(content_row - sb_len)
            }
        };

        let mut candidates: Vec<BulletCandidate> = Vec::new();
        // Count of `❯` rows we've encountered going bottom-up. The FIRST
        // `❯` (going up from the cursor) is the LIVE input prompt — Claude's
        // response sits ABOVE it. The SECOND `❯` is the previous user turn,
        // which is our preferred stop boundary. We only collect bullets
        // between sentinel #1 and sentinel #2 (or top-of-scrollback / scan
        // cap if there's no second sentinel — common right after a reload
        // when the previous turn isn't in scrollback).
        //
        // Required: sentinel_count >= 1 at end (i.e., we crossed the live
        // input prompt). Without it, we have no anchor to know where the
        // assistant response begins, so we refuse to fire.
        let mut sentinel_count = 0u32;

        // Start at the most recent live grid row (bottom of the terminal,
        // not the viewport — so we capture the latest turn even if the
        // user has scrolled away).
        let bottom_content_row = sb_len + grid_rows.saturating_sub(1);
        let mut scanned = 0usize;

        // Bottom-up scan over content rows. Stop when we hit a sentinel row
        // (user prompt) so we stay within the latest turn. Each iteration
        // processes one row in a labeled `'row` block — `break 'row` skips
        // to the decrement at the end without aborting the outer scan.
        let mut content_row = bottom_content_row;
        'outer: loop {
            scanned += 1;
            if scanned > SCAN_CAP {
                break;
            }

            'row: {
                let row = match get_row(content_row) {
                    Some(r) => r,
                    None => break 'row,
                };
                let cells = &row.cells;
                if cells.is_empty() {
                    break 'row;
                }

                // Find first non-space cell ("indent").
                let indent = cells
                    .iter()
                    .position(|c| c.grapheme != ' ' && c.grapheme != '\0')
                    .unwrap_or(cells.len());
                if indent >= cells.len() {
                    break 'row; // blank row — skip but don't stop
                }

                // Sentinel detection (window scan). Claude's prompt indicator
                // is `❯` — often wrapped in a box (`│ ❯ …`) so we scan the
                // first ~10 cells, not just the first non-space position.
                let sentinel_window = cells.len().min(10);
                let is_sentinel_row = cells[..sentinel_window]
                    .iter()
                    .any(|c| c.grapheme == '❯');

                if is_sentinel_row {
                    sentinel_count += 1;
                    if sentinel_count >= 2 {
                        // Crossed into the previous turn — clean stop.
                        break 'outer;
                    }
                    // First `❯` is the LIVE input prompt; skip and keep
                    // scanning upward into Claude's response.
                    break 'row;
                }

                // Below the live prompt: still inside the user's input box.
                // We won't find Claude's bullets here, so skip without
                // attempting bullet detection.
                if sentinel_count == 0 {
                    break 'row;
                }

                let first_ch = cells[indent].grapheme;

                // Try to detect a bullet marker starting at `indent`.
                let marker_end: Option<usize> = match first_ch {
                    '-' | '*' | '•' => {
                        // Single-char marker; must be followed by a space.
                        if indent + 1 < cells.len() && cells[indent + 1].grapheme == ' ' {
                            Some(indent + 1) // position of the trailing space
                        } else {
                            None
                        }
                    }
                    c if c.is_ascii_digit() => {
                        // Walk forward over digits, then expect '.' or ')',
                        // then a space.
                        let mut k = indent;
                        while k < cells.len() && cells[k].grapheme.is_ascii_digit() {
                            k += 1;
                        }
                        if k < cells.len()
                            && (cells[k].grapheme == '.' || cells[k].grapheme == ')')
                            && k + 1 < cells.len()
                            && cells[k + 1].grapheme == ' '
                        {
                            Some(k + 1)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };

                let marker_end = match marker_end {
                    Some(e) => e,
                    None => break 'row, // not a bullet — skip but keep scanning
                };

                // Find the first non-space cell after the marker — title start.
                let title_start = (marker_end + 1..cells.len())
                    .find(|&i| cells[i].grapheme != ' ' && cells[i].grapheme != '\0')
                    .unwrap_or(cells.len());
                if title_start >= cells.len() {
                    break 'row; // marker with empty title
                }

                // Title end: if first title cell is BOLD, extend over the
                // bold run; otherwise extend to the last non-blank cell.
                let first_is_bold = cells[title_start].attrs.contains(CellAttrs::BOLD);
                let title_end = if first_is_bold {
                    let mut e = title_start + 1;
                    while e < cells.len() && cells[e].attrs.contains(CellAttrs::BOLD) {
                        e += 1;
                    }
                    e
                } else {
                    let mut e = cells.len();
                    while e > title_start
                        && (cells[e - 1].grapheme == ' ' || cells[e - 1].grapheme == '\0')
                    {
                        e -= 1;
                    }
                    e
                };

                candidates.push(BulletCandidate {
                    content_row,
                    indent,
                    title_start,
                    title_end,
                });
            }

            if content_row == 0 {
                break;
            }
            content_row -= 1;
        }

        // Refuse to fire if we never crossed the live input prompt. Without
        // that anchor, we don't know where the assistant response begins.
        if sentinel_count == 0 {
            return Vec::new();
        }
        if candidates.len() < 2 {
            return Vec::new();
        }

        // Candidates are bottom-up; reverse to top-down.
        candidates.reverse();

        // Find the longest run of consecutive (in scan order) candidates
        // that share the same indent — that's the most likely bullet list.
        // We scan candidates and group by indent + adjacency in row index.
        let mut best_start = 0usize;
        let mut best_len = 0usize;
        let mut i = 0usize;
        while i < candidates.len() {
            let mut j = i + 1;
            while j < candidates.len()
                && candidates[j].indent == candidates[i].indent
                // Consecutive candidates can have non-bullet rows between
                // them (continuation lines), so allow any forward gap.
                && candidates[j].content_row > candidates[j - 1].content_row
            {
                j += 1;
            }
            let len = j - i;
            if len > best_len {
                best_len = len;
                best_start = i;
            }
            i = j;
        }

        if best_len < 2 {
            return Vec::new();
        }

        // Emit ranges in [sr, sc, er, ec] form. End col is INCLUSIVE in the
        // selection model used by pseudo_selection.contains, so we subtract 1.
        let mut out: Vec<u32> = Vec::with_capacity(best_len * 4);
        for c in &candidates[best_start..best_start + best_len] {
            let er_col = c.title_end.saturating_sub(1);
            out.push(c.content_row as u32);
            out.push(c.title_start as u32);
            out.push(c.content_row as u32);
            out.push(er_col as u32);
        }
        out
    }

    /// Detect emphasised (bold) text rows in the visible viewport.
    /// Returns a flat array of `[start_row, start_col, end_row, end_col]`
    /// quads — **one range per row** spanning from the first bold cell
    /// to the last bold cell on that row. This is the fallback for
    /// Cmd+D when no bullet markers are present; treats each line that
    /// contains bold text as a single "headline" task rather than
    /// fragmenting per-word (Claude renders un-bolded spaces between
    /// bold words, which would otherwise split "**Wave 1 —
    /// correctness (do first):**" into 5 separate tasks).
    ///
    /// Skips lines with <3 bold cells (filters spurious single-glyph
    /// emphasis like a bold colon or arrow). Requires ≥2 qualifying
    /// rows to fire so a single bold label doesn't trigger the wizard.
    pub fn pub_detect_bold_runs_viewport(&self) -> Vec<u32> {
        let sb_len = self.terminal.scrollback.len();
        let display_rows = self.terminal.rows();

        let get_row = |content_row: usize| -> Option<&Row> {
            if content_row < sb_len {
                self.terminal.scrollback.get(content_row)
            } else {
                self.terminal.grid.row(content_row - sb_len)
            }
        };

        let mut out: Vec<u32> = Vec::new();
        for d in 0..display_rows {
            let content_row = self.display_to_content(d);
            let row = match get_row(content_row) {
                Some(r) => r,
                None => continue,
            };
            let cells = &row.cells;
            if cells.is_empty() { continue; }

            // Find first and last bold non-space cells on this row.
            // Anything between them — bold or not — is part of the
            // "bold line" we capture.
            let mut first: Option<usize> = None;
            let mut last: Option<usize> = None;
            let mut bold_count = 0usize;
            for (idx, c) in cells.iter().enumerate() {
                if c.attrs.contains(CellAttrs::BOLD)
                    && c.grapheme != ' '
                    && c.grapheme != '\0'
                {
                    if first.is_none() { first = Some(idx); }
                    last = Some(idx);
                    bold_count += 1;
                }
            }

            if let (Some(s), Some(e)) = (first, last) {
                // Need ≥3 bold cells total on the row — filters lines
                // where only a single short label is bold (e.g. a one-
                // word inline emphasis that isn't a real headline).
                if bold_count >= 3 && e >= s {
                    out.push(content_row as u32);
                    out.push(s as u32);
                    out.push(content_row as u32);
                    out.push(e as u32);
                }
            }
        }

        // 4 u32s per row. Need ≥2 rows (8 u32s) to fire.
        if out.len() < 8 { return Vec::new(); }
        out
    }

    /// Fallback for `pub_detect_claude_bullets`: scans only the visible
    /// viewport rows (display 0..rows-1) without the `❯` sentinel logic.
    /// Used when the turn-bounded scan returns nothing — e.g. the user
    /// has scrolled up to an older turn whose live prompt is no longer
    /// in scrollback. Same ≥2-sibling indent rule still applies.
    pub fn pub_detect_claude_bullets_viewport(&self) -> Vec<u32> {
        struct BulletCandidate {
            content_row: usize,
            indent: usize,
            title_start: usize,
            title_end: usize,
        }

        let sb_len = self.terminal.scrollback.len();
        let display_rows = self.terminal.rows();

        let get_row = |content_row: usize| -> Option<&Row> {
            if content_row < sb_len {
                self.terminal.scrollback.get(content_row)
            } else {
                self.terminal.grid.row(content_row - sb_len)
            }
        };

        let mut candidates: Vec<BulletCandidate> = Vec::new();

        for d in 0..display_rows {
            let content_row = self.display_to_content(d);
            let row = match get_row(content_row) {
                Some(r) => r,
                None => continue,
            };
            let cells = &row.cells;
            if cells.is_empty() { continue; }

            let indent = cells.iter()
                .position(|c| c.grapheme != ' ' && c.grapheme != '\0')
                .unwrap_or(cells.len());
            if indent >= cells.len() { continue; }

            let first_ch = cells[indent].grapheme;
            let marker_end: Option<usize> = match first_ch {
                '-' | '*' | '•' => {
                    if indent + 1 < cells.len() && cells[indent + 1].grapheme == ' ' {
                        Some(indent + 1)
                    } else { None }
                }
                c if c.is_ascii_digit() => {
                    let mut k = indent;
                    while k < cells.len() && cells[k].grapheme.is_ascii_digit() { k += 1; }
                    if k < cells.len()
                        && (cells[k].grapheme == '.' || cells[k].grapheme == ')')
                        && k + 1 < cells.len()
                        && cells[k + 1].grapheme == ' '
                    { Some(k + 1) } else { None }
                }
                _ => None,
            };
            let marker_end = match marker_end {
                Some(e) => e,
                None => continue,
            };

            let title_start = (marker_end + 1..cells.len())
                .find(|&i| cells[i].grapheme != ' ' && cells[i].grapheme != '\0')
                .unwrap_or(cells.len());
            if title_start >= cells.len() { continue; }

            let first_is_bold = cells[title_start].attrs.contains(CellAttrs::BOLD);
            let title_end = if first_is_bold {
                let mut e = title_start + 1;
                while e < cells.len() && cells[e].attrs.contains(CellAttrs::BOLD) { e += 1; }
                e
            } else {
                let mut e = cells.len();
                while e > title_start
                    && (cells[e - 1].grapheme == ' ' || cells[e - 1].grapheme == '\0')
                { e -= 1; }
                e
            };

            candidates.push(BulletCandidate {
                content_row, indent, title_start, title_end,
            });
        }

        if candidates.len() < 2 { return Vec::new(); }

        // Same longest-same-indent run grouping as the turn-bounded scan.
        let mut best_start = 0usize;
        let mut best_len = 0usize;
        let mut i = 0usize;
        while i < candidates.len() {
            let mut j = i + 1;
            while j < candidates.len()
                && candidates[j].indent == candidates[i].indent
                && candidates[j].content_row > candidates[j - 1].content_row
            { j += 1; }
            let len = j - i;
            if len > best_len { best_len = len; best_start = i; }
            i = j;
        }
        if best_len < 2 { return Vec::new(); }

        let mut out: Vec<u32> = Vec::with_capacity(best_len * 4);
        for c in &candidates[best_start..best_start + best_len] {
            let er_col = c.title_end.saturating_sub(1);
            out.push(c.content_row as u32);
            out.push(c.title_start as u32);
            out.push(c.content_row as u32);
            out.push(er_col as u32);
        }
        out
    }

    /// Diagnostic mirror of `pub_detect_claude_bullets` — returns a JSON
    /// blob describing what the scan saw so JS can `console.log` it. Cheap
    /// to call, intended for debugging detection failures from the webview
    /// devtools.
    pub fn pub_debug_claude_bullets(&self) -> String {
        const SCAN_CAP: usize = 500;
        let sb_len = self.terminal.scrollback.len();
        let grid_rows = self.terminal.rows();
        let bottom = sb_len + grid_rows.saturating_sub(1);

        let get_row = |content_row: usize| -> Option<&Row> {
            if content_row < sb_len {
                self.terminal.scrollback.get(content_row)
            } else {
                self.terminal.grid.row(content_row - sb_len)
            }
        };

        let mut scanned = 0usize;
        let mut found_sentinel = false;
        let mut sentinel_count = 0u32;
        let mut sentinel_rows: Vec<usize> = Vec::new();
        let mut bullet_rows: Vec<(usize, usize, String)> = Vec::new(); // (row, indent, preview)

        let mut content_row = bottom;
        'outer: loop {
            scanned += 1;
            if scanned > SCAN_CAP {
                break;
            }
            'row: {
                let row = match get_row(content_row) {
                    Some(r) => r,
                    None => break 'row,
                };
                let cells = &row.cells;
                if cells.is_empty() {
                    break 'row;
                }

                let indent = cells.iter()
                    .position(|c| c.grapheme != ' ' && c.grapheme != '\0')
                    .unwrap_or(cells.len());
                if indent >= cells.len() { break 'row; }

                // Sentinel detection (window scan). First `❯` going up is
                // the live input prompt; we skip it. Second `❯` is the
                // previous turn — that's our hard stop.
                let win = cells.len().min(10);
                if cells[..win].iter().any(|c| c.grapheme == '❯') {
                    sentinel_count += 1;
                    sentinel_rows.push(content_row);
                    if sentinel_count >= 2 {
                        found_sentinel = true;
                        break 'outer;
                    }
                    break 'row;
                }

                // Still inside the live input box — wait until we cross
                // the first `❯` before collecting bullets.
                if sentinel_count == 0 { break 'row; }

                let first_ch = cells[indent].grapheme;
                let is_marker = matches!(first_ch, '-' | '*' | '•')
                    || first_ch.is_ascii_digit();
                if !is_marker { break 'row; }

                // Quick preview: grab up to 40 chars of row text.
                let preview: String = cells.iter()
                    .take(40)
                    .map(|c| if c.grapheme == '\0' { ' ' } else { c.grapheme })
                    .collect();
                bullet_rows.push((content_row, indent, preview));
            }
            if content_row == 0 { break; }
            content_row -= 1;
        }

        // Build a tiny hand-rolled JSON blob (avoid serde dep).
        let mut s = String::new();
        s.push_str("{\"scanned\":");
        s.push_str(&scanned.to_string());
        s.push_str(",\"bottom_row\":");
        s.push_str(&bottom.to_string());
        s.push_str(",\"sb_len\":");
        s.push_str(&sb_len.to_string());
        s.push_str(",\"grid_rows\":");
        s.push_str(&grid_rows.to_string());
        s.push_str(",\"found_sentinel\":");
        s.push_str(if found_sentinel { "true" } else { "false" });
        s.push_str(",\"sentinel_count\":");
        s.push_str(&sentinel_count.to_string());
        s.push_str(",\"sentinel_rows\":[");
        for (i, r) in sentinel_rows.iter().enumerate() {
            if i > 0 { s.push(','); }
            s.push_str(&r.to_string());
        }
        s.push(']');
        s.push_str(",\"bullet_count\":");
        s.push_str(&bullet_rows.len().to_string());
        s.push_str(",\"bullets\":[");
        for (i, (r, indent, prev)) in bullet_rows.iter().take(8).enumerate() {
            if i > 0 { s.push(','); }
            s.push_str("{\"row\":");
            s.push_str(&r.to_string());
            s.push_str(",\"indent\":");
            s.push_str(&indent.to_string());
            s.push_str(",\"text\":");
            // Naive JSON escape — strip quotes/backslashes/control chars.
            s.push('"');
            for c in prev.chars() {
                if c == '"' || c == '\\' { s.push('\\'); }
                if (c as u32) < 0x20 { s.push(' '); } else { s.push(c); }
            }
            s.push('"');
            s.push('}');
        }
        s.push_str("]}");
        s
    }

    /// Add a comment anchored at the current selection.
    /// Returns the new comment id, or 0 if there's no active selection.
    pub fn pub_add_comment_for_selection(
        &mut self,
        comment_text: String,
        created_at_ms: f64,
    ) -> u32 {
        if !self.selection.is_active || !self.pseudo_cursors.is_empty() {
            return 0;
        }
        let ((sc, sr), (ec, er)) = self.selection.range();
        let selection_text = self.get_selected_text();

        // Anchor pill to the first row of the selection. col_end is clamped
        // to that row's last column for single-line selections; for multi-row
        // the pill just marks the start.
        let anchor_row = sr;
        let (col_start, col_end) = if sr == er {
            (sc, ec)
        } else {
            let sb_len = self.terminal.scrollback.len();
            let row = if anchor_row < sb_len {
                self.terminal.scrollback.get(anchor_row)
            } else {
                self.terminal.grid.row(anchor_row - sb_len)
            };
            let len = row.map(|r| r.cells.len()).unwrap_or(0);
            (sc, len.saturating_sub(1))
        };

        let line_text = self.read_content_row_text(anchor_row);
        let line_id = self.content_idx_to_line_id(anchor_row);

        self.comments.add(
            line_id,
            col_start,
            col_end,
            line_text,
            selection_text,
            comment_text,
            created_at_ms,
        )
    }

    /// Read the visible text of a content-coord range
    /// `[sr, sc, er, ec]`. Used by the Cmd+E wizard to embed each
    /// bullet's title into the editor preview. Returns an empty string
    /// if the range is invalid; trims trailing whitespace.
    pub fn pub_read_range_text(
        &self,
        sr: usize,
        sc: usize,
        er: usize,
        ec: usize,
    ) -> String {
        let mut buf = String::new();
        let sb_len = self.terminal.scrollback.len();
        let get_row = |row: usize| -> Option<&Row> {
            if row < sb_len {
                self.terminal.scrollback.get(row)
            } else {
                self.terminal.grid.row(row - sb_len)
            }
        };
        if er < sr {
            return buf;
        }
        for row in sr..=er {
            let cells = match get_row(row) {
                Some(r) => &r.cells,
                None => continue,
            };
            let lo = if row == sr { sc } else { 0 };
            let hi = if row == er { (ec + 1).min(cells.len()) } else { cells.len() };
            for c in &cells[lo..hi] {
                if c.grapheme != '\0' {
                    buf.push(c.grapheme);
                }
            }
            if row != er {
                buf.push('\n');
            }
        }
        buf.trim_end().to_string()
    }

    /// Stage a comment anchored at an explicit content-coord range
    /// `[sr, sc, er, ec]` instead of the live selection. Used by the
    /// Cmd+E auto-comment wizard, which iterates over multiple
    /// pre-detected bullet ranges while pseudo-cursors hold the
    /// visualization (so `pub_add_comment_for_selection`'s pseudo-cursor
    /// guard would otherwise reject every call).
    /// Returns the new comment id, or 0 if the range is invalid.
    pub fn pub_add_comment_for_range(
        &mut self,
        sr: usize,
        sc: usize,
        er: usize,
        ec: usize,
        comment_text: String,
        created_at_ms: f64,
    ) -> u32 {
        // Snapshot the selected text by reading the cells in the range
        // so the citation block carries the exact bullet title.
        let selection_text = {
            let mut buf = String::new();
            let sb_len = self.terminal.scrollback.len();
            let get_row = |row: usize| -> Option<&Row> {
                if row < sb_len {
                    self.terminal.scrollback.get(row)
                } else {
                    self.terminal.grid.row(row - sb_len)
                }
            };
            for row in sr..=er {
                let cells = match get_row(row) {
                    Some(r) => &r.cells,
                    None => continue,
                };
                let lo = if row == sr { sc } else { 0 };
                let hi = if row == er { (ec + 1).min(cells.len()) } else { cells.len() };
                for c in &cells[lo..hi] {
                    if c.grapheme != '\0' {
                        buf.push(c.grapheme);
                    }
                }
                if row != er { buf.push('\n'); }
            }
            buf.trim_end().to_string()
        };

        let line_text = self.read_content_row_text(sr);
        let line_id = self.content_idx_to_line_id(sr);

        self.comments.add(
            line_id,
            sc,
            ec,
            line_text,
            selection_text,
            comment_text,
            created_at_ms,
        )
    }

    /// Remove a comment by id. No-op if id doesn't exist.
    pub fn pub_remove_comment(&mut self, id: u32) -> bool {
        self.comments.remove(id)
    }

    /// Clear all staged comments.
    pub fn pub_clear_comments(&mut self) {
        self.comments.clear();
    }

    /// Number of staged comments.
    pub fn pub_comments_count(&self) -> u32 {
        self.comments.len() as u32
    }

    /// All staged comments as a JSON array. Used by the webview to render
    /// the compose panel's cards and to serialize the citation block on send.
    pub fn pub_list_comments_json(&self) -> String {
        serde_json::to_string(&self.comments.items).unwrap_or_else(|_| "[]".into())
    }

    /// Update the body text of an existing comment. Returns true if found.
    pub fn pub_update_comment_text(&mut self, id: u32, new_text: String) -> bool {
        self.comments.update_text(id, new_text)
    }

    /// Visible comment anchors for the current frame.
    /// Packed layout: `[display_row, col_start, col_end, id, ...]`. Pills
    /// are emitted whenever (a) the anchor row hasn't been evicted from
    /// scrollback and (b) it's currently visible in the viewport. We do
    /// NOT compare the live row text to the stored snapshot — TUIs like
    /// Claude Code rewrite rows on every tick (same visible content, new
    /// bytes), which would cause a byte-level check to continuously hide
    /// valid pills. The stored `line_text` / `selection_text` remain in
    /// the comment record so the citation sent to Claude still reflects
    /// what the user originally saw.
    pub fn pub_visible_comment_anchors(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for c in &self.comments.items {
            let Some(content_idx) = self.line_id_to_content_idx(c.line_id) else {
                continue; // evicted
            };
            let Some(display_row) = self.content_to_display(content_idx) else {
                continue; // scrolled off
            };
            out.push(display_row as u32);
            out.push(c.col_start as u32);
            out.push(c.col_end as u32);
            out.push(c.id);
        }
        out
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Outer JS-facing wrapper. Routes every method call through a RefCell so that
// WebKit's synchronous DOM event dispatch (which re-enters wasm mid-method)
// degrades to a dropped no-op instead of a wasm-bindgen "recursive use of an
// object" panic. See fix/wasm-reentrancy branch history for the investigation.
// ═══════════════════════════════════════════════════════════════════════════

impl WasmTerminalInner {
    /// Async entry point used by the outer wrapper. Holds the RefCell borrow
    /// across the adapter/device awaits. Safe on wasm32 (single-threaded; !Send
    /// futures OK in wasm-bindgen-futures). Any concurrent call from JS will
    /// fail the try_borrow_mut and be reported as a re-entrant error.
    // The RefCell guard is intentionally held across the await — that's the
    // serialization mechanism that prevents wasm-bindgen's "recursive use of
    // an object" panic when init_gpu re-enters via the JS side.
    #[allow(clippy::await_holding_refcell_ref)]
    pub async fn init_gpu_shared(
        cell: &std::cell::RefCell<Self>,
        canvas_id: &str,
        dpr: f32,
    ) -> Result<(), JsValue> {
        let mut inner = cell
            .try_borrow_mut()
            .map_err(|_| JsValue::from_str("init_gpu: another call in progress"))?;
        inner.init_gpu(canvas_id, dpr).await
    }
}

// ────────────────────────────────────────────────────────────────
// JS-facing wrapper that gates every call through a RefCell borrow.
// Prevents wasm-bindgen's "recursive use of an object" panic when
// WebKit dispatches DOM events synchronously mid-render.
// ────────────────────────────────────────────────────────────────

#[wasm_bindgen]
pub struct WasmTerminal {
    inner: std::cell::RefCell<WasmTerminalInner>,
}

#[wasm_bindgen]
impl WasmTerminal {
    #[wasm_bindgen(constructor)]
    pub fn new(cols: usize, rows: usize) -> Self {
        Self { inner: std::cell::RefCell::new(WasmTerminalInner::new(cols, rows)) }
    }

    pub async fn init_gpu(&self, canvas_id: &str, dpr: f32) -> Result<(), JsValue> {
        // Split-phase: hold the borrow only around the sync parts.
        // WasmTerminalInner::init_gpu_async does the await-free work inside
        // borrow_mut scopes and releases during adapter/device awaits.
        WasmTerminalInner::init_gpu_shared(&self.inner, canvas_id, dpr).await
    }

    pub fn load_snapshot(&self, json: &str, immorterm_id: &str) -> Result<(), JsValue> {
        let mut i = self.inner.try_borrow_mut()
            .map_err(|_| JsValue::from_str("load_snapshot: re-entrant call blocked"))?;
        i.load_snapshot(json, immorterm_id)
    }

    pub fn process(&self, data: &[u8]) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.process(data);
        }
    }

    pub fn process_str(&self, text: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.process_str(text);
        }
    }

    pub fn set_status_bar_enabled(&self, enabled: bool) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_status_bar_enabled(enabled);
        }
    }

    pub fn set_status_bar_reveal(&self, reveal: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_status_bar_reveal(reveal);
        }
    }

    pub fn set_status_bar_mode(&self, mode: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_status_bar_mode(mode);
        }
    }

    pub fn set_border_enabled(&self, enabled: bool) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_border_enabled(enabled);
        }
    }

    pub fn set_border_opacity(&self, opacity: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_border_opacity(opacity);
        }
    }

    pub fn set_animations_enabled(&self, enabled: bool) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_animations_enabled(enabled);
        }
    }

    pub fn set_expression_effects(&self, enabled: bool) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_expression_effects(enabled);
        }
    }

    pub fn set_celebrations_enabled(&self, enabled: bool) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_celebrations_enabled(enabled);
        }
    }

    pub fn set_danger_effects(&self, enabled: bool) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_danger_effects(enabled);
        }
    }

    pub fn set_expression(&self, json: &str) -> Result<(), JsValue> {
        let mut i = self.inner.try_borrow_mut()
            .map_err(|_| JsValue::from_str("set_expression: re-entrant call blocked"))?;
        i.set_expression(json)
    }

    pub fn set_text_animations(&self, enabled: bool) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_text_animations(enabled);
        }
    }

    pub fn render(&self) -> bool {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.render(),
            Err(_) => Default::default(),
        }
    }

    pub fn handle_key(&self, key: &str, ctrl: bool, shift: bool, alt: bool) -> Vec<u8> {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.handle_key(key, ctrl, shift, alt),
            Err(_) => Default::default(),
        }
    }

    pub fn selection_start(&self, css_x: f32, css_y: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.selection_start(css_x, css_y);
        }
    }

    pub fn select_word_at(&self, css_x: f32, css_y: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.select_word_at(css_x, css_y);
        }
    }

    pub fn select_line_at(&self, css_x: f32, css_y: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.select_line_at(css_x, css_y);
        }
    }

    pub fn selection_start_block(&self, css_x: f32, css_y: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.selection_start_block(css_x, css_y);
        }
    }

    pub fn selection_update(&self, css_x: f32, css_y: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.selection_update(css_x, css_y);
        }
    }

    pub fn selection_extend(&self, direction: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.selection_extend(direction);
        }
    }

    pub fn selection_clear(&self) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.selection_clear();
        }
    }

    pub fn select_all_input(&self) -> bool {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.select_all_input(),
            Err(_) => Default::default(),
        }
    }

    pub fn mouse_tracking_enabled(&self) -> bool {
        match self.inner.try_borrow() {
            Ok(i) => i.mouse_tracking_enabled(),
            Err(_) => Default::default(),
        }
    }

    pub fn encode_mouse_event(&self, button: u8, pressed: bool, css_x: f32, css_y: f32) -> Vec<u8> {
        match self.inner.try_borrow() {
            Ok(i) => i.encode_mouse_event(button, pressed, css_x, css_y),
            Err(_) => Default::default(),
        }
    }

    pub fn debug_click_trace(&self, css_x: f32, css_y: f32) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.debug_click_trace(css_x, css_y),
            Err(_) => Default::default(),
        }
    }

    pub fn debug_click_info(&self, css_x: f32, css_y: f32) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.debug_click_info(css_x, css_y),
            Err(_) => Default::default(),
        }
    }

    pub fn link_at(&self, css_x: f32, css_y: f32) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.link_at(css_x, css_y),
            Err(_) => Default::default(),
        }
    }

    pub fn click_to_cursor_sequence(&self, css_x: f32, css_y: f32) -> Vec<u8> {
        match self.inner.try_borrow() {
            Ok(i) => i.click_to_cursor_sequence(css_x, css_y),
            Err(_) => Default::default(),
        }
    }

    pub fn click_to_cursor_correction_seq(&self, target_col: usize, target_grid_row: usize) -> Vec<u8> {
        match self.inner.try_borrow() {
            Ok(i) => i.click_to_cursor_correction_seq(target_col, target_grid_row),
            Err(_) => Default::default(),
        }
    }

    pub fn visual_cursor_display(&self) -> Vec<i32> {
        match self.inner.try_borrow() {
            Ok(i) => i.visual_cursor_display(),
            Err(_) => Default::default(),
        }
    }

    pub fn cell_grapheme_at(&self, grid_row: usize, col: usize) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.cell_grapheme_at(grid_row, col),
            Err(_) => Default::default(),
        }
    }

    pub fn click_to_cursor_plan(&self, css_x: f32, css_y: f32) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.click_to_cursor_plan(css_x, css_y),
            Err(_) => Default::default(),
        }
    }

    pub fn paste_undo_probe(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.paste_undo_probe(),
            Err(_) => "{\"ok\":false,\"reason\":\"busy\"}".to_string(),
        }
    }

    pub fn delete_selection_sequence(&self) -> Vec<u8> {
        match self.inner.try_borrow() {
            Ok(i) => i.delete_selection_sequence(),
            Err(_) => Default::default(),
        }
    }

    pub fn has_selection(&self) -> bool {
        match self.inner.try_borrow() {
            Ok(i) => i.has_selection(),
            Err(_) => Default::default(),
        }
    }

    pub fn get_selected_text(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.get_selected_text(),
            Err(_) => Default::default(),
        }
    }

    pub fn get_selected_html(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.get_selected_html(),
            Err(_) => Default::default(),
        }
    }

    pub fn debug_selection_wrapped(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.debug_selection_wrapped(),
            Err(_) => Default::default(),
        }
    }

    pub fn pseudo_cursor_add(&self, css_x: f32, css_y: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.pseudo_cursor_add(css_x, css_y);
        }
    }

    pub fn pseudo_cursor_add_at(&self, col: usize, content_row: usize) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.pseudo_cursor_add_at(col, content_row);
        }
    }

    pub fn pseudo_cursor_add_at_visual_cursor(&self) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.pseudo_cursor_add_at_visual_cursor();
        }
    }

    pub fn pseudo_cursor_add_vertical(&self, direction: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.pseudo_cursor_add_vertical(direction);
        }
    }

    pub fn pseudo_cursor_extend_all(&self, direction: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.pseudo_cursor_extend_all(direction);
        }
    }

    pub fn pseudo_cursor_clear(&self) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.pseudo_cursor_clear();
        }
    }

    pub fn has_pseudo_cursors(&self) -> bool {
        match self.inner.try_borrow() {
            Ok(i) => i.has_pseudo_cursors(),
            Err(_) => Default::default(),
        }
    }

    pub fn pseudo_cursor_count(&self) -> usize {
        match self.inner.try_borrow() {
            Ok(i) => i.pseudo_cursor_count(),
            Err(_) => Default::default(),
        }
    }

    pub fn get_pseudo_cursor_text(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.get_pseudo_cursor_text(),
            Err(_) => Default::default(),
        }
    }

    pub fn reinit_renderer(&self, new_dpr: f32) -> Result<Vec<usize>, JsValue> {
        let mut i = self.inner.try_borrow_mut()
            .map_err(|_| JsValue::from_str("reinit_renderer: re-entrant call blocked"))?;
        i.reinit_renderer(new_dpr)
    }

    pub fn resize(&self, width: u32, height: u32) -> Vec<usize> {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.resize(width, height),
            Err(_) => Default::default(),
        }
    }

    pub fn dimensions(&self) -> Vec<usize> {
        match self.inner.try_borrow() {
            Ok(i) => i.dimensions(),
            Err(_) => Default::default(),
        }
    }

    pub fn scroll_offset(&self) -> usize {
        match self.inner.try_borrow() {
            Ok(i) => i.scroll_offset(),
            Err(_) => Default::default(),
        }
    }

    pub fn take_bell(&self) -> bool {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.take_bell(),
            Err(_) => Default::default(),
        }
    }

    pub fn scrollback_len(&self) -> usize {
        match self.inner.try_borrow() {
            Ok(i) => i.scrollback_len(),
            Err(_) => Default::default(),
        }
    }

    pub fn scroll(&self, delta: i32) -> bool {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.scroll(delta),
            Err(_) => Default::default(),
        }
    }

    pub fn set_scroll_offset(&self, offset: usize) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_scroll_offset(offset);
        }
    }

    pub fn scroll_to_bottom(&self) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.scroll_to_bottom();
        }
    }

    pub fn prepend_scrollback(&self, json: &str) -> Result<(), JsValue> {
        let mut i = self.inner.try_borrow_mut()
            .map_err(|_| JsValue::from_str("prepend_scrollback: re-entrant call blocked"))?;
        i.prepend_scrollback(json)
    }

    pub fn clear_scrollback(&self) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.clear_scrollback();
        }
    }

    pub fn set_scroll_indicator_proximity(&self, proximity: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_scroll_indicator_proximity(proximity);
        }
    }

    pub fn status_bar_hit_test(&self, col: usize) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.status_bar_hit_test(col),
            Err(_) => Default::default(),
        }
    }

    pub fn set_status_bar_hover(&self, target: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_status_bar_hover(target);
        }
    }

    pub fn set_font_size(&self, size: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_font_size(size);
        }
    }

    pub fn set_line_height(&self, value: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_line_height(value);
        }
    }

    pub fn set_char_height_css(&self, value: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_char_height_css(value);
        }
    }

    pub fn set_font_weight(&self, weight: u16) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_font_weight(weight);
        }
    }

    pub fn set_content_padding(&self, top: f32, right: f32, bottom: f32, left: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_content_padding(top, right, bottom, left);
        }
    }

    pub fn set_custom_font(&self, data: &[u8]) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_custom_font(data);
        }
    }

    pub fn set_custom_font_name(&self, name: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_custom_font_name(name);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_terminal_colors(&self, bg_r: f32, bg_g: f32, bg_b: f32,
        fg_r: f32, fg_g: f32, fg_b: f32,
        cursor_r: f32, cursor_g: f32, cursor_b: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_terminal_colors(bg_r, bg_g, bg_b, fg_r, fg_g, fg_b, cursor_r, cursor_g, cursor_b);
        }
    }

    pub fn set_selection_color(&self, r: f32, g: f32, b: f32, a: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_selection_color(r, g, b, a);
        }
    }

    pub fn set_ansi_colors(&self, colors: &[f32]) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_ansi_colors(colors);
        }
    }

    pub fn set_text_alignment(&self, alignment: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_text_alignment(alignment);
        }
    }

    pub fn set_paragraph_direction(&self, direction: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_paragraph_direction(direction);
        }
    }

    pub fn text_alignment(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.text_alignment(),
            Err(_) => Default::default(),
        }
    }

    pub fn paragraph_direction(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.paragraph_direction(),
            Err(_) => Default::default(),
        }
    }

    pub fn set_project_name(&self, name: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_project_name(name);
        }
    }

    pub fn title(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.title(),
            Err(_) => Default::default(),
        }
    }

    /// Current working directory (see inner `cwd()` for details).
    pub fn cwd(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.cwd(),
            Err(_) => String::new(),
        }
    }

    pub fn set_session_title(&self, title: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_session_title(title);
        }
    }

    pub fn set_immorterm_id(&self, id: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_immorterm_id(id);
        }
    }

    pub fn set_ai_stats(&self, stats: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_ai_stats(stats);
        }
    }

    pub fn set_ai_ctx_pct(&self, pct: f32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_ai_ctx_pct(pct);
        }
    }

    pub fn update_ai_primitives(&self, json: &str, daemon_sb_len: usize) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.update_ai_primitives(json, daemon_sb_len);
        }
    }

    pub fn get_ai_primitives_json(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.get_ai_primitives_json(),
            Err(_) => Default::default(),
        }
    }

    pub fn set_theme(&self, name: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_theme(name);
        }
    }

    pub fn visible_rows(&self) -> usize {
        match self.inner.try_borrow() {
            Ok(i) => i.visible_rows(),
            Err(_) => Default::default(),
        }
    }

    pub fn visible_emoji_cells(&self) -> Vec<u32> {
        match self.inner.try_borrow() {
            Ok(i) => i.visible_emoji_cells(),
            Err(_) => Default::default(),
        }
    }

    pub fn cell_size_device(&self) -> Vec<f32> {
        match self.inner.try_borrow() {
            Ok(i) => i.cell_size_device(),
            Err(_) => Default::default(),
        }
    }

    pub fn debug_bidi_row(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.debug_bidi_row(),
            Err(_) => Default::default(),
        }
    }

    pub fn debug_cursor(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.debug_cursor(),
            Err(_) => Default::default(),
        }
    }

    pub fn debug_theme(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.debug_theme(),
            Err(_) => Default::default(),
        }
    }

    pub fn save_active(&self) -> u32 {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.save_active(),
            Err(_) => Default::default(),
        }
    }

    pub fn restore(&self, id: u32) -> Result<(), JsValue> {
        let mut i = self.inner.try_borrow_mut()
            .map_err(|_| JsValue::from_str("restore: re-entrant call blocked"))?;
        i.restore(id)
    }

    pub fn resize_backgrounds(&self) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.resize_backgrounds();
        }
    }

    pub fn drop_background(&self, id: u32) -> Result<(), JsValue> {
        let mut i = self.inner.try_borrow_mut()
            .map_err(|_| JsValue::from_str("drop_background: re-entrant call blocked"))?;
        i.drop_background(id)
    }

    pub fn process_background(&self, id: u32, data: &[u8]) -> Result<bool, JsValue> {
        let mut i = self.inner.try_borrow_mut()
            .map_err(|_| JsValue::from_str("process_background: re-entrant call blocked"))?;
        i.process_background(id, data)
    }

    pub fn load_snapshot_background(&self, id: u32, json: &str) -> Result<(), JsValue> {
        let mut i = self.inner.try_borrow_mut()
            .map_err(|_| JsValue::from_str("load_snapshot_background: re-entrant call blocked"))?;
        i.load_snapshot_background(id, json)
    }

    pub fn create_background(&self, json: &str) -> Result<u32, JsValue> {
        let mut i = self.inner.try_borrow_mut()
            .map_err(|_| JsValue::from_str("create_background: re-entrant call blocked"))?;
        i.create_background(json)
    }

    pub fn destroy_background(&self, id: u32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.destroy_background(id);
        }
    }

    pub fn set_background_ai_stats(&self, id: u32, stats: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_background_ai_stats(id, stats);
        }
    }

    pub fn set_background_session_title(&self, id: u32, title: &str) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_background_session_title(id, title);
        }
    }

    // ─── Inline comments ────────────────────────────────────────────────
    // See `src/comments.rs` and the inner public-API block for details.

    /// Current selection as `[start_row, start_col, end_row, end_col]` in
    /// absolute content coordinates. Empty if no selection.
    pub fn selection_content_range(&self) -> Vec<u32> {
        match self.inner.try_borrow() {
            Ok(i) => i.pub_selection_content_range(),
            Err(_) => Vec::new(),
        }
    }

    /// Detect bullet titles in the current Claude turn (bottom-up scan
    /// bounded by a `❯` user-prompt sentinel). Returns a flat array of
    /// `[start_row, start_col, end_row, end_col]` quads in absolute
    /// content coords, or an empty array if no confident match is found.
    pub fn detect_claude_bullets(&self) -> Vec<u32> {
        match self.inner.try_borrow() {
            Ok(i) => i.pub_detect_claude_bullets(),
            Err(_) => Vec::new(),
        }
    }

    /// Fallback bullet detection over just the visible viewport. Use
    /// when `detect_claude_bullets` returns empty (e.g. the user has
    /// scrolled to an older turn). Same `≥2 sibling` rule applies.
    pub fn detect_claude_bullets_viewport(&self) -> Vec<u32> {
        match self.inner.try_borrow() {
            Ok(i) => i.pub_detect_claude_bullets_viewport(),
            Err(_) => Vec::new(),
        }
    }

    /// Detect bold/emphasised text runs in the visible viewport.
    /// Used by the Cmd+D no-selection path so the user can turn
    /// each emphasised phrase into a task. Returns flat
    /// `[sr, sc, er, ec, ...]` ranges. Requires ≥2 runs to fire.
    pub fn detect_bold_runs_viewport(&self) -> Vec<u32> {
        match self.inner.try_borrow() {
            Ok(i) => i.pub_detect_bold_runs_viewport(),
            Err(_) => Vec::new(),
        }
    }

    /// Diagnostic: returns a JSON string describing what the bullet
    /// detector saw. Helps debug why detect_claude_bullets returned empty.
    /// Includes: scanned row count, found_sentinel flag, candidate count,
    /// and the first 5 candidate previews. Call from JS console.
    pub fn debug_claude_bullets(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.pub_debug_claude_bullets(),
            Err(_) => "{\"error\":\"borrow failed\"}".to_string(),
        }
    }

    /// Replace pseudo-cursors with N range pseudo-selections, one per
    /// `[start_row, start_col, end_row, end_col]` quad in `flat`. Used by
    /// the Cmd+E auto-comment flow to visualize detected bullet titles.
    pub fn pseudo_select_ranges(&self, flat: &[u32]) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.pub_pseudo_select_ranges(flat.to_vec());
        }
    }

    /// Stage a new comment anchored at the current selection. Returns the
    /// new comment id (>0), or 0 if there is no active selection.
    pub fn add_comment_for_selection(&self, comment_text: &str, created_at_ms: f64) -> u32 {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.pub_add_comment_for_selection(comment_text.to_string(), created_at_ms),
            Err(_) => 0,
        }
    }

    /// Read the visible text of a content-coord range. Returns an
    /// empty string if the range is invalid.
    pub fn read_range_text(&self, sr: u32, sc: u32, er: u32, ec: u32) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.pub_read_range_text(sr as usize, sc as usize, er as usize, ec as usize),
            Err(_) => String::new(),
        }
    }

    /// Stage a comment anchored at an explicit content-coord range
    /// `[sr, sc, er, ec]`. Bypasses the live-selection / pseudo-cursor
    /// guards used by `add_comment_for_selection` — required by the
    /// Cmd+E wizard which iterates over multiple pre-detected ranges
    /// while pseudo-cursors are still held for visualization. Returns
    /// the new comment id, or 0 on failure.
    pub fn add_comment_for_range(&self, sr: u32, sc: u32, er: u32, ec: u32, comment_text: &str, created_at_ms: f64) -> u32 {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.pub_add_comment_for_range(
                sr as usize, sc as usize, er as usize, ec as usize,
                comment_text.to_string(), created_at_ms,
            ),
            Err(_) => 0,
        }
    }

    /// Remove a staged comment by id. Returns true if found.
    pub fn remove_comment(&self, id: u32) -> bool {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.pub_remove_comment(id),
            Err(_) => false,
        }
    }

    /// Drop every staged comment.
    pub fn clear_comments(&self) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.pub_clear_comments();
        }
    }

    /// Number of staged comments (may include orphaned ones).
    pub fn comments_count(&self) -> u32 {
        match self.inner.try_borrow() {
            Ok(i) => i.pub_comments_count(),
            Err(_) => 0,
        }
    }

    /// All staged comments as a JSON array string.
    /// Schema matches `comments::Comment`.
    pub fn list_comments_json(&self) -> String {
        match self.inner.try_borrow() {
            Ok(i) => i.pub_list_comments_json(),
            Err(_) => "[]".into(),
        }
    }

    /// Update a staged comment's body text.
    pub fn update_comment_text(&self, id: u32, new_text: &str) -> bool {
        match self.inner.try_borrow_mut() {
            Ok(mut i) => i.pub_update_comment_text(id, new_text.to_string()),
            Err(_) => false,
        }
    }

    /// Visible comment anchors for the current frame, packed as
    /// `[display_row, col_start, col_end, id, ...]`. JS positions a
    /// sidebar pill per entry using the same math as emoji overlays.
    pub fn visible_comment_anchors(&self) -> Vec<u32> {
        match self.inner.try_borrow() {
            Ok(i) => i.pub_visible_comment_anchors(),
            Err(_) => Vec::new(),
        }
    }
}
