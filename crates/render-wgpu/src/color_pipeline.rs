use codec_core::{ColorRange, MatrixCoefficients, VideoColorMetadata};
use render_core::{
    ActiveColorPath, ActiveColorPathFallback, ColorPipelineSettings, RenderableFrame,
};

/// Размер GPU uniform buffer-а, который должен совпадать с WGSL struct layout.
pub(crate) const COLOR_PIPELINE_UNIFORM_SIZE: u64 =
    std::mem::size_of::<ColorPipelineUniforms>() as u64;

/// Старый shader использовал 16/256, а не 16/255; сохраняем это для visual parity.
const PRESERVE_CURRENT_LIMITED_LUMA_OFFSET: f32 = 0.0625;

/// Старый limited-range множитель luma: 255 / 219.
const LIMITED_LUMA_SCALE: f32 = 1.164_383_6;

/// Центр chroma-компонент в нормализованной NV12 texture.
const CHROMA_CENTER_OFFSET: f32 = 0.5;

/// Текущий shader contract уже держал chroma scale внутри matrix coefficients.
const PRESERVE_CURRENT_CHROMA_SCALE: f32 = 1.0;

/// Full-range luma не требует смещения или расширения диапазона.
const FULL_RANGE_LUMA_OFFSET: f32 = 0.0;

/// Full-range luma уже находится в нормализованном диапазоне 0..1.
const FULL_RANGE_LUMA_SCALE: f32 = 1.0;

/// Full-range chroma scale оставляет U/V вокруг центра без studio-range расширения.
const FULL_RANGE_CHROMA_SCALE: f32 = 1.0;

/// BT.709 limited coefficients из прежнего `nv12_to_rgba.wgsl`.
const BT709_LIMITED_YUV_TO_RGB_ROWS: [[f32; 4]; 3] = [
    [1.0, 0.0, 1.792_741_1, 0.0],
    [1.0, -0.213_248_61, -0.532_909_33, 0.0],
    [1.0, 2.112_401_7, 0.0, 0.0],
];

/// BT.709 full-range coefficients из стандартных Kr/Kb значений.
const BT709_FULL_YUV_TO_RGB_ROWS: [[f32; 4]; 3] = [
    [1.0, 0.0, 1.574_8, 0.0],
    [1.0, -0.187_324_27, -0.468_124_27, 0.0],
    [1.0, 1.855_6, 0.0, 0.0],
];

/// BT.709 luma weights для SDR saturation adjustment.
const BT709_LUMA_WEIGHTS: [f32; 4] = [0.212_6, 0.715_2, 0.072_2, 0.0];

/// BT.601 limited coefficients для SDR SD-контента.
const BT601_LIMITED_YUV_TO_RGB_ROWS: [[f32; 4]; 3] = [
    [1.0, 0.0, 1.596_027_1, 0.0],
    [1.0, -0.391_762_3, -0.812_968, 0.0],
    [1.0, 2.017_232_2, 0.0, 0.0],
];

/// BT.601 full-range coefficients из стандартных Kr/Kb значений.
const BT601_FULL_YUV_TO_RGB_ROWS: [[f32; 4]; 3] = [
    [1.0, 0.0, 1.402, 0.0],
    [1.0, -0.344_136_3, -0.714_136_3, 0.0],
    [1.0, 1.772, 0.0, 0.0],
];

/// BT.601 luma weights для SDR saturation adjustment.
const BT601_LUMA_WEIGHTS: [f32; 4] = [0.299, 0.587, 0.114, 0.0];

/// Uniform buffer для NV12 SDR shader-а.
///
/// Layout вручную держит WGSL uniform alignment:
/// - две `vec2<f32>` в начале занимают первые 16 байт;
/// - остальные поля представлены как `vec4<f32>` и начинаются с 16-byte offset.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ColorPipelineUniforms {
    /// Масштаб UV для letterbox.
    pub uv_scale: [f32; 2],

    /// Смещение UV для letterbox.
    pub uv_offset: [f32; 2],

    /// `x`: Y offset, `y`: Y scale, `z`: U offset, `w`: V offset.
    pub yuv_range: [f32; 4],

    /// `x`: U scale, `y`: V scale, `z/w`: reserved для 16-byte стабильности.
    pub chroma_scale: [f32; 4],

    /// Первая строка YUV->RGB matrix: R.
    pub yuv_to_rgb_row0: [f32; 4],

    /// Вторая строка YUV->RGB matrix: G.
    pub yuv_to_rgb_row1: [f32; 4],

    /// Третья строка YUV->RGB matrix: B.
    pub yuv_to_rgb_row2: [f32; 4],

    /// RGB weights для вычисления luma в saturation adjustment.
    pub saturation_luma_weights: [f32; 4],

    /// `x`: brightness, `y`: contrast, `z`: saturation, `w`: exposure.
    pub color_adjustment: [f32; 4],

    /// Поканальный RGB gain; `w` reserved.
    pub rgb_gain: [f32; 4],

    /// Поканальный RGB offset; `w` reserved.
    pub rgb_offset: [f32; 4],
}

