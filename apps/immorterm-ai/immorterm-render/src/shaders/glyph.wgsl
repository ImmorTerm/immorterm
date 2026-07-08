// Glyph atlas shader — samples from monochrome mask OR color atlas based on is_color flag.

struct Uniforms {
    projection: mat4x4<f32>,
    cell_size: vec2<f32>,
    time: f32,
    font_thicken: f32,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var atlas_texture: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;
@group(1) @binding(2) var color_atlas: texture_2d<f32>;

struct GlyphInstance {
    @location(0) pos: vec2<f32>,     // pixel position of glyph
    @location(1) size: vec2<f32>,    // pixel size of glyph bitmap
    @location(2) uv_pos: vec2<f32>,  // atlas UV origin (normalized)
    @location(3) uv_size: vec2<f32>, // atlas UV extent (normalized)
    @location(4) color: vec4<f32>,   // foreground RGBA
    @location(5) is_color: f32,      // 0.0 = mono mask, 1.0 = color atlas
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) is_color: f32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    instance: GlyphInstance,
) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0),
    );

    let p = corners[vi];
    let glyph_pos = instance.pos + p * instance.size;
    let pixel_pos = glyph_pos;

    var out: VertexOutput;
    out.position = uniforms.projection * vec4(pixel_pos, 0.0, 1.0);
    out.uv = instance.uv_pos + p * instance.uv_size;
    out.color = instance.color;
    out.is_color = instance.is_color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (in.is_color > 0.5) {
        // Color glyph (emoji) — sample RGBA directly from color atlas.
        // textureSampleLevel (explicit LOD=0) avoids the uniform-control-flow
        // requirement of textureSample, which computes implicit derivatives.
        // Our atlases have mip_level_count=1, so level 0 is the only level.
        return textureSampleLevel(color_atlas, atlas_sampler, in.uv, 0.0);
    } else {
        // Monochrome glyph — sample alpha from mask atlas, tint with foreground color.
        // Power curve: pow(alpha, 1/font_thicken) boosts edge alphas to approximate
        // the visual weight of subpixel AA (which VS Code/Chromium uses).
        // Unlike smoothstep, the power curve has no hard cutoffs — counter openings
        // in "e", "a", "s" stay proportionally faint, and letter shapes are preserved.
        let raw_alpha = textureSampleLevel(atlas_texture, atlas_sampler, in.uv, 0.0).r;
        let alpha = pow(raw_alpha, 1.0 / uniforms.font_thicken);
        return vec4(in.color.rgb, in.color.a * alpha);
    }
}
