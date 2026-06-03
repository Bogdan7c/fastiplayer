// P010 to SDR fullscreen shader.
// P010 SDR BT.709 and P010 HDR BT.2446-C live here, while NV12 SDR remains in
// nv12_to_rgba.wgsl to keep the existing SDR path isolated.

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
    orientation_transform_row0: vec4<f32>,
    orientation_transform_row1: vec4<f32>,
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
const P010_SHADER_MODE_HDR_BT2446C: u32 = 1u;
const P010_TRANSFER_MODE_SDR_BT709: u32 = 0u;
const P010_TRANSFER_MODE_PQ: u32 = 1u;
const P010_TRANSFER_MODE_HLG: u32 = 2u;
const P010_10BIT_STORAGE_SHIFT_SCALE: f32 = 64.0;
const P010_10BIT_MAX_CODE_VALUE: f32 = 1023.0;
const R16_UNORM_MAX_CODE_VALUE: f32 = 65535.0;
const BT2020_KR: f32 = 0.2627;
const BT2020_KB: f32 = 0.0593;
const BT2020_KG: f32 = 0.6780;
const PQ_M1: f32 = 0.1593017578125;
const PQ_M2: f32 = 78.84375;
const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.8515625;
const PQ_C3: f32 = 18.6875;
const PQ_REFERENCE_PEAK_NITS: f32 = 10000.0;
const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 0.28466892;
const HLG_C: f32 = 0.559910729529562;
const HLG_REFERENCE_SYSTEM_GAMMA: f32 = 1.2;
const BT2446C_REFERENCE_CROSSTALK_ALPHA: f32 = 0.04;
const BT2446C_K1: f32 = 0.83802;
const BT2446C_K2: f32 = 15.09968;
const BT2446C_K3: f32 = 0.74204;
const BT2446C_K4: f32 = 78.99439;
const BT2446C_SDR_INFLECTION_NITS: f32 = 58.5;
const BT1886_REFERENCE_GAMMA: f32 = 2.4;
const D65_WHITE_X: f32 = 0.3127;
const D65_WHITE_Y: f32 = 0.3290;
const CHROMATICITY_EPSILON: f32 = 0.000001;
const BT2020_LINEAR_RGB_TO_XYZ_ROW0: vec3<f32> = vec3<f32>(0.6370, 0.1446, 0.1689);
const BT2020_LINEAR_RGB_TO_XYZ_ROW1: vec3<f32> = vec3<f32>(0.2627, 0.6780, 0.0593);
const BT2020_LINEAR_RGB_TO_XYZ_ROW2: vec3<f32> = vec3<f32>(0.0000, 0.0281, 1.0610);
const XYZ_TO_BT709_LINEAR_RGB_ROW0: vec3<f32> = vec3<f32>(1.7167, -0.3557, -0.2534);
const XYZ_TO_BT709_LINEAR_RGB_ROW1: vec3<f32> = vec3<f32>(-0.6667, 1.6165, 0.0158);
const XYZ_TO_BT709_LINEAR_RGB_ROW2: vec3<f32> = vec3<f32>(0.0176, -0.0428, 0.9421);

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

fn bt2020_yuv_to_rgb(normalized_yuv: vec3<f32>) -> vec3<f32> {
    let red = normalized_yuv.x + 2.0 * (1.0 - BT2020_KR) * normalized_yuv.z;
    let blue = normalized_yuv.x + 2.0 * (1.0 - BT2020_KB) * normalized_yuv.y;
    let green = (normalized_yuv.x - BT2020_KR * red - BT2020_KB * blue) / BT2020_KG;

    return vec3<f32>(red, green, blue);
}

fn pq_eotf_nits(encoded_value: f32) -> f32 {
    let bounded_encoded_value = clamp(encoded_value, 0.0, 1.0);
    let encoded_root = pow(bounded_encoded_value, 1.0 / PQ_M2);
    let numerator = max(encoded_root - PQ_C1, 0.0);
    let denominator = PQ_C2 - PQ_C3 * encoded_root;
    let normalized_luminance = pow(numerator / denominator, 1.0 / PQ_M1);

    return PQ_REFERENCE_PEAK_NITS * normalized_luminance;
}

