//! Glyph atlas — shelf-packed GPU texture for terminal glyph rendering.
//!
//! Uses cosmic-text for font shaping and rasterization. Glyphs are cached
//! by (character, style_flags) and packed into a 2048x2048 R8Unorm texture.

use std::collections::HashMap;

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Style, Weight,
};
use immorterm_core::cell::CellAttrs;

/// Size of the atlas texture in pixels (2048x2048 = 4MB at R8Unorm).
const ATLAS_SIZE: u32 = 2048;

/// Cached glyph entry — UV coordinates and placement offsets for rendering.
#[derive(Debug, Clone, Copy)]
pub struct GlyphEntry {
    /// Normalized UV origin in atlas (x, y)
    pub uv_pos: [f32; 2],
    /// Normalized UV size in atlas (w, h)
    pub uv_size: [f32; 2],
    /// Pixel offset from cell origin to glyph bitmap
    pub offset: [f32; 2],
    /// Pixel size of glyph bitmap
    pub size: [f32; 2],
    /// True for color glyphs (emoji) — use RGBA color atlas instead of mask atlas
    pub is_color: bool,
}

/// Simple shelf allocator for packing glyphs into the atlas texture.
struct ShelfAllocator {
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    shelf_height: u32,
}

impl ShelfAllocator {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            cursor_x: 0,
            cursor_y: 0,
            shelf_height: 0,
        }
    }

    /// Allocate space for a glyph. Returns (x, y) in pixel coordinates.
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w == 0 || h == 0 {
            return Some((0, 0));
        }
        if w > self.width || h > self.height {
            return None;
        }
        // Start new shelf if current one is full
        if self.cursor_x + w > self.width {
            self.cursor_y += self.shelf_height;
            self.cursor_x = 0;
            self.shelf_height = 0;
        }
        if self.cursor_y + h > self.height {
            tracing::warn!("Glyph atlas full ({0}x{0})", self.width);
            return None;
        }
        let pos = (self.cursor_x, self.cursor_y);
        self.cursor_x += w;
        self.shelf_height = self.shelf_height.max(h);
        Some(pos)
    }
}

/// Cache key for glyphs: (character, bold | italic flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    ch: char,
    flags: u16, // CellAttrs bits (only BOLD | ITALIC matter for font selection)
}

/// Cache key for grapheme clusters: (base char + combining marks, style flags).
/// Used for characters with Hebrew niqqud, Arabic diacritics, etc.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClusterKey {
    /// Base character + combining marks as a string
    cluster: smallvec::SmallVec<[char; 5]>,
    flags: u16,
}

