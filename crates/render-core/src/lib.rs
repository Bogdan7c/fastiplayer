//! Общие контракты render layer.
//!
//! Этот crate не создаёт GPU-ресурсы и не зависит от `wgpu`, `egui` или windowing.
//! Его задача — описать факты, которые нужны player/capability layer для выбора
//! воспроизводимого потока до запуска decode.

#![forbid(unsafe_code)]

use std::fmt;
use std::time::Duration;

use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction,
    VideoColorMetadata, VideoDecodeRequirement,
};
use serde::{Deserialize, Serialize};

/// Стабильный идентификатор семейства renderer backend-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderBackendKind {
    /// WGPU backend: Vulkan-first на Linux, с будущими DX12/Metal target-ами.
    Wgpu,

    /// Future OpenGL ES fallback для старых X11/GLES2 систем.
    OpenGles,
}

impl RenderBackendKind {
    /// Возвращает короткое стабильное имя backend-а для diagnostics.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::Wgpu => "wgpu",
            Self::OpenGles => "opengles",
        }
    }

    /// Возвращает человекочитаемое имя backend-а для UI/report.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Wgpu => "WGPU",
            Self::OpenGles => "OpenGL ES",
        }
    }
}

impl fmt::Display for RenderBackendKind {
    /// Печатает стабильный id без дополнительного форматирования.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.stable_id())
    }
}

/// Формат decoded frame, который renderer может принять на вход.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoFrameFormat {
    /// 8-bit 4:2:0 NV12: отдельная Y plane и interleaved UV plane.
    Nv12,

    /// 10-bit 4:2:0 P010: будущий путь для HDR и 10-bit SDR.
    P010,

    /// 8-bit RGBA texture: software/upload fallback или готовый RGB path.
    Rgba8,
}

impl VideoFrameFormat {
    /// Выводит минимально ожидаемый renderer input format из stream requirement.
    ///
    /// Unknown bit depth трактуется как текущий MVP SDR/NV12 path. Если bitstream
    /// позже уточнит profile/bit depth до P010, повторная проверка capability layer
    /// отвергнет поток до отправки packet-а в hardware decoder.
    #[must_use]
    pub fn from_decode_requirement(requirement: &VideoDecodeRequirement) -> Option<Self> {
        if let Some(chroma) = requirement.chroma
            && chroma != ChromaSubsampling::Yuv420
        {
            return None;
        }

        match requirement.bit_depth {
            Some(BitDepth::Ten) => Some(Self::P010),
            Some(BitDepth::Twelve) => None,
            Some(BitDepth::Eight) | None => Some(Self::Nv12),
        }
    }
}

impl fmt::Display for VideoFrameFormat {
    /// Печатает формат кадра в привычной video-терминологии.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Nv12 => "NV12",
            Self::P010 => "P010",
            Self::Rgba8 => "RGBA8",
        };
        formatter.write_str(label)
    }
}

/// Нейтральная для renderer-а color metadata, проброшенная с decoder boundary.
pub type RenderColorMetadata = VideoColorMetadata;

/// Цветовое пространство, в которое renderer готовит swapchain output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderOutputColorSpace {
    /// SDR output с BT.709/sRGB primaries для текущего обычного swapchain path.
    SdrBt709,
}

impl RenderOutputColorSpace {
    /// Возвращает подпись для diagnostics и capability report.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::SdrBt709 => "SDR BT.709",
        }
    }
}

impl fmt::Display for RenderOutputColorSpace {
    /// Печатает output color space в человекочитаемом виде.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
    }
}

/// Поведение swapchain transfer на финальной записи в render target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwapchainTransferMode {
    /// Текущий Phase 8.5 режим: shader пишет display-referred SDR значения в `Unorm`.
    PreserveCurrentUnorm,

    /// Будущий режим: shader отдаёт linear SDR, а `UnormSrgb` render target делает encode.
    SrgbRenderTarget,

    /// Будущий режим: shader явно применяет output OETF и пишет результат в `Unorm`.
    ExplicitShaderOetf,
}

impl SwapchainTransferMode {
    /// Возвращает стабильный id для логов, config и diagnostics.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::PreserveCurrentUnorm => "preserve-current-unorm",
            Self::SrgbRenderTarget => "srgb-render-target",
            Self::ExplicitShaderOetf => "explicit-shader-oetf",
        }
    }
}

