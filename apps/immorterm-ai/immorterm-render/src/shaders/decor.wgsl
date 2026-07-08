// Decoration shader — underlines, strikethrough, cursor shapes.
//
// Renders colored rectangles with fragment-level pattern generation.
// The `style` field selects the pattern:
//   0 = solid (single underline, strikethrough, block/bar/underline cursor)
//   1 = double underline (two thin lines)
//   2 = curly underline (sine wave)
//   3 = dotted underline
//   4 = dashed underline

struct Uniforms {
    projection: mat4x4<f32>,
    cell_size: vec2<f32>,
    time: f32,
    font_thicken: f32,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct DecorInstance {
    @location(0) pos: vec2<f32>,    // pixel position (x, y)
    @location(1) size: vec2<f32>,   // pixel size (width, height)
    @location(2) color: vec4<f32>,  // RGBA color
    @location(3) extra: vec2<f32>,  // x = style (0-4), y = phase (breathing)
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local_uv: vec2<f32>,  // 0..1 across the decoration rect
    @location(2) extra: vec2<f32>,
    @location(3) pixel_size: vec2<f32>, // actual pixel dimensions for pattern scaling
};

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    instance: DecorInstance,
) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0),
    );

    let p = corners[vi];
    let pixel_pos = instance.pos + p * instance.size;

    var out: VertexOutput;
    out.position = uniforms.projection * vec4(pixel_pos, 0.0, 1.0);
    out.color = instance.color;
    out.local_uv = p;
    out.extra = instance.extra;
    out.pixel_size = instance.size;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let style = i32(in.extra.x);
    let phase = in.extra.y;
    let u = in.local_uv.x;
    let v = in.local_uv.y;

    var alpha = 1.0;

    switch style {
        // Solid — used for single underline, strikethrough, cursor shapes
        case 0 {
            alpha = 1.0;
        }

        // Double underline — two thin horizontal lines
        case 1 {
            let line_top = smoothstep(0.0, 0.15, v) * (1.0 - smoothstep(0.25, 0.4, v));
            let line_bot = smoothstep(0.6, 0.75, v) * (1.0 - smoothstep(0.85, 1.0, v));
            alpha = max(line_top, line_bot);
        }

        // Curly underline — sine wave with antialiased edges
        case 2 {
            // Scale wave frequency based on actual pixel width
            let freq = in.pixel_size.x / 6.0; // ~6px per wave cycle
            let wave_y = sin(u * freq * 6.2831853) * 0.35 + 0.5;
            let dist = abs(v - wave_y);
            alpha = 1.0 - smoothstep(0.0, 0.25, dist);
        }

        // Dotted underline — evenly spaced dots
        case 3 {
            let dot_count = in.pixel_size.x / 4.0; // ~4px per dot cycle
            let in_dot = step(fract(u * dot_count), 0.45);
            let y_band = smoothstep(0.1, 0.3, v) * (1.0 - smoothstep(0.7, 0.9, v));
            alpha = in_dot * y_band;
        }

        // Dashed underline — longer segments with gaps
        case 4 {
            let dash_count = in.pixel_size.x / 8.0; // ~8px per dash cycle
            let in_dash = step(fract(u * dash_count), 0.6);
            alpha = in_dash;
        }

        default {
            alpha = 1.0;
        }
    }

    // Apply breathing animation phase (0.0 = no animation, >0 = breathing)
    let breath = select(1.0, 0.4 + 0.6 * abs(sin(uniforms.time * 2.0 + phase)), phase > 0.0);

    return vec4(in.color.rgb, in.color.a * alpha * breath);
}
