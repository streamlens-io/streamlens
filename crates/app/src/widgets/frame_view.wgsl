// Draws the video frame: fullscreen-quad sampling planar YUV textures,
// converting to RGB. `transform` maps image space into the widget viewport:
// scale/offset are in normalized device coordinates of the widget bounds.

struct Uniforms {
    // xy: scale, zw: offset (NDC)
    transform: vec4<f32>,
    // x: matrix selector (0 = BT.601, 1 = BT.709, 2 = BT.2020)
    // y: sample scale (1.0 for 8-bit, 64.06 for 10-bit in R16Unorm)
    // z: channel mode (0 = color, 1 = Y, 2 = U, 3 = V)
    params: vec4<f32>,
}

@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var u_tex: texture_2d<f32>;
@group(0) @binding(2) var v_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // Two-triangle quad from vertex index.
    let corner = vec2<f32>(
        f32(index & 1u),
        f32((index >> 1u) & 1u),
    );
    var out: VertexOutput;
    let pos = corner * uniforms.transform.xy + uniforms.transform.zw;
    // corner (0,0) = image top-left; NDC y points up.
    out.position = vec4<f32>(pos.x * 2.0 - 1.0, 1.0 - pos.y * 2.0, 0.0, 1.0);
    out.uv = corner;
    return out;
}

fn yuv_to_rgb(yuv: vec3<f32>, selector: f32) -> vec3<f32> {
    let y = yuv.x;
    let u = yuv.y - 0.5;
    let v = yuv.z - 0.5;
    // Limited-range coefficients.
    let yl = (y - 16.0 / 255.0) * (255.0 / 219.0);
    let uc = u * (255.0 / 224.0);
    let vc = v * (255.0 / 224.0);
    if selector < 0.5 { // BT.601
        return vec3<f32>(
            yl + 1.402 * vc,
            yl - 0.344136 * uc - 0.714136 * vc,
            yl + 1.772 * uc,
        );
    } else if selector < 1.5 { // BT.709
        return vec3<f32>(
            yl + 1.5748 * vc,
            yl - 0.1873 * uc - 0.4681 * vc,
            yl + 1.8556 * uc,
        );
    }
    // BT.2020
    return vec3<f32>(
        yl + 1.4746 * vc,
        yl - 0.16455 * uc - 0.57135 * vc,
        yl + 1.8814 * uc,
    );
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let scale = uniforms.params.y;
    let y = textureSample(y_tex, samp, in.uv).r * scale;
    let u = textureSample(u_tex, samp, in.uv).r * scale;
    let v = textureSample(v_tex, samp, in.uv).r * scale;
    let mode = uniforms.params.z;
    if mode > 0.5 && mode < 1.5 {
        return vec4<f32>(y, y, y, 1.0);
    } else if mode < 2.5 && mode > 1.5 {
        return vec4<f32>(u, u, u, 1.0);
    } else if mode > 2.5 {
        return vec4<f32>(v, v, v, 1.0);
    }
    let rgb = yuv_to_rgb(vec3<f32>(y, u, v), uniforms.params.x);
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