impl Default for SwapchainTransferMode {
    /// Сохраняет текущий SDR result и не переключает swapchain на implicit sRGB conversion.
    fn default() -> Self {
        Self::PreserveCurrentUnorm
    }
}

impl fmt::Display for SwapchainTransferMode {
    /// Печатает стабильный id режима transfer.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.stable_id())
    }
}

/// Tone mapping mode, который renderer может применить при поддержанном HDR path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToneMappingMode {
    /// Tone mapping отключён; единственный активный режим текущего SDR renderer-а.
    Off,

    /// Будущий SDR output mode с простой Reinhard-кривой.
    Reinhard,

    /// Будущий SDR output mode с ACES-like filmic curve.
    AcesFitted,
}

impl ToneMappingMode {
    /// Возвращает стабильный id для логов, config и diagnostics.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Reinhard => "reinhard",
            Self::AcesFitted => "aces-fitted",
        }
    }
}

impl Default for ToneMappingMode {
    /// Не обещает HDR support и сохраняет текущий SDR-only renderer path.
    fn default() -> Self {
        Self::Off
    }
}

impl fmt::Display for ToneMappingMode {
    /// Печатает стабильный id режима tone mapping.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.stable_id())
    }
}

/// Пользовательские SDR/RGB корректировки с identity default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ColorAdjustment {
    /// Аддитивное смещение яркости в SDR shader space.
    pub brightness: f32,

    /// Множитель контраста вокруг нейтральной середины.
    pub contrast: f32,

    /// Множитель насыщенности.
    pub saturation: f32,

    /// Exposure offset для будущей linear-light части pipeline.
    pub exposure: f32,

    /// Поканальный множитель RGB.
    pub rgb_gain: [f32; 3],

    /// Поканальное аддитивное смещение RGB.
    pub rgb_offset: [f32; 3],
}

impl ColorAdjustment {
    /// Возвращает identity настройки, которые не меняют SDR картинку.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            exposure: 0.0,
            rgb_gain: [1.0, 1.0, 1.0],
            rgb_offset: [0.0, 0.0, 0.0],
        }
    }

    /// Проверяет, что корректировки не меняют входной цвет.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::identity()
    }
}

impl Default for ColorAdjustment {
    /// Делает отсутствие config безопасным для текущего SDR результата.
    fn default() -> Self {
        Self::identity()
    }
}

/// Настройки renderer color pipeline, общие для SDR path и будущего HDR path.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ColorPipelineSettings {
    /// SDR/RGB корректировки, применяемые после YUV->RGB conversion.
    pub adjustment: ColorAdjustment,

    /// Tone mapping mode; текущий renderer поддерживает только `Off`.
    pub tone_mapping: ToneMappingMode,

    /// Поведение output transfer при записи в swapchain render target.
    pub swapchain_transfer: SwapchainTransferMode,
}

impl ColorPipelineSettings {
    /// Возвращает settings, которые сохраняют текущий SDR NV12 результат.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            adjustment: ColorAdjustment::identity(),
            tone_mapping: ToneMappingMode::Off,
            swapchain_transfer: SwapchainTransferMode::PreserveCurrentUnorm,
        }
    }

    /// Проверяет, что весь color pipeline находится в identity/default режиме.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.adjustment.is_identity()
            && self.tone_mapping == ToneMappingMode::Off
            && self.swapchain_transfer == SwapchainTransferMode::PreserveCurrentUnorm
    }
}

impl Default for ColorPipelineSettings {
    /// Сохраняет visual parity текущего SDR path при отсутствии пользовательского config.
    fn default() -> Self {
        Self::identity()
    }
}

/// Причина, по которой active color path помечен как временный fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveColorPathFallback {
    /// Metadata неизвестна, поэтому renderer будет использовать SDR BT.709 default.
    UnknownInputMetadata,

    /// Input использует wide-gamut metadata, но Phase 8.5 ещё выводит только SDR BT.709.
    WideGamutToSdrBt709,

    /// Input выглядит как HDR, но текущий renderer не объявляет HDR support.
    UnsupportedHdrInput,
}

