use std::fmt;

use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction,
    VideoColorMetadata,
};
use serde::{Deserialize, Serialize};
use video_frame_contract::VideoFramePixelLayout;

use crate::RenderableFrame;

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

/// HDR tone mapping operator, который production renderer может явно поддержать.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrToneMappingOperator {
    /// ITU-R BT.2446 Method C: единственный production baseline Phase 10.
    Bt2446C,
}

impl HdrToneMappingOperator {
    /// Возвращает стабильный id operator-а для config, логов и diagnostics.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::Bt2446C => "bt2446-c",
        }
    }
}

impl Default for HdrToneMappingOperator {
    /// Phase 10 по умолчанию использует утверждённый BT.2446 Method C baseline.
    fn default() -> Self {
        Self::Bt2446C
    }
}

impl fmt::Display for HdrToneMappingOperator {
    /// Печатает стабильный id operator-а без UI-специфичного форматирования.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.stable_id())
    }
}

/// Output mode для HDR renderer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrOutputMode {
    /// Phase 10 выводит только SDR BT.709; native HDR output остаётся future scope.
    SdrBt709Only,
}

impl HdrOutputMode {
    /// Возвращает output color space, который соответствует этому HDR output mode.
    #[must_use]
    pub const fn output_color_space(self) -> RenderOutputColorSpace {
        match self {
            Self::SdrBt709Only => RenderOutputColorSpace::SdrBt709,
        }
    }

    /// Возвращает стабильный id режима для config, логов и diagnostics.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::SdrBt709Only => "sdr-bt709-only",
        }
    }
}

impl Default for HdrOutputMode {
    /// Без platform HDR negotiation renderer обязан оставаться в SDR BT.709 output.
    fn default() -> Self {
        Self::SdrBt709Only
    }
}

impl fmt::Display for HdrOutputMode {
    /// Печатает стабильный id output mode.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.stable_id())
    }
}

/// Typed settings для HDR-to-SDR path.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HdrToSdrSettings {
    /// Включает HDR-to-SDR path, если renderer capabilities тоже подтверждают support.
    pub enabled: bool,

    /// Tone mapping operator, выбранный для HDR-to-SDR conversion.
    pub operator: HdrToneMappingOperator,

    /// Единственный output mode Phase 10: SDR BT.709.
    pub output_mode: HdrOutputMode,

    /// SDR reference white в nits для BT.2446-C baseline.
    pub sdr_reference_white_nits: f32,

    /// HDR reference peak в nits для BT.2446-C baseline.
    pub hdr_reference_peak_nits: f32,
}

impl HdrToSdrSettings {
    /// Создаёт documented Phase 10 defaults из архитектурного плана.
    #[must_use]
    pub const fn bt2446_c_sdr_bt709() -> Self {
        Self {
            enabled: true,
            operator: HdrToneMappingOperator::Bt2446C,
            output_mode: HdrOutputMode::SdrBt709Only,
            sdr_reference_white_nits: 100.0,
            hdr_reference_peak_nits: 1_000.0,
        }
    }

    /// Проверяет, что settings описывают единственный production HDR path Phase 10.
    #[must_use]
    pub fn is_phase10_bt2446_c_sdr_bt709(&self) -> bool {
        self.enabled
            && self.operator == HdrToneMappingOperator::Bt2446C
            && self.output_mode == HdrOutputMode::SdrBt709Only
            && self.sdr_reference_white_nits.is_finite()
            && self.sdr_reference_white_nits > 0.0
            && self.hdr_reference_peak_nits.is_finite()
            && self.hdr_reference_peak_nits > self.sdr_reference_white_nits
    }
}

impl Default for HdrToSdrSettings {
    /// Делает HDR-to-SDR включённым в config contract, но gated renderer capabilities.
    fn default() -> Self {
        Self::bt2446_c_sdr_bt709()
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
    pub input_format: VideoFramePixelLayout,

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

    /// HDR-to-SDR settings, если frame идёт через production HDR path.
    #[serde(default)]
    pub hdr_to_sdr: Option<HdrToSdrSettings>,

    /// Swapchain transfer mode, выбранный settings.
    pub swapchain_transfer: SwapchainTransferMode,

    /// Fallback marker для честной диагностики временных SDR ограничений.
    pub fallback: Option<ActiveColorPathFallback>,
}

impl ActiveColorPath {
    /// Строит active color path из renderer-neutral frame metadata и settings.
    #[must_use]
    pub fn from_frame(frame: &RenderableFrame, settings: &ColorPipelineSettings) -> Self {
        Self::from_frame_with_hdr_to_sdr(frame, settings, None)
    }

    /// Строит active color path для renderer-а, который явно поддерживает HDR-to-SDR.
    #[must_use]
    pub fn from_frame_with_hdr_to_sdr(
        frame: &RenderableFrame,
        settings: &ColorPipelineSettings,
        hdr_to_sdr: Option<HdrToSdrSettings>,
    ) -> Self {
        Self::from_parts_with_hdr_to_sdr(
            frame.format,
            frame.bit_depth,
            frame.chroma,
            frame.color.clone(),
            settings,
            hdr_to_sdr,
        )
    }

