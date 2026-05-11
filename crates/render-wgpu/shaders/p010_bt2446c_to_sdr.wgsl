// P010 to SDR fragment shader skeleton.
// The file is intentionally separate from nv12_to_rgba.wgsl so P010/HDR work
// cannot silently change the current NV12 SDR path.

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );

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

struct HdrColorPipelineUniforms {
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
    shader_mode: vec4<u32>,
    luma_range: vec4<f32>,
    chroma_range: vec4<f32>,
    hdr_reference_nits: vec4<f32>,
    content_light_levels: vec4<f32>,
    optional_metadata_markers: vec4<u32>,
};

@group(0) @binding(0)
var<uniform> uniforms: HdrColorPipelineUniforms;

@group(0) @binding(1)
var p010_sampler: sampler;

@group(0) @binding(2)
var p010_y_texture: texture_2d<f32>;

@group(0) @binding(3)
var p010_uv_texture: texture_2d<f32>;

const P010_SHADER_MODE_SDR_BT709: u32 = 0u;
const P010_10BIT_STORAGE_SHIFT_SCALE: f32 = 64.0;
const P010_10BIT_MAX_CODE_VALUE: f32 = 1023.0;
const R16_UNORM_MAX_CODE_VALUE: f32 = 65535.0;

fn decode_p010_unorm_to_code_value(sampled_component: f32) -> f32 {
    return clamp(
        sampled_component * R16_UNORM_MAX_CODE_VALUE / P010_10BIT_STORAGE_SHIFT_SCALE,
        0.0,
        P010_10BIT_MAX_CODE_VALUE,
    );
}

fn normalize_p010_sample(sampled_y: f32, sampled_u: f32, sampled_v: f32) -> vec3<f32> {
    let y_code = clamp(
        decode_p010_unorm_to_code_value(sampled_y),
        uniforms.luma_range.z,
        uniforms.luma_range.w,
    );
    let u_code = decode_p010_unorm_to_code_value(sampled_u);
    let v_code = decode_p010_unorm_to_code_value(sampled_v);
    let normalized_y = (y_code - uniforms.luma_range.x) * uniforms.luma_range.y;
    let normalized_u = (u_code - uniforms.chroma_range.x) * uniforms.chroma_range.y;
    let normalized_v = (v_code - uniforms.chroma_range.z) * uniforms.chroma_range.w;

    return vec3<f32>(normalized_y, normalized_u, normalized_v);
}

fn p010_sdr_bt709_to_rgb(normalized_yuv: vec3<f32>) -> vec3<f32> {
    let red = normalized_yuv.x + 1.5748 * normalized_yuv.z;
    let green = normalized_yuv.x - 0.18732427 * normalized_yuv.y - 0.46812427 * normalized_yuv.z;
    let blue = normalized_yuv.x + 1.8556 * normalized_yuv.y;

    return clamp(vec3<f32>(red, green, blue), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn p010_hdr_skeleton_to_sdr(normalized_yuv: vec3<f32>) -> vec3<f32> {
    let placeholder_reference_scale = uniforms.hdr_reference_nits.x
        / max(uniforms.hdr_reference_nits.y, uniforms.hdr_reference_nits.x);
    let grayscale = clamp(normalized_yuv.x * max(placeholder_reference_scale, 0.1), 0.0, 1.0);

    return vec3<f32>(grayscale);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let scaled_uv = input.uv * uniforms.uv_scale + uniforms.uv_offset;

    if (scaled_uv.x < 0.0 || scaled_uv.x > 1.0 ||
        scaled_uv.y < 0.0 || scaled_uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let sampled_y = textureSample(p010_y_texture, p010_sampler, scaled_uv).r;
    let sampled_uv = textureSample(p010_uv_texture, p010_sampler, scaled_uv).rg;
    let normalized_yuv = normalize_p010_sample(sampled_y, sampled_uv.r, sampled_uv.g);

    if (uniforms.shader_mode.x == P010_SHADER_MODE_SDR_BT709) {
        return vec4<f32>(p010_sdr_bt709_to_rgb(normalized_yuv), 1.0);
    }

    return vec4<f32>(p010_hdr_skeleton_to_sdr(normalized_yuv), 1.0);
}