/// Диагностическое описание фактически выбранного color path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ActiveColorPath {
    /// Формат decoded frame на входе renderer-а.
    pub input_format: VideoFrameFormat,

    /// Bit depth decoded frame на входе renderer-а.
    pub input_bit_depth: BitDepth,

    /// Chroma subsampling decoded frame на входе renderer-а.
    pub input_chroma: ChromaSubsampling,

    /// Typed color metadata decoded frame на входе renderer-а.
    pub input_color: RenderColorMetadata,

    /// Output color space, куда текущий renderer готовит изображение.
    pub output_color_space: RenderOutputColorSpace,

    /// Tone mapping mode, выбранный settings.
    pub tone_mapping: ToneMappingMode,

    /// Swapchain transfer mode, выбранный settings.
    pub swapchain_transfer: SwapchainTransferMode,

    /// Fallback marker для честной диагностики временных SDR ограничений.
    pub fallback: Option<ActiveColorPathFallback>,
}

impl ActiveColorPath {
    /// Строит active color path из renderer-neutral frame metadata и settings.
    #[must_use]
    pub fn from_frame(frame: &RenderableFrame, settings: &ColorPipelineSettings) -> Self {
        Self::from_parts(
            frame.format,
            frame.bit_depth,
            frame.chroma,
            frame.color.clone(),
            settings,
        )
    }

    /// Строит active color path без зависимости от конкретного frame struct.
    #[must_use]
    pub fn from_parts(
        input_format: VideoFrameFormat,
        input_bit_depth: BitDepth,
        input_chroma: ChromaSubsampling,
        input_color: RenderColorMetadata,
        settings: &ColorPipelineSettings,
    ) -> Self {
        let fallback = classify_color_path_fallback(&input_color);

        Self {
            input_format,
            input_bit_depth,
            input_chroma,
            input_color,
            output_color_space: RenderOutputColorSpace::SdrBt709,
            tone_mapping: settings.tone_mapping,
            swapchain_transfer: settings.swapchain_transfer,
            fallback,
        }
    }

    /// Формирует компактную строку для telemetry/capability diagnostics.
    #[must_use]
    pub fn diagnostic_text(&self) -> String {
        let fallback_label = if self.fallback.is_some() {
            " fallback"
        } else {
            ""
        };

        format!(
            "{} {} {} {} -> {}{} {}",
            self.input_format,
            self.input_bit_depth,
            matrix_label(self.input_color.matrix),
            range_label(self.input_color.range),
            self.output_color_space,
            fallback_label,
            self.swapchain_transfer,
        )
    }
}

/// Определяет fallback marker без изменения текущего renderer support matrix.
fn classify_color_path_fallback(
    color_metadata: &RenderColorMetadata,
) -> Option<ActiveColorPathFallback> {
    if color_metadata.hdr_metadata.is_some()
        || matches!(
            color_metadata.transfer,
            TransferFunction::Pq | TransferFunction::Hlg
        )
    {
        return Some(ActiveColorPathFallback::UnsupportedHdrInput);
    }

    if color_metadata.range == ColorRange::Unknown
        || color_metadata.matrix == MatrixCoefficients::Unknown
        || color_metadata.primaries == ColorPrimaries::Unknown
        || color_metadata.transfer == TransferFunction::Unknown
    {
        return Some(ActiveColorPathFallback::UnknownInputMetadata);
    }

    if color_metadata.matrix == MatrixCoefficients::Bt2020
        || color_metadata.primaries == ColorPrimaries::Bt2020
    {
        return Some(ActiveColorPathFallback::WideGamutToSdrBt709);
    }

    None
}

/// Возвращает короткую подпись matrix coefficients для active path diagnostics.
fn matrix_label(matrix_coefficients: MatrixCoefficients) -> &'static str {
    match matrix_coefficients {
        MatrixCoefficients::Bt601 => "BT.601",
        MatrixCoefficients::Bt709 => "BT.709",
        MatrixCoefficients::Bt2020 => "BT.2020",
        MatrixCoefficients::Unknown => "unknown-matrix",
    }
}

/// Возвращает короткую подпись range для active path diagnostics.
fn range_label(color_range: ColorRange) -> &'static str {
    match color_range {
        ColorRange::Limited => "limited",
        ColorRange::Full => "full",
        ColorRange::Unknown => "unknown-range",
    }
}

/// Способ композиции UI относительно video pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiCompositionMode {
    /// UI рисуется поверх video pass в тот же swapchain frame.
    Overlay,

    /// Backend не занимается UI; shell использует отдельный путь.
    External,
}

