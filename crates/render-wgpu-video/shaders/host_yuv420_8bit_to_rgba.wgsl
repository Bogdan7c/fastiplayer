// Host-planar YUV 8-bit to RGBA fragment shader.
// CPU uploads Y, U, and V bytes as separate R8Unorm textures; color conversion stays on GPU.

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

struct ColorPipelineUniforms {
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
    orientation_transform_row0: vec4<f32>,
    orientation_transform_row1: vec4<f32>,
    yuv_range: vec4<f32>,
    chroma_scale: vec4<f32>,
    yuv_to_rgb_row0: vec4<f32>,
    yuv_to_rgb_row1: vec4<f32>,
    yuv_to_rgb_row2: vec4<f32>,
    saturation_luma_weights: vec4<f32>,
    color_adjustment: vec4<f32>,
    rgb_gain: vec4<f32>,
    rgb_offset: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> uniforms: ColorPipelineUniforms;

@group(0) @binding(1)
var planar_sampler: sampler;

@group(0) @binding(2)
var y_texture: texture_2d<f32>;

@group(0) @binding(3)
var u_texture: texture_2d<f32>;

@group(0) @binding(4)
var v_texture: texture_2d<f32>;

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

fn source_uv_to_texel(texture_size: vec2<u32>, source_uv: vec2<f32>) -> vec2<u32> {
    let max_texel = texture_size - vec2<u32>(1u);
    let clamped_uv = clamp(source_uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let unclamped_texel = floor(clamped_uv * vec2<f32>(texture_size));
    let texel = min(
        vec2<u32>(u32(unclamped_texel.x), u32(unclamped_texel.y)),
        max_texel,
    );

    return texel;
}

fn axis_subsampling_factor(luma_axis_size: u32, chroma_axis_size: u32) -> u32 {
    // 4:2:0 и 4:2:2 используют ceil(width / 2), поэтому odd widths тоже дают factor 2.
    if (chroma_axis_size < luma_axis_size) {
        return 2u;
    }

    return 1u;
}

fn source_axis_to_chroma_uv(
    luma_axis_size: u32,
    chroma_axis_size: u32,
    luma_axis_texel: u32,
    source_axis_uv: f32,
) -> f32 {
    if (chroma_axis_size == luma_axis_size) {
        return source_axis_uv;
    }

    let subsampling_factor = axis_subsampling_factor(luma_axis_size, chroma_axis_size);
    let max_chroma_texel = chroma_axis_size - 1u;
    let chroma_texel = min(luma_axis_texel / subsampling_factor, max_chroma_texel);

    return (f32(chroma_texel) + 0.5) / f32(chroma_axis_size);
}

fn source_uv_to_chroma_uv(chroma_texture_size: vec2<u32>, source_uv: vec2<f32>) -> vec2<f32> {
    let luma_texture_size = textureDimensions(y_texture);
    let luma_texel = source_uv_to_texel(luma_texture_size, source_uv);

    return vec2<f32>(
        source_axis_to_chroma_uv(
            luma_texture_size.x,
            chroma_texture_size.x,
            luma_texel.x,
            source_uv.x,
        ),
        source_axis_to_chroma_uv(
            luma_texture_size.y,
            chroma_texture_size.y,
            luma_texel.y,
            source_uv.y,
        ),
    );
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let display_uv = input.uv * uniforms.uv_scale + uniforms.uv_offset;

    if (display_uv.x < 0.0 || display_uv.x > 1.0 ||
        display_uv.y < 0.0 || display_uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let source_uv = display_uv_to_source_uv(display_uv);
    let sampled_y = textureSample(y_texture, planar_sampler, source_uv).r;
    let sampled_u = textureSample(
        u_texture,
        planar_sampler,
        source_uv_to_chroma_uv(textureDimensions(u_texture), source_uv),
    ).r;
    let sampled_v = textureSample(
        v_texture,
        planar_sampler,
        source_uv_to_chroma_uv(textureDimensions(v_texture), source_uv),
    ).r;
    let normalized_yuv = normalize_yuv_sample(sampled_y, sampled_u, sampled_v);
    let rgb = convert_yuv_to_rgb(normalized_yuv);
    let adjusted_rgb = apply_sdr_adjustments(rgb);

    return vec4<f32>(adjusted_rgb, 1.0);
}