fn hlg_inverse_oetf(encoded_value: f32) -> f32 {
    let bounded_encoded_value = clamp(encoded_value, 0.0, 1.0);

    if (bounded_encoded_value <= 0.5) {
        return bounded_encoded_value * bounded_encoded_value / 3.0;
    }

    return (exp((bounded_encoded_value - HLG_C) / HLG_A) + HLG_B) / 12.0;
}

fn hlg_eotf_rgb_nits(encoded_rgb: vec3<f32>) -> vec3<f32> {
    let scene_rgb = vec3<f32>(
        hlg_inverse_oetf(encoded_rgb.r),
        hlg_inverse_oetf(encoded_rgb.g),
        hlg_inverse_oetf(encoded_rgb.b),
    );
    let scene_luma = BT2020_KR * scene_rgb.r + BT2020_KG * scene_rgb.g + BT2020_KB * scene_rgb.b;

    if (scene_luma <= 0.0) {
        return vec3<f32>(0.0);
    }

    let peak_nits = max(uniforms.hdr_reference_nits.y, uniforms.hdr_reference_nits.x);
    let system_gamma_scale = pow(scene_luma, HLG_REFERENCE_SYSTEM_GAMMA - 1.0);

    return peak_nits * system_gamma_scale * scene_rgb;
}

fn apply_hdr_eotf(nonlinear_bt2020_rgb: vec3<f32>) -> vec3<f32> {
    let bounded_rgb = clamp(nonlinear_bt2020_rgb, vec3<f32>(0.0), vec3<f32>(1.0));

    if (uniforms.shader_mode.y == P010_TRANSFER_MODE_PQ) {
        return vec3<f32>(
            pq_eotf_nits(bounded_rgb.r),
            pq_eotf_nits(bounded_rgb.g),
            pq_eotf_nits(bounded_rgb.b),
        );
    }

    if (uniforms.shader_mode.y == P010_TRANSFER_MODE_HLG) {
        return hlg_eotf_rgb_nits(bounded_rgb);
    }

    return vec3<f32>(0.0);
}

fn apply_bt2446c_crosstalk(rgb: vec3<f32>) -> vec3<f32> {
    let alpha = BT2446C_REFERENCE_CROSSTALK_ALPHA;
    let self_weight = 1.0 - 2.0 * alpha;

    return vec3<f32>(
        self_weight * rgb.r + alpha * rgb.g + alpha * rgb.b,
        alpha * rgb.r + self_weight * rgb.g + alpha * rgb.b,
        alpha * rgb.r + alpha * rgb.g + self_weight * rgb.b,
    );
}

fn bt2020_linear_rgb_to_xyz(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(BT2020_LINEAR_RGB_TO_XYZ_ROW0, rgb),
        dot(BT2020_LINEAR_RGB_TO_XYZ_ROW1, rgb),
        dot(BT2020_LINEAR_RGB_TO_XYZ_ROW2, rgb),
    );
}

fn xyz_to_chromaticity(xyz: vec3<f32>) -> vec2<f32> {
    let tristimulus_sum = xyz.x + xyz.y + xyz.z;

    if (abs(tristimulus_sum) <= CHROMATICITY_EPSILON) {
        return vec2<f32>(D65_WHITE_X, D65_WHITE_Y);
    }

    return vec2<f32>(xyz.x / tristimulus_sum, xyz.y / tristimulus_sum);
}

fn bt2446c_tone_map_luminance_nits(hdr_luminance_nits: f32) -> f32 {
    let hdr_inflection_nits = BT2446C_SDR_INFLECTION_NITS / BT2446C_K1;

    if (hdr_luminance_nits < hdr_inflection_nits) {
        return BT2446C_K1 * hdr_luminance_nits;
    }

    return BT2446C_K2 * log(hdr_luminance_nits / hdr_inflection_nits - BT2446C_K3) + BT2446C_K4;
}

fn xyz_from_luminance_and_chromaticity(luminance_nits: f32, chromaticity: vec2<f32>) -> vec3<f32> {
    if (luminance_nits <= 0.0) {
        return vec3<f32>(0.0);
    }

    let chromaticity_y = max(chromaticity.y, CHROMATICITY_EPSILON);

    return vec3<f32>(
        chromaticity.x / chromaticity_y * luminance_nits,
        luminance_nits,
        (1.0 - chromaticity.x - chromaticity.y) / chromaticity_y * luminance_nits,
    );
}

