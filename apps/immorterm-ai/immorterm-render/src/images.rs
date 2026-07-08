//! ImageRenderer — GPU texture management for Kitty protocol images.
//!
//! Each image gets its own wgpu texture + bind group. Images are uploaded
//! on first sight and freed when the core layer deletes them.

use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;

/// Per-instance data for image quads.
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct ImageInstance {
    /// Pixel position (x, y)
    pub pos: [f32; 2],
    /// Pixel size (width, height)
    pub size: [f32; 2],
    /// UV origin (usually [0, 0])
    pub uv_pos: [f32; 2],
    /// UV extent (usually [1, 1])
    pub uv_size: [f32; 2],
    /// Opacity (0.0..1.0)
    pub opacity: f32,
    pub _pad: [f32; 3],
}

/// A single uploaded GPU image.
struct GpuImage {
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// Manages GPU textures for terminal images.
pub struct ImageRenderer {
    /// Uploaded images by ID
    images: HashMap<u32, GpuImage>,
    /// Render pipeline for image quads
    pipeline: wgpu::RenderPipeline,
    /// Instance buffer (shared across all images per frame)
    instance_buffer: wgpu::Buffer,
    /// Bind group layout for per-image texture
    texture_bind_group_layout: wgpu::BindGroupLayout,
    /// Shared sampler (bilinear filtering)
    sampler: wgpu::Sampler,
}

impl ImageRenderer {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        uniform_bind_group_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        // Per-image texture bind group layout (group 1)
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("image_texture_bgl"),
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
                ],
            });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image_sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Image pipeline
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/image.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image_pipeline_layout"),
            bind_group_layouts: &[uniform_bind_group_layout, &texture_bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ImageInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, // pos
                        1 => Float32x2, // size
                        2 => Float32x2, // uv_pos
                        3 => Float32x2, // uv_size
                        4 => Float32,   // opacity
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent::OVER,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image_instances"),
            // Support up to 64 simultaneous images
            size: 64 * std::mem::size_of::<ImageInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            images: HashMap::new(),
            pipeline,
            instance_buffer,
            texture_bind_group_layout,
            sampler,
        }
    }

    /// Upload an image to the GPU. Returns true if newly uploaded, false if already present.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: u32,
        rgba_data: &[u8],
        width: u32,
        height: u32,
    ) -> bool {
        if self.images.contains_key(&id) {
            return false;
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("image_{}", id)),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("image_bg_{}", id)),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        self.images.insert(
            id,
            GpuImage {
                _texture: texture,
                bind_group,
            },
        );

        true
    }

    /// Remove an image's GPU resources.
    pub fn remove(&mut self, id: u32) {
        self.images.remove(&id);
    }

    /// Check if an image is already uploaded.
    pub fn has_image(&self, id: u32) -> bool {
        self.images.contains_key(&id)
    }

    /// Remove GPU textures for images not in the active set.
    pub fn retain(&mut self, active_ids: &[u32]) {
        self.images.retain(|id, _| active_ids.contains(id));
    }

    /// Render all visible images into the current render pass.
    ///
    /// Each image needs its own draw call (different bind group per texture).
    /// Images are sorted by z_index for proper layering.
    pub fn render<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        queue: &wgpu::Queue,
        uniform_bind_group: &'a wgpu::BindGroup,
        instances: &[ImageInstance],
        image_ids: &[u32],
    ) {
        if instances.is_empty() {
            return;
        }

        // Upload all instances
        queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(instances),
        );

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, uniform_bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));

        // Draw each image with its own bind group
        for (idx, &id) in image_ids.iter().enumerate() {
            if let Some(gpu_image) = self.images.get(&id) {
                pass.set_bind_group(1, &gpu_image.bind_group, &[]);
                pass.draw(0..6, idx as u32..idx as u32 + 1);
            }
        }
    }
}