/// Подготовленный результат CPU-side color pipeline для одного NV12 frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PreparedColorPipeline {
    /// Shader uniforms для текущего кадра.
    pub uniforms: ColorPipelineUniforms,

    /// Диагностическое описание выбранного renderer color path.
    pub active_path: ActiveColorPath,
}

/// Собирает uniforms и diagnostic path для текущего NV12 SDR renderer-а.
#[must_use]
pub(crate) fn prepare_nv12_color_pipeline(
    frame: &RenderableFrame,
    settings: &ColorPipelineSettings,
    uv_scale: [f32; 2],
    uv_offset: [f32; 2],
) -> PreparedColorPipeline {
    let active_path = ActiveColorPath::from_frame(frame, settings);
    let effective_color = effective_shader_color_metadata(&active_path);
    let color_coefficients = coefficients_for_metadata(&effective_color);
    let adjustment = settings.adjustment;

    PreparedColorPipeline {
        uniforms: ColorPipelineUniforms {
            uv_scale,
            uv_offset,
            yuv_range: color_coefficients.yuv_range,
            chroma_scale: color_coefficients.chroma_scale,
            yuv_to_rgb_row0: color_coefficients.yuv_to_rgb_rows[0],
            yuv_to_rgb_row1: color_coefficients.yuv_to_rgb_rows[1],
            yuv_to_rgb_row2: color_coefficients.yuv_to_rgb_rows[2],
            saturation_luma_weights: color_coefficients.saturation_luma_weights,
            color_adjustment: [
                adjustment.brightness,
                adjustment.contrast,
                adjustment.saturation,
                adjustment.exposure,
            ],
            rgb_gain: [
                adjustment.rgb_gain[0],
                adjustment.rgb_gain[1],
                adjustment.rgb_gain[2],
                0.0,
            ],
            rgb_offset: [
                adjustment.rgb_offset[0],
                adjustment.rgb_offset[1],
                adjustment.rgb_offset[2],
                0.0,
            ],
        },
        active_path,
    }
}

/// Выбирает metadata, которая реально идёт в shader uniforms.
fn effective_shader_color_metadata(active_path: &ActiveColorPath) -> VideoColorMetadata {
    match active_path.fallback {
        // Полностью поддержанный SDR path использует metadata кадра без подмены.
        None => active_path.input_color.clone(),

        // Эти fallback-и намеренно видны в `ActiveColorPath`, но shader Phase 8.5
        // остаётся SDR BT.709/NV12 path и получает безопасные SDR uniforms.
        Some(
            ActiveColorPathFallback::UnknownInputMetadata
            | ActiveColorPathFallback::WideGamutToSdrBt709
            | ActiveColorPathFallback::UnsupportedHdrInput,
        ) => VideoColorMetadata::sdr_bt709_limited(),
    }
}

/// Набор coefficients, уже разложенный под WGSL uniform rows.
struct YuvToRgbUniformCoefficients {
    /// Range normalization: Y offset/scale и U/V center offsets.
    yuv_range: [f32; 4],

    /// Chroma scale для U/V после вычитания центра.
    chroma_scale: [f32; 4],

    /// Matrix rows, которые shader умножает на нормализованный YUV vector.
    yuv_to_rgb_rows: [[f32; 4]; 3],

    /// Luma weights, которые shader использует только для SDR saturation adjustment.
    saturation_luma_weights: [f32; 4],
}