/// Renderer-neutral описание кадра, готового к presentation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderableFrame {
    /// Opaque handle исходного decoded frame для связи с decoder texture pool.
    pub handle: u64,

    /// Presentation timestamp кадра.
    pub pts: Duration,

    /// Формат входных texture planes или готового RGB image.
    pub format: VideoFrameFormat,

    /// Bit depth decoded frame на render boundary.
    pub bit_depth: BitDepth,

    /// Chroma subsampling decoded frame на render boundary.
    pub chroma: ChromaSubsampling,

    /// Coded width из decoder-а.
    pub coded_width: u32,

    /// Coded height из decoder-а.
    pub coded_height: u32,

    /// Display width после crop/aspect handling.
    pub render_width: u32,

    /// Display height после crop/aspect handling.
    pub render_height: u32,

    /// Typed color metadata кадра.
    pub color: RenderColorMetadata,
}

impl RenderableFrame {
    /// Возвращает `true`, если frame содержит ненулевой display size.
    #[must_use]
    pub const fn has_display_size(&self) -> bool {
        self.render_width > 0 && self.render_height > 0
    }
}

/// Возможности одного renderer backend-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderCapabilities {
    /// Семейство backend-а.
    pub backend: RenderBackendKind,

    /// Человекочитаемое имя backend-а.
    pub display_name: String,

    /// Форматы decoded frame, которые backend может принять.
    pub supported_frame_formats: Vec<VideoFrameFormat>,

    /// Может ли backend выполнить HDR-to-SDR tone mapping.
    pub supports_hdr_to_sdr: bool,

    /// Может ли backend отдать native HDR output в swapchain/display.
    pub supports_native_hdr_output: bool,

    /// Максимальный размер 2D texture, если backend его сообщил.
    pub max_texture_size: Option<u32>,

    /// Поддерживает ли backend расширенный UI overlay.
    pub advanced_ui: bool,

    /// Как backend композитит UI.
    pub ui_composition_mode: UiCompositionMode,

    /// Есть ли у backend-а метрики present/frame pacing.
    pub present_timing_metrics: bool,
}

impl RenderCapabilities {
    /// Создаёт capabilities для текущего WGPU MVP backend-а.
    #[must_use]
    pub fn wgpu_nv12(max_texture_size: Option<u32>) -> Self {
        Self {
            backend: RenderBackendKind::Wgpu,
            display_name: "WGPU NV12 renderer".to_string(),
            supported_frame_formats: vec![VideoFrameFormat::Nv12],
            supports_hdr_to_sdr: false,
            supports_native_hdr_output: false,
            max_texture_size,
            advanced_ui: true,
            ui_composition_mode: UiCompositionMode::Overlay,
            present_timing_metrics: true,
        }
    }

    /// Проверяет прямую поддержку входного frame format.
    #[must_use]
    pub fn supports_frame_format(&self, format: VideoFrameFormat) -> bool {
        self.supported_frame_formats.contains(&format)
    }

    /// Проверяет, сможет ли renderer показать stream с указанными требованиями.
    #[must_use]
    pub fn supports_decode_requirement(&self, requirement: &VideoDecodeRequirement) -> bool {
        if requirement.hdr && !(self.supports_hdr_to_sdr || self.supports_native_hdr_output) {
            return false;
        }

        let Some(frame_format) = VideoFrameFormat::from_decode_requirement(requirement) else {
            return false;
        };

        if !self.supports_frame_format(frame_format) {
            return false;
        }

        if let (Some(width), Some(max_texture_size)) = (requirement.width, self.max_texture_size)
            && width > max_texture_size
        {
            return false;
        }

        if let (Some(height), Some(max_texture_size)) = (requirement.height, self.max_texture_size)
            && height > max_texture_size
        {
            return false;
        }

        true
    }

    /// Формирует одну строку diagnostics для capability report.
    #[must_use]
    pub fn summary_text(&self) -> String {
        let frame_formats = self
            .supported_frame_formats
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");

        let hdr_support_label = if self.supports_hdr_to_sdr || self.supports_native_hdr_output {
            "HDR available"
        } else {
            "SDR only, HDR unavailable"
        };

        let p010_support_label = if self.supports_frame_format(VideoFrameFormat::P010) {
            "P010 input available"
        } else {
            "P010 input unavailable"
        };

        format!(
            "{}: {}, {}, formats: {}, max texture: {}",
            self.display_name,
            hdr_support_label,
            p010_support_label,
            frame_formats,
            self.max_texture_size
                .map(|size| size.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )
    }
}

#[cfg(test)]
mod tests {
    use codec_core::{
        BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients,
        TransferFunction, VideoCodec, VideoColorMetadata, VideoDecodeRequirement,
    };