fn xyz_to_bt709_linear_rgb(xyz: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(XYZ_TO_BT709_LINEAR_RGB_ROW0, xyz),
        dot(XYZ_TO_BT709_LINEAR_RGB_ROW1, xyz),
        dot(XYZ_TO_BT709_LINEAR_RGB_ROW2, xyz),
    );
}

fn apply_bt2446c_inverse_crosstalk(rgb: vec3<f32>) -> vec3<f32> {
    let alpha = BT2446C_REFERENCE_CROSSTALK_ALPHA;
    let denominator = 1.0 - 3.0 * alpha;

    return vec3<f32>(
        ((1.0 - alpha) * rgb.r - alpha * rgb.g - alpha * rgb.b) / denominator,
        (-alpha * rgb.r + (1.0 - alpha) * rgb.g - alpha * rgb.b) / denominator,
        (-alpha * rgb.r - alpha * rgb.g + (1.0 - alpha) * rgb.b) / denominator,
    );
}

fn inverse_bt1886_oetf(linear_nits: f32, reference_white_nits: f32) -> f32 {
    return pow(clamp(linear_nits / reference_white_nits, 0.0, 1.0), 1.0 / BT1886_REFERENCE_GAMMA);
}

fn encode_sdr_bt709_output(linear_rgb_nits: vec3<f32>) -> vec3<f32> {
    let reference_white_nits = max(uniforms.hdr_reference_nits.x, 0.001);

    return vec3<f32>(
        inverse_bt1886_oetf(linear_rgb_nits.r, reference_white_nits),
        inverse_bt1886_oetf(linear_rgb_nits.g, reference_white_nits),
        inverse_bt1886_oetf(linear_rgb_nits.b, reference_white_nits),
    );
}

fn p010_hdr_bt2446c_to_sdr(normalized_yuv: vec3<f32>) -> vec3<f32> {
    let nonlinear_bt2020_rgb = bt2020_yuv_to_rgb(normalized_yuv);
    let linear_hdr_rgb = apply_hdr_eotf(nonlinear_bt2020_rgb);
    let crosstalk_hdr_rgb = apply_bt2446c_crosstalk(linear_hdr_rgb);
    let hdr_xyz = bt2020_linear_rgb_to_xyz(crosstalk_hdr_rgb);
    let hdr_chromaticity = xyz_to_chromaticity(hdr_xyz);
    let tone_mapped_sdr_luminance_nits = bt2446c_tone_map_luminance_nits(max(hdr_xyz.y, 0.0));
    let sdr_xyz = xyz_from_luminance_and_chromaticity(
        tone_mapped_sdr_luminance_nits,
        hdr_chromaticity,
    );
    let crosstalk_sdr_rgb = xyz_to_bt709_linear_rgb(sdr_xyz);
    let linear_sdr_rgb = apply_bt2446c_inverse_crosstalk(crosstalk_sdr_rgb);

    return encode_sdr_bt709_output(linear_sdr_rgb);
}

fn display_uv_to_source_uv(display_uv: vec2<f32>) -> vec2<f32> {
    let affine_input = vec3<f32>(display_uv, 1.0);
    let source_x = dot(uniforms.orientation_transform_row0.xyz, affine_input);
    let source_y = dot(uniforms.orientation_transform_row1.xyz, affine_input);

    return vec2<f32>(source_x, source_y);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let display_uv = input.uv * uniforms.uv_scale + uniforms.uv_offset;

    if (display_uv.x < 0.0 || display_uv.x > 1.0 ||
        display_uv.y < 0.0 || display_uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let source_uv = display_uv_to_source_uv(display_uv);
    let sampled_y = textureSample(p010_y_texture, p010_sampler, source_uv).r;
    let sampled_uv = textureSample(p010_uv_texture, p010_sampler, source_uv).rg;
    let normalized_yuv = normalize_p010_sample(sampled_y, sampled_uv.r, sampled_uv.g);

    if (uniforms.shader_mode.x == P010_SHADER_MODE_SDR_BT709) {
        return vec4<f32>(p010_sdr_bt709_to_rgb(normalized_yuv), 1.0);
    }

    if (uniforms.shader_mode.x == P010_SHADER_MODE_HDR_BT2446C) {
        return vec4<f32>(p010_hdr_bt2446c_to_sdr(normalized_yuv), 1.0);
    }

    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