/// Whether a codepoint should render with color emoji presentation by default.
///
/// Covers the common emoji blocks including Misc Symbols (❤, ⚡, ☀, ⭐) and
/// Dingbats (✂, ✈, ❄) which — despite being "dual-presentation" in Unicode
/// (text or emoji via VS15/VS16) — render as color emoji in Apple Color Emoji,
/// Segoe UI Emoji, and Noto Color Emoji. In a terminal context, users expect
/// these to look like emoji, not monochrome symbols.
///
/// Matching codepoints are routed to the DOM `<span>` overlay layer (see
/// `visible_emoji_cells` and `updateBodyEmojiOverlays` in gpu-terminal.html)
/// so the browser's native text engine uses the system emoji font.
pub fn is_emoji_codepoint(ch: char) -> bool {
    // Circled numbers (①, ❶, ➀, ➊, ⓵, ⓪, ⓿) are substituted with color
    // keycap emoji (1️⃣, 🔟) in the DOM overlay — see `circled_number_value`
    // and `visible_emoji_cells`. They must be routed to the overlay layer
    // (zero-size in the GPU atlas) for the substitution to be visible.
    if circled_number_value(ch).is_some() {
        return true;
    }

    // Supplementary Plane (U+1F000+) — almost entirely emoji pictographs.
    // Fast path: any BMP-plus codepoint in these blocks renders as color emoji.
    if matches!(ch as u32,
        0x1F300..=0x1F5FF   // Misc Symbols and Pictographs (incl. 💙💚💛💜 hearts)
        | 0x1F600..=0x1F64F // Emoticons
        | 0x1F680..=0x1F6FF // Transport and Map
        | 0x1F7E0..=0x1F7EB // Geometric Shapes Extended — colored circles+squares
                            // 🟠🟡🟢🟣🟤  🟥🟦🟧🟨🟩🟪🟫
        | 0x1F900..=0x1F9FF // Supplemental Symbols and Pictographs (incl. 🤍🤎🧡)
        | 0x1FA00..=0x1FAFF // Symbols and Pictographs Extended-A (incl. 🩵🩶🩷)
    ) {
        return true;
    }

    // BMP codepoints — only those with Unicode property Emoji_Presentation=Yes
    // (formal "default emoji") PLUS a small user-requested whitelist for the
    // dual-presentation codepoints that users expect as color in chat context.
    //
    // We deliberately EXCLUDE codepoints that Claude Code and other TUIs use
    // as monochrome decoration:  ⏺ ⏸ ⏹ (record/pause/stop),  ✓ ✔ ✗ ✘ (checks
    // that may be styled with ANSI color),  ✻ ✶ ❁ ❀ ✿ (spinner/art glyphs),
    //  ▪ ▫ ▶ ◀ ◻ ◼ (geometric decoration).
    matches!(ch as u32,
        0x2139                             // ℹ  info (text default, common in CLI output)
        | 0x231A | 0x231B                  // ⌚ ⌛
        | 0x23E9..=0x23EC                  // ⏩ ⏪ ⏫ ⏬
        | 0x23F0..=0x23F3                  // ⏰ ⏱ ⏲ ⏳ (stopwatch/timer Claude uses)
        | 0x25FD | 0x25FE                  // ◽ ◾
        | 0x2600..=0x2604                  // ☀ ☁ ☂ ☃ ☄ (weather)
        | 0x2614 | 0x2615                  // ☔ ☕
        | 0x2648..=0x2653                  // Zodiac ♈..♓
        | 0x267F                           // ♿
        | 0x2693                           // ⚓
        | 0x2699                           // ⚙  gear (text default, Claude/CLI use)
        | 0x26A0 | 0x26A1                  // ⚠ ⚡ (warning + high voltage)
        | 0x26AA | 0x26AB                  // ⚪ ⚫
        | 0x26BD | 0x26BE                  // ⚽ ⚾
        | 0x26C4 | 0x26C5                  // ⛄ ⛅
        | 0x26CE                           // ⛎
        | 0x26D4                           // ⛔
        | 0x26EA                           // ⛪
        | 0x26F2 | 0x26F3 | 0x26F5         // ⛲ ⛳ ⛵
        | 0x26FA | 0x26FD                  // ⛺ ⛽
        | 0x2705                           // ✅
        | 0x2708                           // ✈  airplane (text default, common)
        | 0x270A | 0x270B                  // ✊ ✋
        | 0x2728                           // ✨
        | 0x2744                           // ❄  snowflake (text default)
        | 0x274C | 0x274E                  // ❌ ❎
        | 0x2753..=0x2755                  // ❓ ❔ ❕
        | 0x2757                           // ❗
        | 0x2763 | 0x2764                  // ❣ ❤ (text-default in Unicode, but
                                           //    users expect color in chat)
        | 0x2795..=0x2797                  // ➕ ➖ ➗
        | 0x27B0 | 0x27BF                  // ➰ ➿
        | 0x2B1B | 0x2B1C                  // ⬛ ⬜
        | 0x2B50 | 0x2B55                  // ⭐ ⭕
        | 0x1F000..=0x1F02F                // Mahjong
        | 0x1F0A0..=0x1F0FF                // Playing Cards
        | 0x1F100..=0x1F1FF                // Enclosed Alphanumeric + Flags
        | 0x1F200..=0x1F2FF                // Enclosed Ideographic
    )
}

/// Whether a (base char + combining marks) cluster forms an emoji keycap
/// sequence: digit/#/* followed by U+20E3 COMBINING ENCLOSING KEYCAP
/// (optionally with U+FE0F), e.g. a literal 1️⃣ in the byte stream.
/// These must render via the DOM emoji overlay — the GPU cluster path
/// rasterizes them as monochrome masks (boxed digit, no color).
pub fn is_keycap_cluster(base: char, combining: &[char]) -> bool {
    matches!(base, '0'..='9' | '#' | '*') && combining.contains(&'\u{20E3}')
}

/// Numeric value (0–20) of a circled-number codepoint, or None.
///
/// Covers the circled-number forms across the Enclosed Alphanumerics and
/// Dingbats blocks. These have NO color glyphs in Apple Color Emoji, so
/// instead of routing them to the overlay as-is (monochrome), the overlay
/// substitutes a keycap: 0–9 → real keycap emoji (digit + VS16 + U+20E3,
/// e.g. 1️⃣), 10 → 🔟 (U+1F51F), 11–20 → a CSS-drawn keycap lookalike in
/// the DOM overlay (no emoji equivalent exists).
pub fn circled_number_value(ch: char) -> Option<u8> {
    let cp = ch as u32;
    let value = match cp {
        0x2460..=0x2473 => cp - 0x2460 + 1, // ①–⑳  circled 1–20
        0x24EA => 0,                        // ⓪    circled 0
        0x24EB..=0x24F4 => cp - 0x24EB + 11, // ⓫–⓴ negative circled 11–20
        0x24F5..=0x24FE => cp - 0x24F5 + 1, // ⓵–⓾  double circled 1–10
        0x24FF => 0,                        // ⓿    negative circled 0
        0x2776..=0x277F => cp - 0x2776 + 1, // ❶–❿  negative circled 1–10
        0x2780..=0x2789 => cp - 0x2780 + 1, // ➀–➉  sans-serif circled 1–10
        0x278A..=0x2793 => cp - 0x278A + 1, // ➊–➓  negative sans-serif 1–10
        _ => return None,
    };
    Some(value as u8)
}

