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
    VideoColorMetadata, VideoDecodeRequirement, VideoSurfaceFormat,
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

/// Compatibility alias: renderer использует общий codec-neutral surface contract.
pub type VideoFrameFormat = VideoSurfaceFormat;

/// Состояние P010 на границе decoder/renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum P010RenderReadiness {
    /// P010 path недоступен даже как проверенная zero-copy граница.
    Unavailable,

    /// DMA-BUF/P010 boundary проверен диагностически, но production renderer ещё не рисует P010.
    ZeroCopyBoundaryVerified,

    /// Renderer умеет принять P010 и вывести его в production path.
    Renderable,
}

impl P010RenderReadiness {
    /// Возвращает `true`, только если P010 можно выбирать для production playback.
    #[must_use]
    pub const fn is_renderable(self) -> bool {
        matches!(self, Self::Renderable)
    }

    /// Возвращает короткое описание для capability report.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::Unavailable => "P010 unavailable",
            Self::ZeroCopyBoundaryVerified => {
                "P010 zero-copy boundary verified, render unavailable"
            }
            Self::Renderable => "P010 renderable",
        }
    }
}

impl fmt::Display for P010RenderReadiness {
    /// Печатает стабильное diagnostic-описание P010 readiness.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
    }
}

impl Default for P010RenderReadiness {
    /// Старые reports без P010 readiness безопасно считаются production-неготовыми.
    fn default() -> Self {
        Self::Unavailable
    }
}

/// Storage layout, через который P010 DMA-BUF попадает на renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum P010StorageLayout {
    /// Основной Phase 10 layout: VA отдаёт luma/chroma как R16 + Rg16 textures.
    BaselineSeparateLayer,

    /// Compatibility layout: VA отдаёт один composed `TextureFormat::P010`.
    CompatibilityComposed,
}

impl P010StorageLayout {
    /// Возвращает stable label layout-а для capability report и ошибок.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::BaselineSeparateLayer => "baseline separate-layer R16/Rg16",
            Self::CompatibilityComposed => "compatibility composed P010",
        }
    }

    /// Возвращает wgpu feature, без которого этот layout нельзя импортировать.
    #[must_use]
    pub const fn required_wgpu_feature_label(self) -> &'static str {
        match self {
            Self::BaselineSeparateLayer => "TEXTURE_FORMAT_16BIT_NORM",
            Self::CompatibilityComposed => "TEXTURE_FORMAT_P010",
        }
    }
}

impl fmt::Display for P010StorageLayout {
    /// Печатает stable layout label без backend-specific Debug-шума.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
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

/// Источник optional HDR metadata, который renderer использовал для diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrMetadataDiagnosticMarker {
    /// Поле не применимо к текущему color path.
    NotApplicable,

    /// Значение пришло из container/bitstream/backend metadata.
    Confirmed,

    /// Значение заменено documented reference default-ом.
    ReferenceDefault,
}

impl HdrMetadataDiagnosticMarker {
    /// Возвращает стабильную подпись для telemetry panel.
    #[must_use]
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::NotApplicable => "not-applicable",
            Self::Confirmed => "confirmed",
            Self::ReferenceDefault => "reference-default",
        }
    }
}

impl fmt::Display for HdrMetadataDiagnosticMarker {
    /// Печатает marker без UI-специфичного форматирования.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_label())
    }
}

/// Source markers для optional HDR metadata, использованной HDR-to-SDR path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HdrReferenceDefaultDiagnostics {
    /// Mastering display max luminance source.
    pub mastering_max_luminance: HdrMetadataDiagnosticMarker,

    /// Mastering display min luminance source.
    pub mastering_min_luminance: HdrMetadataDiagnosticMarker,

    /// MaxCLL source.
    pub max_content_light_level: HdrMetadataDiagnosticMarker,

    /// MaxFALL source.
    pub max_frame_average_light_level: HdrMetadataDiagnosticMarker,
}