/// Выбирает range normalization и matrix coefficients для поддержанного SDR metadata.
fn coefficients_for_metadata(color: &VideoColorMetadata) -> YuvToRgbUniformCoefficients {
    let yuv_range = match color.range {
        ColorRange::Limited | ColorRange::Unknown => [
            PRESERVE_CURRENT_LIMITED_LUMA_OFFSET,
            LIMITED_LUMA_SCALE,
            CHROMA_CENTER_OFFSET,
            CHROMA_CENTER_OFFSET,
        ],
        ColorRange::Full => [
            FULL_RANGE_LUMA_OFFSET,
            FULL_RANGE_LUMA_SCALE,
            CHROMA_CENTER_OFFSET,
            CHROMA_CENTER_OFFSET,
        ],
    };

    let chroma_scale = match color.range {
        ColorRange::Limited | ColorRange::Unknown => [
            PRESERVE_CURRENT_CHROMA_SCALE,
            PRESERVE_CURRENT_CHROMA_SCALE,
            0.0,
            0.0,
        ],
        ColorRange::Full => [FULL_RANGE_CHROMA_SCALE, FULL_RANGE_CHROMA_SCALE, 0.0, 0.0],
    };

    let (yuv_to_rgb_rows, saturation_luma_weights) = match (color.matrix, color.range) {
        (MatrixCoefficients::Bt601, ColorRange::Full) => {
            (BT601_FULL_YUV_TO_RGB_ROWS, BT601_LUMA_WEIGHTS)
        }
        (MatrixCoefficients::Bt601, ColorRange::Limited | ColorRange::Unknown) => {
            (BT601_LIMITED_YUV_TO_RGB_ROWS, BT601_LUMA_WEIGHTS)
        }
        (
            MatrixCoefficients::Bt709 | MatrixCoefficients::Bt2020 | MatrixCoefficients::Unknown,
            ColorRange::Full,
        ) => (BT709_FULL_YUV_TO_RGB_ROWS, BT709_LUMA_WEIGHTS),
        (
            MatrixCoefficients::Bt709 | MatrixCoefficients::Bt2020 | MatrixCoefficients::Unknown,
            ColorRange::Limited | ColorRange::Unknown,
        ) => (BT709_LIMITED_YUV_TO_RGB_ROWS, BT709_LUMA_WEIGHTS),
    };

    YuvToRgbUniformCoefficients {
        yuv_range,
        chroma_scale,
        yuv_to_rgb_rows,
        saturation_luma_weights,
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};
    use std::time::Duration;

    use bytemuck::Zeroable;
    use codec_core::{
        BitDepth, ChromaSubsampling, ColorMetadataConfidence, ColorMetadataOrigin, ColorPrimaries,
        ColorRange, HdrMetadata, MatrixCoefficients, TransferFunction,
    };
    use render_core::{
        ActiveColorPathFallback, RenderCapabilities, RenderOutputColorSpace, VideoFrameFormat,
    };

    use super::*;

    /// Допуск нужен только для f32 arithmetic parity с shader formula.
    const FLOAT_TOLERANCE: f32 = 0.000_001;

    /// Возвращает renderer-neutral test frame для unit tests этого backend boundary.
    fn nv12_test_frame(color: VideoColorMetadata) -> RenderableFrame {
        RenderableFrame {
            handle: 1,
            pts: Duration::ZERO,
            format: VideoFrameFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            coded_width: 1920,
            coded_height: 1080,
            render_width: 1920,
            render_height: 1080,
            color,
        }
    }

    /// Создаёт uniforms для test metadata с identity settings и full-frame UV.
    fn uniforms_for_color(color: VideoColorMetadata) -> PreparedColorPipeline {
        prepare_nv12_color_pipeline(
            &nv12_test_frame(color),
            &ColorPipelineSettings::default(),
            [1.0, 1.0],
            [0.0, 0.0],
        )
    }

    /// Проверяет f32 значения без ложной строгости к последнему биту.
    fn assert_float_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= FLOAT_TOLERANCE,
            "actual {actual} differs from expected {expected}"
        );
    }

    /// Повторяет shader conversion на CPU, чтобы сравнить с прежней формулой.
    fn apply_uniforms_to_nv12_sample(
        uniforms: &ColorPipelineUniforms,
        sampled_y: f32,
        sampled_u: f32,
        sampled_v: f32,
    ) -> [f32; 3] {
        let normalized_y = (sampled_y - uniforms.yuv_range[0]) * uniforms.yuv_range[1];
        let normalized_u = (sampled_u - uniforms.yuv_range[2]) * uniforms.chroma_scale[0];
        let normalized_v = (sampled_v - uniforms.yuv_range[3]) * uniforms.chroma_scale[1];

        let red = uniforms.yuv_to_rgb_row0[0] * normalized_y
            + uniforms.yuv_to_rgb_row0[1] * normalized_u
            + uniforms.yuv_to_rgb_row0[2] * normalized_v
            + uniforms.yuv_to_rgb_row0[3];
        let green = uniforms.yuv_to_rgb_row1[0] * normalized_y
            + uniforms.yuv_to_rgb_row1[1] * normalized_u
            + uniforms.yuv_to_rgb_row1[2] * normalized_v
            + uniforms.yuv_to_rgb_row1[3];
        let blue = uniforms.yuv_to_rgb_row2[0] * normalized_y
            + uniforms.yuv_to_rgb_row2[1] * normalized_u
            + uniforms.yuv_to_rgb_row2[2] * normalized_v
            + uniforms.yuv_to_rgb_row2[3];

        [
            red.clamp(0.0, 1.0),
            green.clamp(0.0, 1.0),
            blue.clamp(0.0, 1.0),
        ]
    }

    /// Повторяет старую WGSL BT.709 limited формулу без uniform indirection.
    fn apply_old_bt709_limited_shader_formula(
        sampled_y: f32,
        sampled_u: f32,
        sampled_v: f32,
    ) -> [f32; 3] {
        let scaled_y = (sampled_y - 0.0625) * 1.164_383_6;
        let scaled_u = sampled_u - 0.5;
        let scaled_v = sampled_v - 0.5;

        let red = scaled_y + 1.792_741_1 * scaled_v;
        let green = scaled_y - 0.532_909_33 * scaled_v - 0.213_248_61 * scaled_u;
        let blue = scaled_y + 2.112_401_7 * scaled_u;

        [
            red.clamp(0.0, 1.0),
            green.clamp(0.0, 1.0),
            blue.clamp(0.0, 1.0),
        ]
    }

    #[test]
    fn color_pipeline_uniforms_match_wgsl_uniform_layout() {
        let uniforms = ColorPipelineUniforms::zeroed();

        assert_eq!(align_of::<ColorPipelineUniforms>(), 16);
        assert_eq!(size_of::<ColorPipelineUniforms>(), 160);
        assert_eq!(size_of::<ColorPipelineUniforms>() % 16, 0);
        assert_eq!(offset_of!(ColorPipelineUniforms, uv_scale), 0);
        assert_eq!(offset_of!(ColorPipelineUniforms, uv_offset), 8);
        assert_eq!(offset_of!(ColorPipelineUniforms, yuv_range), 16);
        assert_eq!(offset_of!(ColorPipelineUniforms, chroma_scale), 32);
        assert_eq!(offset_of!(ColorPipelineUniforms, yuv_to_rgb_row0), 48);
        assert_eq!(offset_of!(ColorPipelineUniforms, yuv_to_rgb_row1), 64);
        assert_eq!(offset_of!(ColorPipelineUniforms, yuv_to_rgb_row2), 80);
        assert_eq!(
            offset_of!(ColorPipelineUniforms, saturation_luma_weights),
            96
        );
        assert_eq!(offset_of!(ColorPipelineUniforms, color_adjustment), 112);
        assert_eq!(offset_of!(ColorPipelineUniforms, rgb_gain), 128);
        assert_eq!(offset_of!(ColorPipelineUniforms, rgb_offset), 144);
        assert_eq!(
            bytemuck::bytes_of(&uniforms).len() as u64,
            COLOR_PIPELINE_UNIFORM_SIZE
        );
    }

    #[test]
    fn bt709_limited_uniforms_match_previous_shader_formula() {
        let prepared_color_pipeline = uniforms_for_color(VideoColorMetadata::sdr_bt709_limited());

        let sampled_values = [(0.50, 0.50, 0.50), (0.42, 0.47, 0.56), (0.72, 0.62, 0.44)];

        for (sampled_y, sampled_u, sampled_v) in sampled_values {
            let uniform_rgb = apply_uniforms_to_nv12_sample(
                &prepared_color_pipeline.uniforms,
                sampled_y,
                sampled_u,
                sampled_v,
            );
            let old_shader_rgb =
                apply_old_bt709_limited_shader_formula(sampled_y, sampled_u, sampled_v);

            assert_float_close(uniform_rgb[0], old_shader_rgb[0]);
            assert_float_close(uniform_rgb[1], old_shader_rgb[1]);
            assert_float_close(uniform_rgb[2], old_shader_rgb[2]);
        }
    }

    #[test]
    fn full_range_metadata_uses_full_range_normalization() {
        let mut full_range_color = VideoColorMetadata::sdr_bt709_limited();
        full_range_color.range = ColorRange::Full;
        full_range_color.origin = ColorMetadataOrigin::Container;
        full_range_color.confidence = ColorMetadataConfidence::Confirmed;

        let prepared_color_pipeline = uniforms_for_color(full_range_color);
        let uniforms = prepared_color_pipeline.uniforms;

        assert_eq!(uniforms.yuv_range, [0.0, 1.0, 0.5, 0.5]);
        assert_eq!(uniforms.chroma_scale, [1.0, 1.0, 0.0, 0.0]);
        assert_float_close(uniforms.yuv_to_rgb_row0[2], 1.574_8);
        assert_float_close(uniforms.yuv_to_rgb_row2[1], 1.855_6);
    }

    #[test]
    fn bt2020_sdr_metadata_uses_sdr_bt709_fallback_marker_without_wide_gamut_output() {
        let bt2020_sdr_color = VideoColorMetadata {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt2020,
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Bt709,
            hdr_metadata: None,
            origin: ColorMetadataOrigin::Container,
            confidence: ColorMetadataConfidence::Confirmed,
        };

        let prepared_color_pipeline = uniforms_for_color(bt2020_sdr_color);
        let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));

        assert_eq!(
            prepared_color_pipeline.active_path.fallback,
            Some(ActiveColorPathFallback::WideGamutToSdrBt709)
        );
        assert_eq!(
            prepared_color_pipeline.active_path.output_color_space,
            RenderOutputColorSpace::SdrBt709
        );
        assert!(!capabilities.supports_native_hdr_output);
        assert!(!capabilities.supports_hdr_to_sdr);
        assert_eq!(
            prepared_color_pipeline.uniforms.yuv_to_rgb_row0,
            BT709_LIMITED_YUV_TO_RGB_ROWS[0]
        );
    }

    #[test]
    fn unknown_metadata_uses_bt709_limited_fallback_uniforms_with_marker() {
        let unknown_color = VideoColorMetadata {
            range: ColorRange::Unknown,
            matrix: MatrixCoefficients::Unknown,
            primaries: ColorPrimaries::Unknown,
            transfer: TransferFunction::Unknown,
            hdr_metadata: None,
            origin: ColorMetadataOrigin::Container,
            confidence: ColorMetadataConfidence::Hint,
        };

        let fallback_pipeline = uniforms_for_color(VideoColorMetadata::sdr_bt709_limited());
        let unknown_pipeline = uniforms_for_color(unknown_color);

        assert_eq!(
            unknown_pipeline.active_path.fallback,
            Some(ActiveColorPathFallback::UnknownInputMetadata)
        );
        assert_eq!(unknown_pipeline.uniforms, fallback_pipeline.uniforms);
    }

    #[test]
    fn pq_and_hlg_metadata_stay_unsupported_by_current_hdr_capabilities() {
        for transfer in [TransferFunction::Pq, TransferFunction::Hlg] {
            let hdr_like_color = VideoColorMetadata {
                range: ColorRange::Limited,
                matrix: MatrixCoefficients::Bt2020,
                primaries: ColorPrimaries::Bt2020,
                transfer,
                hdr_metadata: Some(HdrMetadata {
                    color_primaries: ColorPrimaries::Bt2020,
                    transfer_function: transfer,
                    max_luminance_nits: Some(1_000.0),
                    min_luminance_nits: Some(0.01),
                    max_content_light_level_nits: Some(1_000),
                    max_frame_average_light_level_nits: Some(400),
                }),
                origin: ColorMetadataOrigin::Bitstream,
                confidence: ColorMetadataConfidence::Confirmed,
            };

            let prepared_color_pipeline = uniforms_for_color(hdr_like_color);
            let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));

            assert_eq!(
                prepared_color_pipeline.active_path.fallback,
                Some(ActiveColorPathFallback::UnsupportedHdrInput)
            );
            assert!(!capabilities.supports_hdr_to_sdr);
            assert!(!capabilities.supports_native_hdr_output);
            assert_eq!(
                prepared_color_pipeline.active_path.output_color_space,
                RenderOutputColorSpace::SdrBt709
            );
            assert_eq!(
                prepared_color_pipeline.uniforms,
                uniforms_for_color(VideoColorMetadata::sdr_bt709_limited()).uniforms
            );
        }
    }
}