/// Cell metrics derived from the font.
#[derive(Debug, Clone, Copy)]
pub struct CellMetrics {
    pub cell_width: f32,
    pub cell_height: f32,
    pub baseline_y: f32, // distance from cell top to baseline (includes centering)
}

/// Fallback-rasterized glyph data (RGBA pixels from Canvas 2D or similar).
pub struct FallbackGlyph {
    pub data: Vec<u8>,     // RGBA pixel data
    pub width: u32,
    pub height: u32,
    pub bearing_x: f32,
    pub bearing_y: f32,
}

/// Primary-rasterized monochrome glyph data (alpha-only, from Canvas 2D).
/// Used to match VS Code's native font renderer (Skia/CoreText) instead
/// of cosmic-text's swash rasterizer.
pub struct MonoGlyph {
    pub data: Vec<u8>,     // Alpha-only (R8) pixel data
    pub width: u32,
    pub height: u32,
    pub offset_x: f32,    // Pixel offset from cell origin (trimmed left)
    pub offset_y: f32,    // Pixel offset from cell origin (trimmed top)
}

/// Rasterizer closure type for emoji/fallback glyphs (Canvas 2D on WASM).
#[cfg(target_arch = "wasm32")]
type FallbackRasterizer = Box<dyn Fn(char, f32) -> Option<FallbackGlyph>>;

/// Rasterizer closure type for primary monochrome glyphs (Canvas 2D on WASM).
/// Signature: (char, font_size, bold, italic) -> Option<MonoGlyph>
#[cfg(target_arch = "wasm32")]
type PrimaryRasterizer = Box<dyn Fn(char, f32, bool, bool) -> Option<MonoGlyph>>;

/// Rasterizer closure for text runs (e.g. Hebrew words) rendered as a single wide glyph.
/// Signature: (text, font_size, bold, italic, num_cells) -> Option<MonoGlyph>
/// The returned MonoGlyph spans `num_cells` cell widths with natural proportional spacing.
#[cfg(target_arch = "wasm32")]
type RunRasterizer = Box<dyn Fn(&str, f32, bool, bool, u32) -> Option<MonoGlyph>>;

/// GPU-backed glyph atlas with cosmic-text shaping and rasterization.
#[allow(dead_code)] // GPU resources must stay alive for RAII; font_size reserved for resize
pub struct GlyphAtlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Color atlas for emoji/color glyphs (Rgba8UnormSrgb)
    color_texture: wgpu::Texture,
    color_view: wgpu::TextureView,
    color_allocator: ShelfAllocator,
    sampler: wgpu::Sampler,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    allocator: ShelfAllocator,
    cache: HashMap<GlyphKey, GlyphEntry>,
    /// Cache for grapheme clusters (base + combining marks, e.g. Hebrew niqqud).
    cluster_cache: HashMap<ClusterKey, GlyphEntry>,
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// Reusable buffer for single-character shaping
    shape_buffer: Buffer,
    pub metrics: CellMetrics,
    font_size: f32,
    /// Base weight for non-bold text (from VS Code `terminal.integrated.fontWeight`).
    /// Default: Weight::NORMAL (400). Set to a heavier value (e.g. 500) to match
    /// the "fatter" look of canvas 2D subpixel rendering.
    base_weight: Weight,
    /// True when using embedded font with no bold/italic variants (WASM).
    /// In this case, always shape with Normal style to avoid "no default font" panics.
    single_font: bool,
    /// When a custom font is loaded (e.g. Menlo.ttc from VS Code), store its actual
    /// family name so we use `Family::Name("Menlo")` instead of `Family::Monospace`.
    /// cosmic-text's generic Monospace resolution can fail in an empty fontdb.
    custom_family: Option<String>,
    /// Canvas 2D fallback rasterizer for glyphs not found in loaded fonts (e.g. emoji).
    /// Only available on WASM where OffscreenCanvas provides access to system fonts.
    #[cfg(target_arch = "wasm32")]
    fallback_rasterizer: Option<FallbackRasterizer>,
    /// Canvas 2D primary rasterizer for ALL monochrome glyphs (WASM only).
    /// Uses the browser's native font renderer (Skia/CoreText) for pixel-identical
    /// results vs VS Code's terminal. Returns alpha-only data for the mask atlas.
    /// Signature: (char, font_size, bold, italic) -> Option<MonoGlyph>
    #[cfg(target_arch = "wasm32")]
    primary_rasterizer: Option<PrimaryRasterizer>,
    /// Canvas 2D run rasterizer for multi-character text runs (e.g. Hebrew words).
    /// Renders a string as a single wide glyph with natural proportional spacing,
    /// avoiding the uneven gaps from per-character monospace placement.
    #[cfg(target_arch = "wasm32")]
    run_rasterizer: Option<RunRasterizer>,
    /// Cache for run-rendered glyphs, keyed by (run_text, style_flags).
    #[cfg(target_arch = "wasm32")]
    run_cache: HashMap<(String, u16), GlyphEntry>,
}

