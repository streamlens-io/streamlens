// Instanced digit sprites drawn over the video (per-block QP / bit values).
// The atlas is an 11-glyph strip of 5x7 bitmaps ('0'-'9', '-') generated at
// pipeline creation. Glyphs hide below a readability zoom threshold.

struct Uniforms {
    transform: vec4<f32>, // xy: scale (image px → widget 0..1), zw: offset
    // x: stroke width scale (1/zoom), y: zoom, zw unused
    params: vec4<f32>,
}

struct Instance {
    @location(0) pos: vec2<f32>,   // top-left, image pixels
    @location(1) size: vec2<f32>,  // image pixels
    @location(2) glyph: f32,       // 0-9 digits, 10 = '-'
    @location(3) _pad: f32,
    @location(4) color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_samp: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

const GLYPH_COUNT: f32 = 11.0;
// Hide text when a 16px block is narrower than ~40 screen px.
const MIN_ZOOM_FOR_TEXT: f32 = 2.5;

@vertex
fn vs_main(@builtin(vertex_index) index: u32, instance: Instance) -> VertexOutput {
    var out: VertexOutput;
    let corner = vec2<f32>(f32(index & 1u), f32((index >> 1u) & 1u));

    if uniforms.params.y < MIN_ZOOM_FOR_TEXT {
        out.position = vec4<f32>(0.0, 0.0, 0.0, 1.0); // degenerate: culled
        out.uv = vec2<f32>(0.0);
        out.color = vec4<f32>(0.0);
        return out;
    }

    let px = instance.pos + corner * instance.size;
    let pos = px * uniforms.transform.xy + uniforms.transform.zw;
    out.position = vec4<f32>(pos.x * 2.0 - 1.0, 1.0 - pos.y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>((instance.glyph + corner.x) / GLYPH_COUNT, corner.y);
    out.color = instance.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let alpha = textureSample(atlas, atlas_samp, in.uv).r;
    if alpha < 0.5 {
        discard;
    }
    return in.color;
}