    use super::*;

    #[test]
    fn eight_bit_yuv420_requirement_maps_to_nv12() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420);

        assert_eq!(
            VideoFrameFormat::from_decode_requirement(&requirement),
            Some(VideoFrameFormat::Nv12)
        );
    }

    #[test]
    fn ten_bit_requirement_maps_to_p010() {
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

        assert_eq!(
            VideoFrameFormat::from_decode_requirement(&requirement),
            Some(VideoFrameFormat::P010)
        );
    }

    #[test]
    fn color_pipeline_settings_default_is_identity() {
        let settings = ColorPipelineSettings::default();

        assert_eq!(settings.adjustment, ColorAdjustment::identity());
        assert_eq!(settings.tone_mapping, ToneMappingMode::Off);
        assert_eq!(
            settings.swapchain_transfer,
            SwapchainTransferMode::PreserveCurrentUnorm
        );
        assert!(settings.is_identity());
    }

    #[test]
    fn active_color_path_describes_current_nv12_bt709_limited_sdr_path() {
        let frame = RenderableFrame {
            handle: 7,
            pts: Duration::ZERO,
            format: VideoFrameFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            coded_width: 1920,
            coded_height: 1080,
            render_width: 1920,
            render_height: 1080,
            color: VideoColorMetadata::sdr_bt709_limited(),
        };
        let settings = ColorPipelineSettings::default();

        let active_path = ActiveColorPath::from_frame(&frame, &settings);

        assert_eq!(active_path.input_format, VideoFrameFormat::Nv12);
        assert_eq!(active_path.input_bit_depth, BitDepth::Eight);
        assert_eq!(active_path.input_chroma, ChromaSubsampling::Yuv420);
        assert_eq!(active_path.input_color.range, ColorRange::Limited);
        assert_eq!(active_path.input_color.matrix, MatrixCoefficients::Bt709);
        assert_eq!(active_path.fallback, None);
        assert_eq!(
            active_path.diagnostic_text(),
            "NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm"
        );
    }

    #[test]
    fn active_color_path_marks_bt2020_sdr_as_sdr_bt709_fallback() {
        let color = VideoColorMetadata {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt2020,
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Bt709,
            hdr_metadata: None,
            origin: codec_core::ColorMetadataOrigin::Container,
            confidence: codec_core::ColorMetadataConfidence::Hint,
        };
        let settings = ColorPipelineSettings::default();

        let active_path = ActiveColorPath::from_parts(
            VideoFrameFormat::Nv12,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
            color,
            &settings,
        );

        assert_eq!(
            active_path.fallback,
            Some(ActiveColorPathFallback::WideGamutToSdrBt709)
        );
        assert_eq!(
            active_path.diagnostic_text(),
            "NV12 8-bit BT.2020 limited -> SDR BT.709 fallback preserve-current-unorm"
        );
    }

    #[test]
    fn current_wgpu_capabilities_do_not_advertise_p010_or_hdr() {
        let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));

        assert!(capabilities.supports_frame_format(VideoFrameFormat::Nv12));
        assert!(!capabilities.supports_frame_format(VideoFrameFormat::P010));
        assert!(!capabilities.supports_hdr_to_sdr);
        assert!(!capabilities.supports_native_hdr_output);
        assert!(capabilities.summary_text().contains("SDR only"));
        assert!(
            capabilities
                .summary_text()
                .contains("P010 input unavailable")
        );
        assert!(!capabilities.summary_text().contains("HDR supported"));
    }

    #[test]
    fn current_wgpu_capabilities_reject_p010_until_hdr_phase() {
        let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

        assert!(!capabilities.supports_decode_requirement(&requirement));
    }

    #[test]
    fn current_wgpu_capabilities_reject_ten_bit_hdr_requirement() {
        let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
        let mut requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);
        requirement.hdr = true;

        assert!(!capabilities.supports_decode_requirement(&requirement));
    }
}
