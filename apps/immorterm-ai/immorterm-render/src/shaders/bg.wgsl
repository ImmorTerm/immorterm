// Background quad shader — instanced colored rectangles for cell backgrounds.

struct Uniforms {
    projection: mat4x4<f32>,
    cell_size: vec2<f32>,
    time: f32,
    font_thicken: f32,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct BgInstance {
    @location(0) pos: vec2<f32>,    // cell grid position (col, row)
    @location(1) color: vec4<f32>,  // background RGBA
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    instance: BgInstance,
) -> VertexOutput {
    // 6 vertices for a quad (two triangles)
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0),
    );

    let p = corners[vi];
    let pixel_pos = (instance.pos + p) * uniforms.cell_size;

    var out: VertexOutput;
    out.position = uniforms.projection * vec4(pixel_pos, 0.0, 1.0);
    out.color = instance.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