impl GlyphAtlas {
    /// Create a new glyph atlas.
    ///
    /// If `font_data` is Some, loads the given font bytes (for WASM).
    /// Pass multiple variants (Regular, Italic, Bold, BoldItalic) for full style support.
    /// Otherwise uses the system font database.
    ///
    /// `line_height_ratio` controls the line-to-font-size multiplier (default 1.15).
    /// VS Code's `terminal.integrated.lineHeight` is applied ON TOP as a second multiplier.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        font_size: f32,
        font_data: Option<&[&[u8]]>,
    ) -> Self {
        Self::with_line_height(device, queue, font_size, font_data, 1.15)
    }

    /// Create a glyph atlas with explicit line-height multiplier.
    pub fn with_line_height(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        font_size: f32,
        font_data: Option<&[&[u8]]>,
        line_height_ratio: f32,
    ) -> Self {
        // On WASM (or when custom fonts are provided), create an empty database
        // and load ONLY the provided fonts. FontSystem::new() discovers system
        // fonts which don't exist in the browser — and without a default font,
        // cosmic-text panics during shaping ("no default font found").
        let (mut font_system, has_italic, custom_family) = if let Some(fonts) = font_data {
            let mut db = fontdb::Database::new();
            for data in fonts {
                db.load_font_data(data.to_vec());
            }
            // Extract the primary family name from the first loaded face.
            // This lets us use Family::Name("Menlo") instead of Family::Monospace,
            // which cosmic-text may fail to resolve in an isolated fontdb.
            let face_count = db.faces().count();
            let all_families: Vec<String> = db
                .faces()
                .flat_map(|f| f.families.iter().map(|(n, _)| n.clone()))
                .collect();
            eprintln!("[GlyphAtlas] loaded {} fonts, {} faces, families: {:?}",
                fonts.len(), face_count, all_families);
            let family_name = db
                .faces()
                .find(|f| f.families.iter().any(|(_, _)| true))
                .and_then(|f| f.families.first())
                .map(|(name, _)| name.clone());
            eprintln!("[GlyphAtlas] custom_family = {:?}", family_name);
            let has_italic = fonts.len() >= 2; // Regular + Italic at minimum
            (FontSystem::new_with_locale_and_db("en-US".to_string(), db), has_italic, family_name)
        } else {
            (FontSystem::new(), true, None) // system fonts have italic
        };

        let line_height = (font_size * line_height_ratio).ceil();
        let cosmic_metrics = Metrics::new(font_size, line_height);
        let mut shape_buffer = Buffer::new(&mut font_system, cosmic_metrics);
        shape_buffer.set_size(&mut font_system, Some(1000.0), Some(1000.0));

        let mut swash_cache = SwashCache::new();

        // Measure cell dimensions by shaping "M" at the ORIGINAL font size.
        // Cell width/height must stay based on original size for correct grid layout.
        let cell_metrics = measure_cell(
            &mut font_system,
            &mut swash_cache,
            &mut shape_buffer,
            font_size,
            line_height,
            custom_family.as_deref(),
        );

        // No glyph_scale — rasterize at the true font_size, same as VS Code/Chromium.
        // Weight matching is handled purely by the alpha curve in the fragment shader.

        // Create GPU texture
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph_atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Clear texture to zero (transparent)
        let zeros = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &zeros,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE),
                rows_per_image: Some(ATLAS_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Color atlas for emoji/color glyphs (RGBA, same size)
        let color_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color_glyph_atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Clear color atlas to zero (transparent black) — WebGPU requires initialization before sampling
        let color_zeros = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &color_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &color_zeros,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ATLAS_SIZE * 4),
                rows_per_image: Some(ATLAS_SIZE),
            },
            wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph_sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas_bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas_bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
            ],
        });

        Self {
            texture,
            view,
            color_texture,
            color_view,
            color_allocator: ShelfAllocator::new(ATLAS_SIZE, ATLAS_SIZE),
            sampler,
            bind_group_layout,
            bind_group,
            allocator: ShelfAllocator::new(ATLAS_SIZE, ATLAS_SIZE),
            cache: HashMap::with_capacity(512),
            cluster_cache: HashMap::new(),
            font_system,
            swash_cache,
            shape_buffer,
            metrics: cell_metrics,
            font_size,
            base_weight: Weight::NORMAL,
            single_font: !has_italic,
            custom_family,
            #[cfg(target_arch = "wasm32")]
            fallback_rasterizer: None,
            #[cfg(target_arch = "wasm32")]
            primary_rasterizer: None,
            #[cfg(target_arch = "wasm32")]
            run_rasterizer: None,
            #[cfg(target_arch = "wasm32")]
            run_cache: HashMap::new(),
        }
    }

    /// Try to rasterize a single character and upload to the atlas.
    /// Returns the GlyphEntry if successful, None if the font lacks the glyph.
    fn try_rasterize(
        &mut self,
        ch: char,
        attrs: CellAttrs,
        queue: &wgpu::Queue,
    ) -> Option<GlyphEntry> {
        // Emoji cells render nothing in the GPU atlas — they are handled by
        // the DOM overlay layer in the browser (see `visible_emoji_cells()`
        // and the `#emoji-overlays` <span> elements in gpu-terminal.html).
        // Returning a zero-size GlyphEntry reserves the cell's space in the
        // grid so overlays align correctly, while drawing nothing on the GPU.
        #[cfg(target_arch = "wasm32")]
        if is_emoji_codepoint(ch)
            || (attrs.contains(CellAttrs::KEYCAP) && matches!(ch, '0'..='9' | '#' | '*'))
        {
            return Some(GlyphEntry {
                uv_pos: [0.0, 0.0],
                uv_size: [0.0, 0.0],
                offset: [0.0, 0.0],
                size: [0.0, 0.0],
                is_color: false,
            });
        }

        let bold = attrs.contains(CellAttrs::BOLD);
        let italic = attrs.contains(CellAttrs::ITALIC) && !self.single_font;

        let family = match self.custom_family {
            Some(ref name) => Family::Name(name),
            None => Family::Monospace,
        };
        let font_attrs = Attrs::new()
            .family(family)
            .weight(if bold { Weight::BOLD } else { self.base_weight })
            .style(if italic { Style::Italic } else { Style::Normal });

        self.shape_buffer.set_text(
            &mut self.font_system,
            &ch.to_string(),
            font_attrs,
            Shaping::Advanced,
        );
        self.shape_buffer
            .shape_until_scroll(&mut self.font_system, false);

        let cache_key = self
            .shape_buffer
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first())
            .map(|g| g.physical((0.0, 0.0), 1.0).cache_key)?;

        // glyph_id 0 = .notdef — no loaded font has this character.
        // Try Canvas 2D primary rasterizer FIRST for monochrome glyphs (WASM only).
        // Uses the browser's native font renderer (Skia/CoreText) for pixel-identical
        // glyph shapes vs VS Code's terminal. Falls through to swash if unavailable.
        #[cfg(target_arch = "wasm32")]
        if let Some(ref rasterizer) = self.primary_rasterizer
            && let Some(mono) = rasterizer(ch, self.font_size, bold, italic)
            && mono.width > 0
            && mono.height > 0
        {
            let (ax, ay) = self.allocator.alloc(mono.width, mono.height)?;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: ax, y: ay, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &mono.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(mono.width),
                    rows_per_image: Some(mono.height),
                },
                wgpu::Extent3d { width: mono.width, height: mono.height, depth_or_array_layers: 1 },
            );
            let atlas_f = ATLAS_SIZE as f32;
            return Some(GlyphEntry {
                uv_pos: [ax as f32 / atlas_f, ay as f32 / atlas_f],
                uv_size: [mono.width as f32 / atlas_f, mono.height as f32 / atlas_f],
                offset: [mono.offset_x, mono.offset_y],
                size: [mono.width as f32, mono.height as f32],
                is_color: false,
            });
        }

        // Color fallback for emoji/color glyphs (WASM only).
        // Tried AFTER primary rasterizer so text chars (Hebrew, Arabic, CJK) get
        // rendered as alpha masks with proper fg color, not as baked-in black RGBA.
        #[cfg(target_arch = "wasm32")]
        if cache_key.glyph_id == 0
            && let Some(ref rasterizer) = self.fallback_rasterizer
            && let Some(fg) = rasterizer(ch, self.font_size)
            && fg.width > 0
            && fg.height > 0
        {
            let (ax, ay) = self.color_allocator.alloc(fg.width, fg.height)?;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.color_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: ax, y: ay, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &fg.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(fg.width * 4),
                    rows_per_image: Some(fg.height),
                },
                wgpu::Extent3d { width: fg.width, height: fg.height, depth_or_array_layers: 1 },
            );
            let atlas_f = ATLAS_SIZE as f32;
            return Some(GlyphEntry {
                uv_pos: [ax as f32 / atlas_f, ay as f32 / atlas_f],
                uv_size: [fg.width as f32 / atlas_f, fg.height as f32 / atlas_f],
                offset: [fg.bearing_x, fg.bearing_y],
                size: [fg.width as f32, fg.height as f32],
                is_color: true,
            });
        }

        let image = self
            .swash_cache
            .get_image_uncached(&mut self.font_system, cache_key)?;

        let is_color = matches!(image.content, SwashContent::Color);
        match image.content {
            SwashContent::Mask | SwashContent::Color => {}
            _ => return None,
        }

        let pw = image.placement.width;
        let ph = image.placement.height;

        if pw == 0 || ph == 0 {
            return Some(GlyphEntry {
                uv_pos: [0.0, 0.0],
                uv_size: [0.0, 0.0],
                offset: [0.0, 0.0],
                size: [0.0, 0.0],
                is_color: false,
            });
        }

        if is_color {
            let (ax, ay) = self.color_allocator.alloc(pw, ph)?;
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.color_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: ax, y: ay, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &image.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pw * 4),
                    rows_per_image: Some(ph),
                },
                wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
            );
            let atlas_f = ATLAS_SIZE as f32;
            return Some(GlyphEntry {
                uv_pos: [ax as f32 / atlas_f, ay as f32 / atlas_f],
                uv_size: [pw as f32 / atlas_f, ph as f32 / atlas_f],
                offset: [
                    image.placement.left as f32,
                    self.metrics.baseline_y - image.placement.top as f32,
                ],
                size: [pw as f32, ph as f32],
                is_color: true,
            });
        }

        let (ax, ay) = self.allocator.alloc(pw, ph)?;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: ax, y: ay, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &image.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pw),
                rows_per_image: Some(ph),
            },
            wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
        );
        let atlas_f = ATLAS_SIZE as f32;
        Some(GlyphEntry {
            uv_pos: [ax as f32 / atlas_f, ay as f32 / atlas_f],
            uv_size: [pw as f32 / atlas_f, ph as f32 / atlas_f],
            offset: [
                image.placement.left as f32,
                self.metrics.baseline_y - image.placement.top as f32,
            ],
            size: [pw as f32, ph as f32],
            is_color: false,
        })
    }

    /// Synthesize a "tofu" box for characters with no font glyph.
    /// Creates a small filled rectangle (~60% of cell size) in the mask atlas.
    fn synthesize_tofu(&mut self, queue: &wgpu::Queue) -> Option<GlyphEntry> {
        let pw = (self.metrics.cell_width * 0.6).ceil() as u32;
        let ph = (self.metrics.cell_height * 0.6).ceil() as u32;
        if pw == 0 || ph == 0 {
            return None;
        }

        let (ax, ay) = self.allocator.alloc(pw, ph)?;
        let data: Vec<u8> = vec![0xFF; (pw * ph) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: ax, y: ay, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pw),
                rows_per_image: Some(ph),
            },
            wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
        );
        let atlas_f = ATLAS_SIZE as f32;
        let offset_x = (self.metrics.cell_width - pw as f32) * 0.5;
        let offset_y = (self.metrics.cell_height - ph as f32) * 0.5;
        Some(GlyphEntry {
            uv_pos: [ax as f32 / atlas_f, ay as f32 / atlas_f],
            uv_size: [pw as f32 / atlas_f, ph as f32 / atlas_f],
            offset: [offset_x, offset_y],
            size: [pw as f32, ph as f32],
            is_color: false,
        })
    }

    /// Get or rasterize a glyph, returning its atlas entry.
    /// Fallback chain: original char → U+FFFD replacement → tofu box.
    pub fn get_glyph(
        &mut self,
        ch: char,
        attrs: CellAttrs,
        queue: &wgpu::Queue,
    ) -> Option<GlyphEntry> {
        let key = GlyphKey {
            ch,
            // KEYCAP participates in the key: a keycap '1' (zero-size, drawn
            // by the DOM overlay) must not share a cache slot with a plain '1'.
            flags: (attrs & (CellAttrs::BOLD | CellAttrs::ITALIC | CellAttrs::KEYCAP)).bits(),
        };

        if let Some(entry) = self.cache.get(&key) {
            return Some(*entry);
        }

        // Fallback chain: try original → U+FFFD → tofu box
        let mut entry = self.try_rasterize(ch, attrs, queue);
        if entry.is_none() && ch != '\u{FFFD}' {
            entry = self.try_rasterize('\u{FFFD}', attrs, queue);
        }
        if entry.is_none() {
            entry = self.synthesize_tofu(queue);
        }

        if let Some(e) = entry {
            self.cache.insert(key, e);
            return Some(e);
        }
        None
    }

    /// Get or rasterize a grapheme cluster (base char + combining marks).
    /// Used for Hebrew niqqud, Arabic diacritics, and other combining sequences.
    /// The full cluster is shaped as a unit so the font's GPOS tables position marks correctly.
    pub fn get_glyph_cluster(
        &mut self,
        base: char,
        combining: &[char],
        attrs: CellAttrs,
        num_cells: usize,
        queue: &wgpu::Queue,
    ) -> Option<GlyphEntry> {
        #[cfg(not(target_arch = "wasm32"))]
        let _ = num_cells;
        // Emoji clusters render in the DOM overlay, not the GPU atlas —
        // mirrors the `is_emoji_codepoint` skip in `try_rasterize`. Covers
        // keycap sequences (1️⃣ = digit + U+20E3) and emoji bases carrying
        // VS16 (❤ + U+FE0F), which would otherwise rasterize as monochrome
        // masks here AND get an overlay glyph (double render).
        #[cfg(target_arch = "wasm32")]
        if is_emoji_codepoint(base) || is_keycap_cluster(base, combining) {
            return Some(GlyphEntry {
                uv_pos: [0.0, 0.0],
                uv_size: [0.0, 0.0],
                offset: [0.0, 0.0],
                size: [0.0, 0.0],
                is_color: false,
            });
        }

        // Build cache key
        let mut cluster = smallvec::SmallVec::<[char; 5]>::new();
        cluster.push(base);
        cluster.extend_from_slice(combining);

        let key = ClusterKey {
            cluster: cluster.clone(),
            flags: (attrs & (CellAttrs::BOLD | CellAttrs::ITALIC)).bits(),
        };

        if let Some(entry) = self.cluster_cache.get(&key) {
            return Some(*entry);
        }

        // Build the cluster string for shaping
        let text: String = cluster.iter().collect();

        // WASM: prefer the Canvas 2D run rasterizer — the browser shapes the
        // cluster with native font fallback (Hebrew niqqud, Arabic diacritics,
        // any script). The swash path below only sees fonts loaded into
        // cosmic-text and rasterizes missing scripts as .notdef boxes.
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(entry) = self.get_run_glyph(&text, attrs, num_cells.max(1), queue) {
                self.cluster_cache.insert(key, entry);
                return Some(entry);
            }
            // Canvas produced no ink (blank base + stray mark, zero-width
            // cell, …). Render the base char alone — a bare letter beats the
            // .notdef box the swash path below would produce.
            if base == ' ' {
                return None;
            }
            self.get_glyph(base, attrs, queue)
        }

        // Native: shape the full cluster via cosmic-text/swash — same pipeline
        // as try_rasterize but with multi-char text.
        #[cfg(not(target_arch = "wasm32"))]
        {
        let bold = attrs.contains(CellAttrs::BOLD);
        let italic = attrs.contains(CellAttrs::ITALIC) && !self.single_font;

        let family = match self.custom_family {
            Some(ref name) => Family::Name(name),
            None => Family::Monospace,
        };
        let font_attrs = Attrs::new()
            .family(family)
            .weight(if bold { Weight::BOLD } else { self.base_weight })
            .style(if italic { Style::Italic } else { Style::Normal });

        self.shape_buffer.set_text(
            &mut self.font_system,
            &text,
            font_attrs,
            Shaping::Advanced,
        );
        self.shape_buffer
            .shape_until_scroll(&mut self.font_system, false);

        // For clusters, we need the first glyph's cache key for rasterization
        let cache_key = self
            .shape_buffer
            .layout_runs()
            .next()
            .and_then(|run| run.glyphs.first())
            .map(|g| g.physical((0.0, 0.0), 1.0).cache_key)?;

        // Note: WASM Canvas 2D primary rasterizer takes a single char,
        // so clusters fall through to cosmic-text/swash for full shaping.

        // Rasterize via swash
        let image = self
            .swash_cache
            .get_image_uncached(&mut self.font_system, cache_key)?;

        let is_color = matches!(image.content, SwashContent::Color);
        match image.content {
            SwashContent::Mask | SwashContent::Color => {}
            _ => return None,
        }

        let pw = image.placement.width;
        let ph = image.placement.height;

        if pw == 0 || ph == 0 {
            return Some(GlyphEntry {
                uv_pos: [0.0, 0.0],
                uv_size: [0.0, 0.0],
                offset: [0.0, 0.0],
                size: [0.0, 0.0],
                is_color: false,
            });
        }

        let (alloc, atlas_tex) = if is_color {
            (&mut self.color_allocator, &self.color_texture)
        } else {
            (&mut self.allocator, &self.texture)
        };

        let (ax, ay) = alloc.alloc(pw, ph)?;
        let bpp = if is_color { 4 } else { 1 };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: atlas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: ax, y: ay, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &image.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pw * bpp),
                rows_per_image: Some(ph),
            },
            wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
        );

        let atlas_f = ATLAS_SIZE as f32;
        // Center the cluster glyph within the monospace cell
        let offset_x = image.placement.left as f32;
        let offset_y = self.metrics.baseline_y - image.placement.top as f32;
        let entry = GlyphEntry {
            uv_pos: [ax as f32 / atlas_f, ay as f32 / atlas_f],
            uv_size: [pw as f32 / atlas_f, ph as f32 / atlas_f],
            offset: [offset_x, offset_y],
            size: [pw as f32, ph as f32],
            is_color,
        };

        self.cluster_cache.insert(key, entry);
        Some(entry)
        }
    }

    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Override the font family name used for text shaping.
    /// Call after construction to explicitly set the family (e.g. "Menlo")
    /// instead of relying on automatic detection from fontdb.
    /// Clears the glyph cache since all glyphs need re-rasterization with the new family.
    pub fn set_custom_family(&mut self, name: String) {
        eprintln!("[GlyphAtlas] set_custom_family: '{}' (was {:?})", name, self.custom_family);
        self.custom_family = Some(name);
        self.cache.clear();
        self.cluster_cache.clear();
    }

    /// Get the current custom family name, if set.
    pub fn custom_family(&self) -> Option<&str> {
        self.custom_family.as_deref()
    }

    /// Override cell metrics (cell_width, cell_height, baseline_y) with host-computed values.
    /// Use this when the host knows the exact font metrics (e.g. from Canvas 2D measureText)
    /// and cosmic-text's metrics don't match. Clears the glyph cache.
    pub fn set_metrics(&mut self, metrics: CellMetrics) {
        self.metrics = metrics;
        self.cache.clear();
        self.cluster_cache.clear();
    }

    /// Set the base font weight for non-bold text.
    /// This clears the glyph cache since all glyphs need re-rasterization.
    pub fn set_base_weight(&mut self, weight: u16) {
        let w = Weight(weight);
        if self.base_weight != w {
            self.base_weight = w;
            self.cache.clear();
        self.cluster_cache.clear();
        }
    }

    /// Set the Canvas 2D fallback rasterizer for glyphs not in loaded fonts.
    /// The closure takes (char, font_size) and returns RGBA pixel data.
    #[cfg(target_arch = "wasm32")]
    pub fn set_fallback_rasterizer(
        &mut self,
        rasterizer: Option<FallbackRasterizer>,
    ) {
        self.fallback_rasterizer = rasterizer;
    }

    /// Set the Canvas 2D primary rasterizer for ALL monochrome glyphs.
    /// Uses the browser's native font renderer for pixel-identical results vs VS Code.
    /// The closure takes (char, font_size, bold, italic) and returns alpha-only data.
    /// When set, this is called BEFORE swash for every non-emoji character.
    #[cfg(target_arch = "wasm32")]
    pub fn set_primary_rasterizer(
        &mut self,
        rasterizer: Option<PrimaryRasterizer>,
    ) {
        self.primary_rasterizer = rasterizer;
        // Clear cache since all glyphs will be re-rasterized with the new rasterizer
        self.cache.clear();
        self.cluster_cache.clear();
    }

    /// Set the Canvas 2D run rasterizer for multi-character text runs.
    /// The closure takes (text, font_size, bold, italic, num_cells) and returns
    /// a wide MonoGlyph spanning the full run width with natural proportional spacing.
    #[cfg(target_arch = "wasm32")]
    pub fn set_run_rasterizer(
        &mut self,
        rasterizer: Option<RunRasterizer>,
    ) {
        self.run_rasterizer = rasterizer;
        self.run_cache.clear();
    }

    /// Rasterize a text run (e.g. Hebrew word) as a single wide glyph.
    /// Returns a GlyphEntry spanning `num_cells` cells, or None if no run rasterizer is set.
    #[cfg(target_arch = "wasm32")]
    pub fn get_run_glyph(
        &mut self,
        text: &str,
        attrs: CellAttrs,
        num_cells: usize,
        queue: &wgpu::Queue,
    ) -> Option<GlyphEntry> {
        let flags = attrs.bits() & (CellAttrs::BOLD.bits() | CellAttrs::ITALIC.bits());
        let key = (text.to_string(), flags);

        if let Some(entry) = self.run_cache.get(&key) {
            return Some(*entry);
        }

        let rasterizer = self.run_rasterizer.as_ref()?;
        let bold = attrs.contains(CellAttrs::BOLD);
        let italic = attrs.contains(CellAttrs::ITALIC);
        let glyph = rasterizer(text, self.font_size, bold, italic, num_cells as u32)?;

        // Upload to mask atlas
        let (ax, ay) = self.allocator.alloc(glyph.width, glyph.height)?;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: ax, y: ay, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &glyph.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(glyph.width),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: glyph.width,
                height: glyph.height,
                depth_or_array_layers: 1,
            },
        );

        let atlas_w = ATLAS_SIZE as f32;
        let atlas_h = ATLAS_SIZE as f32;

        let entry = GlyphEntry {
            uv_pos: [ax as f32 / atlas_w, ay as f32 / atlas_h],
            uv_size: [glyph.width as f32 / atlas_w, glyph.height as f32 / atlas_h],
            offset: [glyph.offset_x, glyph.offset_y],
            size: [glyph.width as f32, glyph.height as f32],
            is_color: false,
        };
        self.run_cache.insert(key, entry);
        Some(entry)
    }
}

