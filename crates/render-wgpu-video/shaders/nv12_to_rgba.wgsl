// NV12 to RGBA fragment shader.
// Color range, matrix coefficients, and SDR adjustments come from uniforms.

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Fullscreen triangle covering clip space.
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );

    // UVs for the fullscreen triangle.
    var texture_coordinates = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );

    var output: VertexOutput;
    output.clip_position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.uv = texture_coordinates[vertex_index];
    return output;
}

// Uniforms for letterbox, YUV normalization, matrix conversion, and SDR settings.
struct ColorPipelineUniforms {
    // Scale factor for UV coordinates to maintain aspect ratio.
    uv_scale: vec2<f32>,
    // Offset to center the video and keep black bars outside active video.
    uv_offset: vec2<f32>,
    // First affine row converting display UV into source texture UV.
    orientation_transform_row0: vec4<f32>,
    // Second affine row converting display UV into source texture UV.
    orientation_transform_row1: vec4<f32>,
    // x: Y offset, y: Y scale, z: U offset, w: V offset.
    yuv_range: vec4<f32>,
    // x: U scale, y: V scale, z/w: reserved.
    chroma_scale: vec4<f32>,
    // Matrix rows for RGB reconstruction from normalized YUV.
    yuv_to_rgb_row0: vec4<f32>,
    yuv_to_rgb_row1: vec4<f32>,
    yuv_to_rgb_row2: vec4<f32>,
    // RGB luma weights used by saturation adjustment.
    saturation_luma_weights: vec4<f32>,
    // x: brightness, y: contrast, z: saturation, w: exposure.
    color_adjustment: vec4<f32>,
    // Per-channel gain and offset; w is reserved.
    rgb_gain: vec4<f32>,
    rgb_offset: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: ColorPipelineUniforms;

@group(0) @binding(1)
var y_sampler: sampler;

@group(0) @binding(2)
var y_texture: texture_2d<f32>;

@group(0) @binding(3)
var uv_sampler: sampler;

@group(0) @binding(4)
var uv_texture: texture_2d<f32>;

fn normalize_yuv_sample(sampled_y: f32, sampled_u: f32, sampled_v: f32) -> vec3<f32> {
    let normalized_y = (sampled_y - uniforms.yuv_range.x) * uniforms.yuv_range.y;
    let normalized_u = (sampled_u - uniforms.yuv_range.z) * uniforms.chroma_scale.x;
    let normalized_v = (sampled_v - uniforms.yuv_range.w) * uniforms.chroma_scale.y;

    return vec3<f32>(normalized_y, normalized_u, normalized_v);
}

fn convert_yuv_to_rgb(normalized_yuv: vec3<f32>) -> vec3<f32> {
    let red = dot(uniforms.yuv_to_rgb_row0.xyz, normalized_yuv) + uniforms.yuv_to_rgb_row0.w;
    let green = dot(uniforms.yuv_to_rgb_row1.xyz, normalized_yuv) + uniforms.yuv_to_rgb_row1.w;
    let blue = dot(uniforms.yuv_to_rgb_row2.xyz, normalized_yuv) + uniforms.yuv_to_rgb_row2.w;

    return vec3<f32>(red, green, blue);
}

fn apply_sdr_adjustments(rgb: vec3<f32>) -> vec3<f32> {
    let brightness = uniforms.color_adjustment.x;
    let contrast = uniforms.color_adjustment.y;
    let saturation = uniforms.color_adjustment.z;
    let exposure = uniforms.color_adjustment.w;
    let luma = dot(rgb, uniforms.saturation_luma_weights.xyz);
    let saturated_rgb = vec3<f32>(luma) + (rgb - vec3<f32>(luma)) * saturation;
    let contrasted_rgb = (saturated_rgb - vec3<f32>(0.5)) * contrast + vec3<f32>(0.5);
    let exposed_rgb = contrasted_rgb * exp2(exposure);
    let adjusted_rgb = (exposed_rgb + vec3<f32>(brightness)) * uniforms.rgb_gain.xyz
        + uniforms.rgb_offset.xyz;

    return clamp(adjusted_rgb, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn display_uv_to_source_uv(display_uv: vec2<f32>) -> vec2<f32> {
    let affine_input = vec3<f32>(display_uv, 1.0);
    let source_x = dot(uniforms.orientation_transform_row0.xyz, affine_input);
    let source_y = dot(uniforms.orientation_transform_row1.xyz, affine_input);

    return vec2<f32>(source_x, source_y);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Apply letterbox scaling.
    let display_uv = input.uv * uniforms.uv_scale + uniforms.uv_offset;

    // Keep letterbox bars black.
    if (display_uv.x < 0.0 || display_uv.x > 1.0 ||
        display_uv.y < 0.0 || display_uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let source_uv = display_uv_to_source_uv(display_uv);
    let sampled_y = textureSample(y_texture, y_sampler, source_uv).r;
    let sampled_uv = textureSample(uv_texture, uv_sampler, source_uv).rg;

    // NV12 stores chroma as U then V; Rg8Unorm exposes them as .r then .g.
    let sampled_u = sampled_uv.r;
    let sampled_v = sampled_uv.g;

    let normalized_yuv = normalize_yuv_sample(sampled_y, sampled_u, sampled_v);
    let rgb = convert_yuv_to_rgb(normalized_yuv);
    let adjusted_rgb = apply_sdr_adjustments(rgb);

    return vec4<f32>(adjusted_rgb, 1.0);
}