impl HdrReferenceDefaultDiagnostics {
    /// Возвращает `true`, если хотя бы одно поле взято из reference defaults.
    #[must_use]
    pub const fn has_reference_defaults(&self) -> bool {
        matches!(
            self.mastering_max_luminance,
            HdrMetadataDiagnosticMarker::ReferenceDefault
        ) || matches!(
            self.mastering_min_luminance,
            HdrMetadataDiagnosticMarker::ReferenceDefault
        ) || matches!(
            self.max_content_light_level,
            HdrMetadataDiagnosticMarker::ReferenceDefault
        ) || matches!(
            self.max_frame_average_light_level,
            HdrMetadataDiagnosticMarker::ReferenceDefault
        )
    }

    /// Формирует compact diagnostics string для UI.
    #[must_use]
    pub fn diagnostic_text(&self) -> String {
        format!(
            "mastering-max={}, mastering-min={}, maxcll={}, maxfall={}",
            self.mastering_max_luminance,
            self.mastering_min_luminance,
            self.max_content_light_level,
            self.max_frame_average_light_level
        )
    }
}

/// Renderer-neutral diagnostics, которые UI может читать без GPU handles.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderDiagnostics {
    /// Последний color path, реально выбранный renderer-ом для video frame.
    pub active_color_path: Option<ActiveColorPath>,

    /// Source markers optional HDR metadata для последнего HDR-to-SDR frame.
    #[serde(default)]
    pub hdr_reference_defaults: Option<HdrReferenceDefaultDiagnostics>,
}

impl RenderDiagnostics {
    /// Возвращает строку active color path для telemetry panel.
    #[must_use]
    pub fn active_color_path_text(&self) -> Option<String> {
        self.active_color_path
            .as_ref()
            .map(ActiveColorPath::diagnostic_text)
    }

