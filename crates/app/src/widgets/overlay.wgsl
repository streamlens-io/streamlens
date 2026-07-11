// Instanced overlay rendering on top of the video frame. Every primitive is
// a "segment" — a quad oriented along p0→p1 with a thickness:
//   * filled rect = horizontal segment through its center, thickness = height
//   * grid line / partition edge = thin axis-aligned segment
//   * motion vector = thin angled segment
//
// Instance coordinates are in image pixel space; the same transform as the
// frame quad maps them to the widget viewport.

struct Uniforms {
    transform: vec4<f32>, // xy: scale (image px → widget 0..1), zw: offset
    // x: stroke width scale (1/zoom, clamped); y: zoom; zw unused
    params: vec4<f32>,
}

struct Instance {
    @location(0) seg: vec4<f32>,   // p0.xy, p1.xy in image pixels
    @location(1) width: f32,       // thickness in image pixels
    @location(2) stroke: f32,      // 1 = screen-space stroke, 0 = image-space fill
    @location(3) color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32, instance: Instance) -> VertexOutput {
    let p0 = instance.seg.xy;
    let p1 = instance.seg.zw;
    let dir = p1 - p0;
    let len = max(length(dir), 1e-6);
    let d = dir / len;
    // Strokes scale with 1/zoom to stay ~1px on screen; fills keep their
    // image-space thickness so they cover their block at any zoom.
    let wscale = mix(1.0, uniforms.params.x, instance.stroke);
    var half_w = instance.width * wscale * 0.5;
    var alpha = 1.0;

    // Never let a stroke go sub-pixel: the rasterizer would hit or miss
    // pixels pseudo-randomly (moiré grids, dashed lines). Widen to exactly
    // one screen pixel and fade by the lost coverage instead.
    if (instance.stroke > 0.5) {
        let screen_w = 2.0 * half_w * uniforms.params.y; // params.y = zoom
        if (screen_w < 1.0) {
            half_w = 0.5 / uniforms.params.y;
            alpha = screen_w;
        }
    }
    let n = vec2<f32>(-d.y, d.x) * half_w;

    // corner.x selects p0/p1, corner.y selects -n/+n
    let corner = vec2<f32>(f32(index & 1u), f32((index >> 1u) & 1u));
    let px = mix(p0, p1, corner.x) + mix(-n, n, corner.y);
    let pos = px * uniforms.transform.xy + uniforms.transform.zw;

    var out: VertexOutput;
    out.position = vec4<f32>(pos.x * 2.0 - 1.0, 1.0 - pos.y * 2.0, 0.0, 1.0);
    out.color = vec4<f32>(instance.color.rgb, instance.color.a * alpha);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
