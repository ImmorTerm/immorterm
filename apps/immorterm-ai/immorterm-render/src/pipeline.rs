//! wgpu render pipelines and GPU-side data structures.
//!
//! Defines the instance buffer layouts and creates the background + glyph
//! render pipelines. All rendering uses instanced quads (6 vertices per quad).

use bytemuck::{Pod, Zeroable};

/// Shared uniforms for all pipelines.
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct Uniforms {
    /// Orthographic projection matrix (column-major)
    pub projection: [[f32; 4]; 4],
    /// Cell dimensions in pixels (width, height)
    pub cell_size: [f32; 2],
    /// Time in seconds since start (for animations)
    pub time: f32,
    /// Gamma exponent for glyph alpha thickening (< 1.0 = bolder text).
    /// Compensates for grayscale AA looking thinner than Chromium's subpixel AA.
    /// Default: 0.75. Set to 1.0 for no thickening.
    pub font_thicken: f32,
}

impl Uniforms {
    /// Create an orthographic projection for pixel-coordinate rendering.
    /// Maps (0,0)→top-left, (w,h)→bottom-right.
    pub fn ortho(width: f32, height: f32, cell_w: f32, cell_h: f32, time: f32, font_thicken: f32) -> Self {
        Self {
            projection: [
                [2.0 / width, 0.0, 0.0, 0.0],
                [0.0, -2.0 / height, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0, 1.0],
            ],
            cell_size: [cell_w, cell_h],
            time,
            font_thicken,
        }
    }

