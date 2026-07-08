//! Headless GPU screenshot — renders a Terminal to PNG without a window.
//!
//! Uses the same `TerminalRenderer` as the GUI window, but renders into an
//! offscreen wgpu texture and reads the pixels back to CPU for PNG encoding.
//! No surface/window needed — just a headless adapter + device.
//!
//! **Important**: This must run in a process with WindowServer access (macOS).
//! Double-forked daemon processes can't access Metal's shader compiler.
//! The CLI and MCP server processes call this; the daemon only provides
//! terminal state via `DumpState`.

use base64::Engine;
use immorterm_core::Terminal;
use immorterm_render::statusbar::{self, StatusBarData};
use immorterm_render::{RenderOptions, TerminalRenderer};

/// Context for status bar rendering (provided by the daemon via DumpState).
pub struct StatusBarContext {
    pub project: String,
    pub ai_stats: String,
}

/// Render a terminal to a PNG screenshot, returning (base64_png, width, height).
///
/// Must be called from a process with GPU access (not a forked daemon).
pub fn render_screenshot(
    terminal: &mut Terminal,
    include_status_bar: bool,
    status_bar_ctx: Option<&StatusBarContext>,
    custom_width: Option<u32>,
    custom_height: Option<u32>,
) -> Result<(String, u32, u32), String> {
    // 1. Create headless wgpu instance + adapter + device
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: None, // headless — no window
        force_fallback_adapter: false,
    }))
    .ok_or("No GPU adapter found — headless rendering requires Metal/Vulkan")?;

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("screenshot"),
            ..Default::default()
        },
        None,
    ))
    .map_err(|e| format!("Device request failed: {}", e))?;

    // 2. Create renderer with system fonts (None = load from system fontdb)
    // Use 2x scale factor to match Retina display quality (same as GUI window)
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let scale_factor = 2.0_f32;
    let font_size = 14.0 * scale_factor;
    let mut renderer = TerminalRenderer::new(&device, &queue, format, None, font_size);
    renderer.status_bar_enabled = include_status_bar;

    // 3. Compute pixel dimensions from terminal geometry
    let (cw, ch) = renderer.cell_metrics();
    let cols = terminal.cols();
    let rows = terminal.rows();
    let status_rows: usize = if include_status_bar { 1 } else { 0 };

    let width = custom_width.unwrap_or((cols as f32 * cw).ceil() as u32).max(1);
    let height = custom_height
        .unwrap_or(((rows + status_rows) as f32 * ch).ceil() as u32)
        .max(1);
    renderer.resize(&device, width, height);

    // 4. Create offscreen texture (RENDER_ATTACHMENT to draw, COPY_SRC to readback)
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("screenshot_texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    // 5. Build status bar data if requested
    let sb_data: Option<StatusBarData> = if include_status_bar {
        let time = renderer.start_time.elapsed().as_secs_f32();
        let dot = statusbar::animated_dot_char(time);
        let (project, ai_stats) = match status_bar_ctx {
            Some(ctx) => (ctx.project.as_str(), ctx.ai_stats.as_str()),
            None => ("ImmorTerm", ""),
        };
        Some(statusbar::build_default_sections(
            project,
            &terminal.title,
            ai_stats,
            "", // no last-active in screenshot mode
            dot,
            cols,
            0.0, // no CTX bar in screenshot mode
        ))
    } else {
        None
    };

    let opts = RenderOptions {
        scroll_offset: 0,
        selections: &[],
        pseudo_selections: &[],
        status_bar: sb_data.as_ref(),
        popup: None,
        pane: None,
        clear: true,
    };

    // 6. Render terminal into offscreen texture
    renderer.render(&device, &queue, &view, terminal, &opts);

    // 7. Copy texture → staging buffer for CPU readback
    let bytes_per_row = 4 * width; // RGBA = 4 bytes per pixel
    // wgpu requires row alignment to 256 bytes
    let padded_bytes_per_row = (bytes_per_row + 255) & !255;
    let buffer_size = (padded_bytes_per_row * height) as u64;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot_staging"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    // 8. Map the buffer and read pixels back to CPU
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).ok();
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|_| "Buffer map channel closed".to_string())?
        .map_err(|e| format!("Buffer map error: {:?}", e))?;

    let data = slice.get_mapped_range();

    // Remove row padding (padded_bytes_per_row → actual bytes_per_row)
    let mut pixels = Vec::with_capacity((bytes_per_row * height) as usize);
    for row in 0..height {
        let start = (row * padded_bytes_per_row) as usize;
        let end = start + bytes_per_row as usize;
        pixels.extend_from_slice(&data[start..end]);
    }
    drop(data);
    staging.unmap();

    // 9. Encode RGBA pixels to PNG
    let img = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(width, height, pixels)
        .ok_or("Failed to create image buffer from pixel data")?;
    let mut png_bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut png_bytes),
        image::ImageFormat::Png,
    )
    .map_err(|e| format!("PNG encode error: {}", e))?;

    // 10. Base64 encode
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);

    Ok((b64, width, height))
}
