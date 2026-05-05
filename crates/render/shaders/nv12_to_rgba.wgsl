// NV12 to RGBA fragment shader with BT.709 limited range color conversion
// and letterboxing support.

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Fullscreen triangle covering clip space.
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    // UVs for the fullscreen triangle.
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );

    var out: VertexOutput;
    out.clip_position = vec4<f32>(pos[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

// Uniforms for letterbox and aspect ratio correction.
struct Uniforms {
    // Scale factor for UV coordinates to maintain aspect ratio.
    // If video is narrower than window: scale_x < 1.0, scale_y = 1.0
    // If video is wider than window: scale_x = 1.0, scale_y < 1.0
    uv_scale: vec2<f32>,
    // Offset to center the video (black bars).
    uv_offset: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(0) @binding(1)
var y_sampler: sampler;

@group(0) @binding(2)
var y_texture: texture_2d<f32>;

@group(0) @binding(3)
var uv_sampler: sampler;

@group(0) @binding(4)
var uv_texture: texture_2d<f32>;

/// BT.709 limited range YUV to RGB conversion.
/// Input: Y in [0.0, 1.0], UV in [0.0, 1.0] (UV is already centered around 0.5 in texture).
fn nv12_to_rgb(y: f32, u: f32, v: f32) -> vec3<f32> {
    // Limited range: Y [16..235], UV [16..240] mapped to [0..1].
    let y_scaled = (y - 0.0625) * 1.16438356;
    let u_scaled = u - 0.5;
    let v_scaled = v - 0.5;

    // BT.709 coefficients.
    let r = y_scaled + 1.7927411 * v_scaled;
    let g = y_scaled - 0.53290933 * v_scaled - 0.21324861 * u_scaled;
    let b = y_scaled + 2.11240179 * u_scaled;

    return vec3<f32>(
        clamp(r, 0.0, 1.0),
        clamp(g, 0.0, 1.0),
        clamp(b, 0.0, 1.0),
    );
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Apply letterbox scaling.
    let scaled_uv = in.uv * uniforms.uv_scale + uniforms.uv_offset;

    // Check if we're in the black bar area.
    if (scaled_uv.x < 0.0 || scaled_uv.x > 1.0 ||
        scaled_uv.y < 0.0 || scaled_uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0); // Black letterbox.
    }

    let y = textureSample(y_texture, y_sampler, scaled_uv).r;
    let uv = textureSample(uv_texture, uv_sampler, scaled_uv).rg;

    // NV12 stores chroma as U then V; Rg8Unorm exposes them as .r then .g.
    let rgb = nv12_to_rgb(y, uv.r, uv.g);
    return vec4<f32>(rgb, 1.0);
}
