// Fullscreen quad — draws VRAM texture to screen
// Uses vertex_index to generate a fullscreen triangle pair (no vertex buffer needed)

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    // Generate fullscreen quad from 6 vertices (2 triangles)
    var positions = array<vec2f, 6>(
        vec2f(-1.0, -1.0), vec2f( 1.0, -1.0), vec2f(-1.0,  1.0),
        vec2f(-1.0,  1.0), vec2f( 1.0, -1.0), vec2f( 1.0,  1.0),
    );
    var uvs = array<vec2f, 6>(
        vec2f(0.0, 1.0), vec2f(1.0, 1.0), vec2f(0.0, 0.0),
        vec2f(0.0, 0.0), vec2f(1.0, 1.0), vec2f(1.0, 0.0),
    );

    var out: VertexOutput;
    out.position = vec4f(positions[idx], 0.0, 1.0);
    out.uv = uvs[idx];
    return out;
}

@group(0) @binding(0) var vram_tex: texture_2d<f32>;
@group(0) @binding(1) var vram_sampler: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4f {
    return textureSample(vram_tex, vram_sampler, in.uv);
}