/// Measure cell dimensions by shaping reference characters.
fn measure_cell(
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    buffer: &mut Buffer,
    font_size: f32,
    line_height: f32,
    custom_family: Option<&str>,
) -> CellMetrics {
    let family = match custom_family {
        Some(name) => Family::Name(name),
        None => Family::Monospace,
    };
    let attrs = Attrs::new().family(family);
    buffer.set_text(font_system, "M", attrs, Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);

    let cell_width = buffer
        .layout_runs()
        .next()
        .and_then(|run| run.glyphs.first())
        .map(|g| g.w)
        .unwrap_or(font_size * 0.6);

    // Get font ascent by rasterizing "M" — placement.top ≈ font ascent
    let font_ascent = buffer
        .layout_runs()
        .next()
        .and_then(|run| run.glyphs.first())
        .and_then(|g| {
            let phys = g.physical((0.0, 0.0), 1.0);
            swash_cache.get_image_uncached(font_system, phys.cache_key)
        })
        .map(|img| img.placement.top as f32)
        .unwrap_or(font_size * 0.8);

    // Center the text vertically within the cell. The line_height is larger
    // than the font em-square, so we distribute the extra space evenly
    // above and below the text. This aligns the cursor block with the text.
    let font_em_height = font_size; // ascent + descent ≈ font_size
    let center_pad = (line_height - font_em_height) / 2.0;
    let baseline_y = font_ascent + center_pad;

    CellMetrics {
        cell_width,
        cell_height: line_height,
        baseline_y,
    }
}