    /// Строит active color path без зависимости от конкретного frame struct.
    #[must_use]
    pub fn from_parts(
        input_format: VideoFramePixelLayout,
        input_bit_depth: BitDepth,
        input_chroma: ChromaSubsampling,
        input_color: RenderColorMetadata,
        settings: &ColorPipelineSettings,
    ) -> Self {
        Self::from_parts_with_hdr_to_sdr(
            input_format,
            input_bit_depth,
            input_chroma,
            input_color,
            settings,
            None,
        )
    }

    /// Строит active color path с явным HDR-to-SDR contract для production HDR path-а.
    #[must_use]
    pub fn from_parts_with_hdr_to_sdr(
        input_format: VideoFramePixelLayout,
        input_bit_depth: BitDepth,
        input_chroma: ChromaSubsampling,
        input_color: RenderColorMetadata,
        settings: &ColorPipelineSettings,
        hdr_to_sdr: Option<HdrToSdrSettings>,
    ) -> Self {
        let fallback = classify_color_path_fallback(input_format, &input_color, hdr_to_sdr);
        let active_hdr_to_sdr = hdr_to_sdr.filter(|settings| {
            is_hdr_input(&input_color)
                && pixel_layout_supports_phase10_hdr_to_sdr(input_format)
                && is_phase10_hdr_transfer(&input_color)
                && settings.is_phase10_bt2446_c_sdr_bt709()
                && fallback.is_none()
        });

        Self {
            input_format,
            input_bit_depth,
            input_chroma,
            input_color,
            output_color_space: active_hdr_to_sdr
                .map(|settings| settings.output_mode.output_color_space())
                .unwrap_or(RenderOutputColorSpace::SdrBt709),
            tone_mapping: settings.tone_mapping,
            hdr_to_sdr: active_hdr_to_sdr,
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

        let transfer_label = transfer_path_label(self.input_color.transfer);
        let hdr_to_sdr_label = self
            .hdr_to_sdr
            .map(|settings| format!(" {}", settings.operator))
            .unwrap_or_default();

        format!(
            "{} {} {}{} {} -> {}{}{} {}",
            self.input_format,
            self.input_bit_depth,
            matrix_label(self.input_color.matrix),
            transfer_label,
            range_label(self.input_color.range),
            self.output_color_space,
            fallback_label,
            hdr_to_sdr_label,
            self.swapchain_transfer,
        )
    }
}

/// Определяет fallback marker без изменения текущего renderer support matrix.
fn classify_color_path_fallback(
    input_format: VideoFramePixelLayout,
    color_metadata: &RenderColorMetadata,
    hdr_to_sdr: Option<HdrToSdrSettings>,
) -> Option<ActiveColorPathFallback> {
    if is_hdr_input(color_metadata) {
        if pixel_layout_supports_phase10_hdr_to_sdr(input_format)
            && is_phase10_hdr_transfer(color_metadata)
            && hdr_to_sdr.is_some_and(|settings| settings.is_phase10_bt2446_c_sdr_bt709())
        {
            return None;
        }

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

/// Проверяет, требует ли metadata HDR transfer/tone-mapping path.
fn is_hdr_input(color_metadata: &RenderColorMetadata) -> bool {
    color_metadata.requires_hdr_processing()
}

/// Проверяет, что transfer входит в Phase 10 HDR-to-SDR production baseline.
fn is_phase10_hdr_transfer(color_metadata: &RenderColorMetadata) -> bool {
    matches!(
        color_metadata.transfer,
        TransferFunction::Pq | TransferFunction::Hlg
    )
}

/// Проверяет, что pixel layout имеет production Phase 10 HDR-to-SDR shader path.
pub(crate) fn pixel_layout_supports_phase10_hdr_to_sdr(
    input_format: VideoFramePixelLayout,
) -> bool {
    matches!(
        input_format,
        VideoFramePixelLayout::P010
            | VideoFramePixelLayout::Yuv420Planar10Le
            | VideoFramePixelLayout::Yuv420Planar12Le
            | VideoFramePixelLayout::Yuv422Planar10Le
            | VideoFramePixelLayout::Yuv422Planar12Le
            | VideoFramePixelLayout::Yuv444Planar10Le
    )
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

/// Возвращает transfer label только там, где без него diagnostics теряет HDR смысл.
fn transfer_path_label(transfer_function: TransferFunction) -> &'static str {
    match transfer_function {
        TransferFunction::Pq => " PQ",
        TransferFunction::Hlg => " HLG",
        TransferFunction::Srgb => " sRGB",
        TransferFunction::Bt709 | TransferFunction::Unknown => "",
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
#[cfg(test)]
#[path = "tests/color.rs"]
mod tests;