    /// Возвращает source markers optional HDR metadata для telemetry panel.
    #[must_use]
    pub fn hdr_reference_defaults_text(&self) -> Option<String> {
        self.hdr_reference_defaults
            .as_ref()
            .map(HdrReferenceDefaultDiagnostics::diagnostic_text)
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
        input_format: VideoFrameFormat,
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

    /// Строит active color path с явным HDR-to-SDR contract для production P010 path.
    #[must_use]
    pub fn from_parts_with_hdr_to_sdr(
        input_format: VideoFrameFormat,
        input_bit_depth: BitDepth,
        input_chroma: ChromaSubsampling,
        input_color: RenderColorMetadata,
        settings: &ColorPipelineSettings,
        hdr_to_sdr: Option<HdrToSdrSettings>,
    ) -> Self {
        let fallback = classify_color_path_fallback(input_format, &input_color, hdr_to_sdr);
        let active_hdr_to_sdr = hdr_to_sdr.filter(|settings| {
            is_hdr_input(&input_color)
                && input_format == VideoFrameFormat::P010
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
    input_format: VideoFrameFormat,
    color_metadata: &RenderColorMetadata,
    hdr_to_sdr: Option<HdrToSdrSettings>,
) -> Option<ActiveColorPathFallback> {
    if is_hdr_input(color_metadata) {
        if input_format == VideoFrameFormat::P010
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

    /// Состояние P010: diagnostic boundary отдельно от production renderability.
    #[serde(default)]
    pub p010_render_readiness: P010RenderReadiness,

    /// P010 storage layouts, которые backend может импортировать без CPU fallback.
    #[serde(default)]
    pub supported_p010_storage_layouts: Vec<P010StorageLayout>,

    /// HDR-to-SDR operators, которые backend реально реализует в production shader path.
    #[serde(default)]
    pub supported_hdr_to_sdr_operators: Vec<HdrToneMappingOperator>,

    /// HDR output mode backend-а; Phase 10 разрешает только SDR BT.709.
    #[serde(default)]
    pub hdr_output_mode: HdrOutputMode,

    /// Raw claim backend-а: может ли он выполнить HDR-to-SDR tone mapping.
    ///
    /// Production selection должна использовать `supports_hdr_to_sdr_with`, потому что
    /// один этот flag не доказывает P010 renderer и конкретный operator.
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
            p010_render_readiness: P010RenderReadiness::Unavailable,
            supported_p010_storage_layouts: Vec::new(),
            supported_hdr_to_sdr_operators: Vec::new(),
            hdr_output_mode: HdrOutputMode::SdrBt709Only,
            supports_hdr_to_sdr: false,
            supports_native_hdr_output: false,
            max_texture_size,
            advanced_ui: true,
            ui_composition_mode: UiCompositionMode::Overlay,
            present_timing_metrics: true,
        }
    }

    /// Создаёт capabilities для WGPU renderer-а с production P010 BT.2446-C path.
    #[must_use]
    pub fn wgpu_p010_bt2446c(max_texture_size: Option<u32>) -> Self {
        Self::wgpu_p010_bt2446c_with_storage_layouts(
            max_texture_size,
            vec![
                P010StorageLayout::BaselineSeparateLayer,
                P010StorageLayout::CompatibilityComposed,
            ],
        )
    }

    /// Создаёт capabilities для WGPU P010 renderer-а с явными import layout-ами.
    #[must_use]
    pub fn wgpu_p010_bt2446c_with_storage_layouts(
        max_texture_size: Option<u32>,
        supported_p010_storage_layouts: Vec<P010StorageLayout>,
    ) -> Self {
        Self {
            backend: RenderBackendKind::Wgpu,
            display_name: "WGPU P010 BT.2446-C renderer".to_string(),
            supported_frame_formats: vec![VideoFrameFormat::Nv12, VideoFrameFormat::P010],
            p010_render_readiness: P010RenderReadiness::Renderable,
            supported_p010_storage_layouts,
            supported_hdr_to_sdr_operators: vec![HdrToneMappingOperator::Bt2446C],
            hdr_output_mode: HdrOutputMode::SdrBt709Only,
            supports_hdr_to_sdr: true,
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

    /// Проверяет, что P010 доступен именно как production-renderable path.
    #[must_use]
    pub fn supports_p010_rendering(&self) -> bool {
        self.p010_render_readiness.is_renderable()
            && self.supports_frame_format(VideoFrameFormat::P010)
            && !self.supported_p010_storage_layouts.is_empty()
    }

    /// Проверяет, что renderer умеет импортировать конкретный P010 storage layout.
    #[must_use]
    pub fn supports_p010_storage_layout(&self, layout: P010StorageLayout) -> bool {
        self.supports_p010_rendering() && self.supported_p010_storage_layouts.contains(&layout)
    }

    /// Проверяет production-ready HDR-to-SDR support для конкретных settings.
    #[must_use]
    pub fn supports_hdr_to_sdr_with(&self, settings: &HdrToSdrSettings) -> bool {
        self.supports_hdr_to_sdr
            && self.supports_p010_rendering()
            && settings.is_phase10_bt2446_c_sdr_bt709()
            && self.hdr_output_mode == settings.output_mode
            && self
                .supported_hdr_to_sdr_operators
                .contains(&settings.operator)
    }

    /// Проверяет, сможет ли renderer показать stream с указанными требованиями.
    #[must_use]
    pub fn supports_decode_requirement(&self, requirement: &VideoDecodeRequirement) -> bool {
        if requirement.hdr
            && !(self.supports_hdr_to_sdr_with(&HdrToSdrSettings::default())
                || self.supports_native_hdr_output)
        {
            return false;
        }

        let Some(frame_format) = VideoFrameFormat::from_decode_requirement(requirement) else {
            return false;
        };

        if frame_format == VideoFrameFormat::P010 && !self.supports_p010_rendering() {
            return false;
        }

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
        let p010_layouts = if self.supported_p010_storage_layouts.is_empty() {
            "none".to_string()
        } else {
            self.supported_p010_storage_layouts
                .iter()
                .map(|layout| layout.diagnostic_label())
                .collect::<Vec<_>>()
                .join(", ")
        };

        let hdr_support_label = if self.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()) {
            "HDR available via HDR-to-SDR"
        } else if self.supports_native_hdr_output {
            "HDR available via native HDR output"
        } else {
            "SDR only, HDR unavailable"
        };
        let native_hdr_label = if self.supports_native_hdr_output {
            "native HDR supported"
        } else {
            "native HDR unsupported"
        };

        format!(
            "{}: {}, {}, {}, formats: {}, P010 layouts: {}, max texture: {}",
            self.display_name,
            hdr_support_label,
            native_hdr_label,
            self.p010_render_readiness,
            frame_formats,
            p010_layouts,
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
    fn hdr_to_sdr_settings_default_to_bt2446c_sdr_bt709_contract() {
        let settings = HdrToSdrSettings::default();

        assert!(settings.enabled);
        assert_eq!(settings.operator, HdrToneMappingOperator::Bt2446C);
        assert_eq!(settings.output_mode, HdrOutputMode::SdrBt709Only);
        assert_eq!(settings.sdr_reference_white_nits, 100.0);
        assert_eq!(settings.hdr_reference_peak_nits, 1_000.0);
        assert!(settings.is_phase10_bt2446_c_sdr_bt709());
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
    fn active_color_path_describes_p010_hdr_to_sdr_bt2446c_path() {
        let color = VideoColorMetadata {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt2020,
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            hdr_metadata: None,
            origin: codec_core::ColorMetadataOrigin::Bitstream,
            confidence: codec_core::ColorMetadataConfidence::Confirmed,
        };
        let settings = ColorPipelineSettings {
            swapchain_transfer: SwapchainTransferMode::ExplicitShaderOetf,
            ..ColorPipelineSettings::default()
        };

        let active_path = ActiveColorPath::from_parts_with_hdr_to_sdr(
            VideoFrameFormat::P010,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            color,
            &settings,
            Some(HdrToSdrSettings::default()),
        );

        assert_eq!(active_path.fallback, None);
        assert_eq!(
            active_path.hdr_to_sdr.map(|settings| settings.operator),
            Some(HdrToneMappingOperator::Bt2446C)
        );
        assert_eq!(
            active_path.diagnostic_text(),
            "P010 10-bit BT.2020 PQ limited -> SDR BT.709 bt2446-c explicit-shader-oetf"
        );
    }

    #[test]
    fn active_color_path_keeps_hdr_fallback_without_explicit_hdr_to_sdr_contract() {
        let color = VideoColorMetadata {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt2020,
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Pq,
            hdr_metadata: None,
            origin: codec_core::ColorMetadataOrigin::Bitstream,
            confidence: codec_core::ColorMetadataConfidence::Confirmed,
        };
        let settings = ColorPipelineSettings::default();

        let active_path = ActiveColorPath::from_parts(
            VideoFrameFormat::P010,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            color,
            &settings,
        );

        assert_eq!(
            active_path.fallback,
            Some(ActiveColorPathFallback::UnsupportedHdrInput)
        );
        assert_eq!(active_path.hdr_to_sdr, None);
    }

    #[test]
    fn active_color_path_treats_bt709_content_light_side_metadata_as_sdr() {
        let color = VideoColorMetadata {
            range: ColorRange::Limited,
            matrix: MatrixCoefficients::Bt2020,
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferFunction::Bt709,
            hdr_metadata: Some(codec_core::HdrMetadata {
                color_primaries: ColorPrimaries::Bt2020,
                transfer_function: TransferFunction::Bt709,
                max_luminance_nits: Some(1_000.0),
                min_luminance_nits: Some(0.01),
                max_content_light_level_nits: Some(1_000),
                max_frame_average_light_level_nits: Some(400),
            }),
            origin: codec_core::ColorMetadataOrigin::Container,
            confidence: codec_core::ColorMetadataConfidence::Hint,
        };
        let settings = ColorPipelineSettings {
            swapchain_transfer: SwapchainTransferMode::ExplicitShaderOetf,
            ..ColorPipelineSettings::default()
        };

        let active_path = ActiveColorPath::from_parts_with_hdr_to_sdr(
            VideoFrameFormat::P010,
            BitDepth::Ten,
            ChromaSubsampling::Yuv420,
            color,
            &settings,
            Some(HdrToSdrSettings::default()),
        );

        assert_eq!(
            active_path.fallback,
            Some(ActiveColorPathFallback::WideGamutToSdrBt709)
        );
        assert_eq!(active_path.hdr_to_sdr, None);
        assert!(!active_path.diagnostic_text().contains("bt2446-c"));
    }

    #[test]
    fn render_diagnostics_exposes_active_color_path_without_gpu_handles() {
        let settings = ColorPipelineSettings::default();
        let active_path = ActiveColorPath::from_parts(
            VideoFrameFormat::Nv12,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
            VideoColorMetadata::sdr_bt709_limited(),
            &settings,
        );
        let diagnostics = RenderDiagnostics {
            active_color_path: Some(active_path),
            hdr_reference_defaults: None,
        };

        assert_eq!(
            diagnostics.active_color_path_text().as_deref(),
            Some("NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm")
        );
    }

    #[test]
    fn current_wgpu_capabilities_do_not_advertise_p010_or_hdr() {
        let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));

        assert!(capabilities.supports_frame_format(VideoFrameFormat::Nv12));
        assert!(!capabilities.supports_frame_format(VideoFrameFormat::P010));
        assert_eq!(
            capabilities.p010_render_readiness,
            P010RenderReadiness::Unavailable
        );
        assert!(capabilities.supported_hdr_to_sdr_operators.is_empty());
        assert_eq!(capabilities.hdr_output_mode, HdrOutputMode::SdrBt709Only);
        assert!(!capabilities.supports_p010_rendering());
        assert!(!capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
        assert!(!capabilities.supports_hdr_to_sdr);
        assert!(!capabilities.supports_native_hdr_output);
        assert!(capabilities.summary_text().contains("SDR only"));
        assert!(
            capabilities
                .summary_text()
                .contains("native HDR unsupported")
        );
        assert!(capabilities.summary_text().contains("P010 unavailable"));
        assert!(!capabilities.summary_text().contains("HDR supported"));
    }

    #[test]
    fn hdr_to_sdr_capability_requires_p010_renderable_and_bt2446c_operator() {
        let settings = HdrToSdrSettings::default();

        let mut raw_hdr_without_p010 = RenderCapabilities::wgpu_nv12(Some(4096));
        raw_hdr_without_p010.supports_hdr_to_sdr = true;
        raw_hdr_without_p010
            .supported_hdr_to_sdr_operators
            .push(HdrToneMappingOperator::Bt2446C);

        let mut p010_without_operator = RenderCapabilities::wgpu_nv12(Some(4096));
        p010_without_operator
            .supported_frame_formats
            .push(VideoFrameFormat::P010);
        p010_without_operator.p010_render_readiness = P010RenderReadiness::Renderable;
        p010_without_operator
            .supported_p010_storage_layouts
            .push(P010StorageLayout::BaselineSeparateLayer);
        p010_without_operator.supports_hdr_to_sdr = true;

        let production_capabilities = RenderCapabilities::wgpu_p010_bt2446c(Some(4096));

        assert!(!raw_hdr_without_p010.supports_hdr_to_sdr_with(&settings));
        assert!(!p010_without_operator.supports_hdr_to_sdr_with(&settings));
        assert!(production_capabilities.supports_hdr_to_sdr_with(&settings));
        assert!(!production_capabilities.supports_native_hdr_output);
    }

    #[test]
    fn p010_zero_copy_boundary_state_is_not_renderable() {
        let mut capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
        capabilities.p010_render_readiness = P010RenderReadiness::ZeroCopyBoundaryVerified;
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

        assert!(!capabilities.supports_p010_rendering());
        assert!(!capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
        assert!(!capabilities.supports_decode_requirement(&requirement));
        assert!(
            capabilities
                .summary_text()
                .contains("P010 zero-copy boundary verified")
        );
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

    #[test]
    fn p010_bt2446c_capabilities_accept_ten_bit_hdr_but_not_native_hdr_output() {
        let capabilities = RenderCapabilities::wgpu_p010_bt2446c(Some(4096));
        let mut requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420);
        requirement.hdr = true;

        assert!(capabilities.supports_decode_requirement(&requirement));
        assert!(capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
        assert!(!capabilities.supports_native_hdr_output);
        assert!(capabilities.summary_text().contains("HDR available"));
        assert!(
            capabilities
                .summary_text()
                .contains("native HDR unsupported")
        );
    }
}
