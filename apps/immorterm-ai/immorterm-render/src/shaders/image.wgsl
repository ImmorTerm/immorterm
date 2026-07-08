// Image quad shader — renders Kitty protocol images as textured quads.
// Each image gets its own RGBA texture, sampled directly (no tint).

struct Uniforms {
    projection: mat4x4<f32>,
    cell_size: vec2<f32>,
    time: f32,
    font_thicken: f32,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var image_texture: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct ImageInstance {
    // Pixel position (x, y) — top-left corner
    @location(0) pos: vec2<f32>,
    // Pixel size (width, height)
    @location(1) size: vec2<f32>,
    // UV region origin (for cropping/atlas — usually 0,0)
    @location(2) uv_pos: vec2<f32>,
    // UV region extent (for cropping/atlas — usually 1,1)
    @location(3) uv_size: vec2<f32>,
    // Opacity (for fade-in/out animations)
    @location(4) opacity: f32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) opacity: f32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    instance: ImageInstance,
) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0),
    );

    let p = corners[vi];
    let pixel_pos = instance.pos + p * instance.size;

    var out: VertexOutput;
    out.position = uniforms.projection * vec4(pixel_pos, 0.0, 1.0);
    out.uv = instance.uv_pos + p * instance.uv_size;
    out.opacity = instance.opacity;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSampleLevel(image_texture, image_sampler, in.uv, 0.0);
    return vec4(color.rgb, color.a * in.opacity);
}