    /// Create an orthographic projection offset to a sub-region of the surface.
    ///
    /// Used for multi-pane rendering: each pane gets its own projection that
    /// maps (0,0)→pane top-left. Combined with a scissor rect, all 4 existing
    /// pipelines (bg, glyph, decor, image) render into the pane automatically
    /// with zero per-instance CPU work.
    ///
    /// `pane_w`/`pane_h`: pixel dimensions of the pane content area.
    /// `offset_x`/`offset_y`: pixel offset of the pane within the surface.
    /// `surface_w`/`surface_h`: total surface dimensions (for NDC mapping).
    #[allow(clippy::too_many_arguments)]
    pub fn ortho_offset(
        _pane_w: f32,
        _pane_h: f32,
        offset_x: f32,
        offset_y: f32,
        surface_w: f32,
        surface_h: f32,
        cell_w: f32,
        cell_h: f32,
        time: f32,
        font_thicken: f32,
    ) -> Self {
        // Map pane-local pixel coords directly to surface NDC:
        //   ndc_x = pixel_x * (2/surface_w) + (2*offset_x/surface_w - 1)
        //   ndc_y = pixel_y * (-2/surface_h) + (1 - 2*offset_y/surface_h)
        let sx = 2.0 / surface_w;
        let sy = -2.0 / surface_h;
        let tx = 2.0 * offset_x / surface_w - 1.0;
        let ty = 1.0 - 2.0 * offset_y / surface_h;

        Self {
            projection: [
                [sx, 0.0, 0.0, 0.0],
                [0.0, sy, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [tx, ty, 0.0, 1.0],
            ],
            cell_size: [cell_w, cell_h],
            time,
            font_thicken,
        }
    }
}

/// Per-instance data for background colored quads.
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct BgInstance {
    /// Cell grid position (col, row)
    pub pos: [f32; 2],
    /// Background RGBA color
    pub color: [f32; 4],
}

/// Per-instance data for glyph quads.
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct GlyphInstance {
    /// Pixel position of glyph bitmap
    pub pos: [f32; 2],
    /// Pixel size of glyph bitmap
    pub size: [f32; 2],
    /// Atlas UV origin (normalized 0..1)
    pub uv_pos: [f32; 2],
    /// Atlas UV extent (normalized 0..1)
    pub uv_size: [f32; 2],
    /// Foreground RGBA color
    pub color: [f32; 4],
    /// 0.0 = monochrome mask atlas, 1.0 = RGBA color atlas (emoji)
    pub is_color: f32,
}

/// Per-instance data for decorations (underlines, strikethrough, cursor shapes).
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct DecorInstance {
    /// Pixel position (x, y)
    pub pos: [f32; 2],
    /// Pixel size (width, height)
    pub size: [f32; 2],
    /// RGBA color
    pub color: [f32; 4],
    /// x = style (0=solid, 1=double, 2=curly, 3=dotted, 4=dashed), y = phase (breathing)
    pub extra: [f32; 2],
    pub _pad: [f32; 2],
}

/// Decoration styles for the `extra.x` field of DecorInstance.
pub mod decor_style {
    pub const SOLID: f32 = 0.0;
    pub const DOUBLE: f32 = 1.0;
    pub const CURLY: f32 = 2.0;
    pub const DOTTED: f32 = 3.0;
    pub const DASHED: f32 = 4.0;
}

/// Maximum cells we pre-allocate instance buffers for (300 cols × 120 rows).
pub const MAX_CELLS: u64 = 300 * 120;

/// All render pipelines and shared GPU resources.
pub struct TextPipeline {
    pub bg_pipeline: wgpu::RenderPipeline,
    pub glyph_pipeline: wgpu::RenderPipeline,
    pub decor_pipeline: wgpu::RenderPipeline,
    pub uniform_buffer: wgpu::Buffer,
    pub uniform_bind_group: wgpu::BindGroup,
    pub uniform_bind_group_layout: wgpu::BindGroupLayout,
    pub bg_buffer: wgpu::Buffer,
    pub glyph_buffer: wgpu::Buffer,
    pub decor_buffer: wgpu::Buffer,
}

impl TextPipeline {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        atlas_bind_group_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        // Uniform bind group layout (shared by both pipelines)
        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("uniform_bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        // Uniform buffer
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniform_bg"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // Instance buffers (pre-allocated)
        let bg_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bg_instances"),
            size: MAX_CELLS * std::mem::size_of::<BgInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let glyph_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glyph_instances"),
            size: MAX_CELLS * std::mem::size_of::<GlyphInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let decor_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("decor_instances"),
            size: MAX_CELLS * std::mem::size_of::<DecorInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ── Background pipeline ──
        let bg_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("bg_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/bg.wgsl").into()),
        });

        let bg_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bg_layout"),
            bind_group_layouts: &[&uniform_bind_group_layout],
            push_constant_ranges: &[],
        });

        // Blend translucent overlays (selection) over solid cell bg while keeping
        // framebuffer alpha = 1.0 — same scheme as glyph pipeline so HTML page bg
        // can't bleed through. Lets selection_color (alpha 0.65) tint over cell bg
        // instead of being fully overwritten.
        let bg_blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Max,
            },
        };

        let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("bg_pipeline"),
            layout: Some(&bg_layout),
            vertex: wgpu::VertexState {
                module: &bg_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<BgInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, // pos
                        1 => Float32x4, // color
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &bg_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(bg_blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Glyph pipeline (alpha-blended) ──
        let glyph_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glyph_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/glyph.wgsl").into()),
        });

        let glyph_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glyph_layout"),
            bind_group_layouts: &[&uniform_bind_group_layout, atlas_bind_group_layout],
            push_constant_ranges: &[],
        });

        // Alpha blend that keeps framebuffer alpha at 1.0 (fully opaque).
        // Glyph anti-aliasing writes sub-1.0 alpha fragments — with PreMultiplied
        // compositing, those would let the HTML page background bleed through,
        // causing dark lines between rows. Fix: blend color normally but force
        // alpha = max(src, dst) which keeps it at 1.0 after the opaque clear.
        let opaque_alpha_blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let glyph_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glyph_pipeline"),
            layout: Some(&glyph_layout),
            vertex: wgpu::VertexState {
                module: &glyph_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, // pos
                        1 => Float32x2, // size
                        2 => Float32x2, // uv_pos
                        3 => Float32x2, // uv_size
                        4 => Float32x4, // color
                        5 => Float32,   // is_color
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &glyph_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(opaque_alpha_blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Decoration pipeline (alpha-blended, same as glyph) ──
        let decor_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("decor_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/decor.wgsl").into()),
        });

        let decor_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("decor_layout"),
            bind_group_layouts: &[&uniform_bind_group_layout],
            push_constant_ranges: &[],
        });

        let decor_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("decor_pipeline"),
            layout: Some(&decor_layout),
            vertex: wgpu::VertexState {
                module: &decor_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<DecorInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, // pos
                        1 => Float32x2, // size
                        2 => Float32x4, // color
                        3 => Float32x2, // extra (style, phase)
                        4 => Float32x2, // _pad
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &decor_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(opaque_alpha_blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            bg_pipeline,
            glyph_pipeline,
            decor_pipeline,
            uniform_buffer,
            uniform_bind_group,
            uniform_bind_group_layout,
            bg_buffer,
            glyph_buffer,
            decor_buffer,
        }
    }
}
