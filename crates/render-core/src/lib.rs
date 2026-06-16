//! Общие контракты render layer.
//!
//! Этот crate не создаёт GPU-ресурсы и не зависит от `wgpu`, `egui` или windowing.
//! Его задача — описать факты, которые нужны player/capability layer для выбора
//! воспроизводимого потока до запуска decode.

#![forbid(unsafe_code)]

use std::{borrow::Cow, collections::BTreeSet, error::Error, fmt, time::Duration};

use codec_core::{
    BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction,
    VideoColorMetadata, VideoDecodeRequirement, VideoDisplayOrientation,
};
use serde::{Deserialize, Serialize};
use video_frame_contract::{
    DmaBufImageLayout, FrameBitDepth, FrameChromaSubsampling, HardwareFrameHandle,
    VideoFrameContract, VideoFrameContractValidationError, VideoFramePixelLayout,
    VideoFrameTransferPath,
};

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

    /// Количество scissor draw rects последнего video pass (1 без exclusion rects,
    /// 0 если video pass не рисовал кадр).
    #[serde(default)]
    pub video_draw_rect_count: usize,
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

/// Renderer-neutral область видео в физических пикселях surface target-а.
///
/// App layer вычисляет эту область из layout-а, а concrete renderer сам решает,
/// как применить её к своему backend-у. Поэтому тип не содержит `egui`, `wgpu`
/// или windowing-объекты.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderViewport {
    /// Левый край области видео в physical pixels.
    pub x: u32,

    /// Верхний край области видео в physical pixels.
    pub y: u32,

    /// Ширина области видео в physical pixels.
    pub width: u32,

    /// Высота области видео в physical pixels.
    pub height: u32,
}

impl RenderViewport {
    /// Создаёт viewport без clamp-а; владелец surface должен зажать его перед render pass.
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Возвращает viewport на всю surface.
    #[must_use]
    pub const fn full_surface(surface_width: u32, surface_height: u32) -> Self {
        Self::new(0, 0, surface_width, surface_height)
    }

    /// Размер viewport-а как `(width, height)` для letterbox расчётов.
    #[must_use]
    pub const fn size(self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Возвращает `true`, если viewport не может безопасно принять draw.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Правый край viewport-а с защитой от переполнения.
    #[must_use]
    pub const fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// Нижний край viewport-а с защитой от переполнения.
    #[must_use]
    pub const fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// Возвращает пересечение двух viewport-ов или `None`, если они не пересекаются.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        if self.is_empty() || other.is_empty() {
            return None;
        }

        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());

        if right <= x || bottom <= y {
            return None;
        }

        Some(Self::new(x, y, right - x, bottom - y))
    }

    /// Разбивает viewport на видимые прямоугольники после вычитания `excluded`.
    ///
    /// Метод нужен для UI overlay-ов: video shader сохраняет пропорции по исходному
    /// `self`, а concrete renderer рисует только в возвращённых scissor-областях.
    #[must_use]
    pub fn subtract(self, excluded: Self) -> Vec<Self> {
        let Some(excluded) = self.intersection(excluded) else {
            return vec![self];
        };

        let mut visible_rects = Vec::with_capacity(4);
        let self_right = self.right();
        let self_bottom = self.bottom();
        let excluded_right = excluded.right();
        let excluded_bottom = excluded.bottom();

        if excluded.y > self.y {
            visible_rects.push(Self::new(self.x, self.y, self.width, excluded.y - self.y));
        }

        if excluded_bottom < self_bottom {
            visible_rects.push(Self::new(
                self.x,
                excluded_bottom,
                self.width,
                self_bottom - excluded_bottom,
            ));
        }

        if excluded.x > self.x {
            visible_rects.push(Self::new(
                self.x,
                excluded.y,
                excluded.x - self.x,
                excluded.height,
            ));
        }

        if excluded_right < self_right {
            visible_rects.push(Self::new(
                excluded_right,
                excluded.y,
                self_right - excluded_right,
                excluded.height,
            ));
        }

        visible_rects
    }

    /// Зажимает viewport к surface; некорректный запрос возвращает full-surface fallback.
    ///
    /// Fallback нужен, чтобы отсутствие/сбой layout rect-а не создавали нулевой scissor
    /// и не меняли старое поведение рендера полного окна.
    #[must_use]
    pub fn clamp_to_surface(self, surface_width: u32, surface_height: u32) -> Self {
        let full_surface = Self::full_surface(surface_width, surface_height);
        if self.is_empty() || self.x >= surface_width || self.y >= surface_height {
            return full_surface;
        }

        let clamped_width = self.width.min(surface_width - self.x);
        let clamped_height = self.height.min(surface_height - self.y);
        if clamped_width == 0 || clamped_height == 0 {
            return full_surface;
        }

        Self::new(self.x, self.y, clamped_width, clamped_height)
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

/// Стабильный id shader parameter-а в renderer-neutral live settings contract.
///
/// Id хранится строкой, потому что будущие shader controls будут добавляться без
/// изменения enum-а. Значение всё равно типизируется через descriptor/value ниже.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShaderParameterId(String);

impl ShaderParameterId {
    /// Создаёт новый стабильный id shader parameter-а.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Возвращает id без аллокации для diagnostics и metadata mapping.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Стабильный id enum option-а внутри shader parameter descriptor-а.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShaderParameterOptionId(String);

impl ShaderParameterOptionId {
    /// Создаёт новый стабильный id option-а.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Возвращает id option-а без аллокации.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Тип значения shader parameter-а; UI и runtime не угадывают его из строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShaderParameterValueType {
    /// Boolean shader switch.
    Bool,

    /// Один scalar `f32`.
    Float,

    /// RGB/vector-like triplet из трёх `f32`.
    Float3,

    /// Одно значение из стабильного списка option ids.
    Enum,
}

/// Числовой диапазон shader parameter-а.
///
/// Диапазон нейтральный: здесь нет slider/egui-представления, только контракт
/// значений, которые adapter может безопасно принять.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShaderNumericRange {
    /// Минимальное допустимое значение включительно.
    pub min: f32,

    /// Максимальное допустимое значение включительно.
    pub max: f32,

    /// Optional step для UI/metadata; adapter не обязан квантовать значение.
    pub step: Option<f32>,
}

impl ShaderNumericRange {
    /// Проверяет один `f32` на конечность и попадание в диапазон.
    #[must_use]
    pub fn contains_float(self, value: f32) -> bool {
        value.is_finite() && value >= self.min && value <= self.max
    }

    /// Проверяет `Float3`, применяя один диапазон ко всем каналам.
    #[must_use]
    pub fn contains_float3(self, values: [f32; 3]) -> bool {
        values
            .into_iter()
            .all(|channel_value| self.contains_float(channel_value))
    }
}

/// Типизированное значение shader parameter-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShaderParameterValue {
    /// Boolean shader switch.
    Bool(bool),

    /// Один scalar `f32`.
    Float(f32),

    /// Три `f32`, например RGB gain/offset-like параметр.
    Float3([f32; 3]),

    /// Стабильный enum option id.
    Enum(ShaderParameterOptionId),
}

impl ShaderParameterValue {
    /// Возвращает тип значения без доступа к descriptor registry.
    #[must_use]
    pub const fn value_type(&self) -> ShaderParameterValueType {
        match self {
            Self::Bool(_) => ShaderParameterValueType::Bool,
            Self::Float(_) => ShaderParameterValueType::Float,
            Self::Float3(_) => ShaderParameterValueType::Float3,
            Self::Enum(_) => ShaderParameterValueType::Enum,
        }
    }
}

/// Descriptor одного shader parameter-а.
///
/// Это schema для live shader controls: stable id, тип значения, optional range и
/// default. Такой контракт не требует `HashMap<String, f32>` и остаётся
/// расширяемым для будущих shader passes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShaderParameterDescriptor {
    /// Stable id parameter-а.
    pub id: ShaderParameterId,

    /// Ожидаемый тип значения.
    pub value_type: ShaderParameterValueType,

    /// Optional numeric range только для `Float`/`Float3`.
    pub range: Option<ShaderNumericRange>,

    /// Default value, который должен соответствовать `value_type` и `range`.
    pub default_value: ShaderParameterValue,
}

impl ShaderParameterDescriptor {
    /// Создаёт descriptor без backend-specific state.
    #[must_use]
    pub fn new(
        id: ShaderParameterId,
        value_type: ShaderParameterValueType,
        range: Option<ShaderNumericRange>,
        default_value: ShaderParameterValue,
    ) -> Self {
        Self {
            id,
            value_type,
            range,
            default_value,
        }
    }

    /// Проверяет значение по типу и optional numeric range.
    #[must_use]
    pub fn accepts_value(&self, value: &ShaderParameterValue) -> bool {
        if value.value_type() != self.value_type {
            return false;
        }

        match (self.range, value) {
            (Some(range), ShaderParameterValue::Float(value)) => range.contains_float(*value),
            (Some(range), ShaderParameterValue::Float3(values)) => range.contains_float3(*values),
            (Some(_), ShaderParameterValue::Bool(_) | ShaderParameterValue::Enum(_)) => false,
            (None, _) => true,
        }
    }

    /// Проверяет, что default value descriptor-а сам валиден.
    #[must_use]
    pub fn default_value_is_valid(&self) -> bool {
        self.accepts_value(&self.default_value)
    }
}

/// Одно текущее значение shader parameter-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShaderParameter {
    /// Stable id parameter-а.
    pub id: ShaderParameterId,

    /// Типизированное значение parameter-а.
    pub value: ShaderParameterValue,
}

impl ShaderParameter {
    /// Создаёт parameter value pair.
    #[must_use]
    pub fn new(id: ShaderParameterId, value: ShaderParameterValue) -> Self {
        Self { id, value }
    }
}

/// Набор shader parameter values без backend-specific storage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ShaderParameterSet {
    /// Ordered values. Это не `HashMap<String, f32>`: каждый value несёт свой тип.
    pub parameters: Vec<ShaderParameter>,
}

impl ShaderParameterSet {
    /// Возвращает пустой набор shader parameters.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            parameters: Vec::new(),
        }
    }

    /// Проверяет, что shader parameters отсутствуют.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty()
    }

    /// Ищет parameter по stable id.
    #[must_use]
    pub fn get(&self, id: &ShaderParameterId) -> Option<&ShaderParameter> {
        self.parameters.iter().find(|parameter| parameter.id == *id)
    }

    /// Возвращает stable ids shader parameters, отличающихся от baseline.
    #[must_use]
    pub fn changed_parameter_ids_from(&self, baseline: &Self) -> Vec<ShaderParameterId> {
        let mut candidate_ids = BTreeSet::new();

        for parameter in &self.parameters {
            candidate_ids.insert(parameter.id.clone());
        }

        for parameter in &baseline.parameters {
            candidate_ids.insert(parameter.id.clone());
        }

        candidate_ids
            .into_iter()
            .filter(|id| {
                let current_value = self.get(id).map(|parameter| &parameter.value);
                let baseline_value = baseline.get(id).map(|parameter| &parameter.value);

                current_value != baseline_value
            })
            .collect()
    }
}

/// Field-level id renderer live setting-а.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveSettingId {
    /// `render.color_adjustment.brightness`.
    ColorAdjustmentBrightness,

    /// `render.color_adjustment.contrast`.
    ColorAdjustmentContrast,

    /// `render.color_adjustment.saturation`.
    ColorAdjustmentSaturation,

    /// `render.color_adjustment.exposure`.
    ColorAdjustmentExposure,

    /// `render.color_adjustment.rgb_gain`.
    ColorAdjustmentRgbGain,

    /// `render.color_adjustment.rgb_offset`.
    ColorAdjustmentRgbOffset,

    /// Renderer-neutral tone mapping mode.
    ColorPipelineToneMapping,

    /// Renderer-neutral swapchain transfer mode.
    ColorPipelineSwapchainTransfer,

    /// `render.hdr_to_sdr.enabled`.
    HdrToSdrEnabled,

    /// `render.hdr_to_sdr.operator`.
    HdrToSdrOperator,

    /// `render.hdr_to_sdr.output_mode`.
    HdrToSdrOutputMode,

    /// `render.hdr_to_sdr.sdr_reference_white_nits`.
    HdrToSdrSdrReferenceWhiteNits,

    /// `render.hdr_to_sdr.hdr_reference_peak_nits`.
    HdrToSdrHdrReferencePeakNits,

    /// Future shader parameter, определённый typed descriptor-ом.
    ShaderParameter(ShaderParameterId),
}

impl RenderLiveSettingId {
    /// Возвращает stable id для reports/status.
    #[must_use]
    pub fn stable_id(&self) -> Cow<'_, str> {
        match self {
            Self::ColorAdjustmentBrightness => Cow::Borrowed("render.color_adjustment.brightness"),
            Self::ColorAdjustmentContrast => Cow::Borrowed("render.color_adjustment.contrast"),
            Self::ColorAdjustmentSaturation => Cow::Borrowed("render.color_adjustment.saturation"),
            Self::ColorAdjustmentExposure => Cow::Borrowed("render.color_adjustment.exposure"),
            Self::ColorAdjustmentRgbGain => Cow::Borrowed("render.color_adjustment.rgb_gain"),
            Self::ColorAdjustmentRgbOffset => Cow::Borrowed("render.color_adjustment.rgb_offset"),
            Self::ColorPipelineToneMapping => Cow::Borrowed("render.color_pipeline.tone_mapping"),
            Self::ColorPipelineSwapchainTransfer => {
                Cow::Borrowed("render.color_pipeline.swapchain_transfer")
            }
            Self::HdrToSdrEnabled => Cow::Borrowed("render.hdr_to_sdr.enabled"),
            Self::HdrToSdrOperator => Cow::Borrowed("render.hdr_to_sdr.operator"),
            Self::HdrToSdrOutputMode => Cow::Borrowed("render.hdr_to_sdr.output_mode"),
            Self::HdrToSdrSdrReferenceWhiteNits => {
                Cow::Borrowed("render.hdr_to_sdr.sdr_reference_white_nits")
            }
            Self::HdrToSdrHdrReferencePeakNits => {
                Cow::Borrowed("render.hdr_to_sdr.hdr_reference_peak_nits")
            }
            Self::ShaderParameter(id) => Cow::Borrowed(id.as_str()),
        }
    }
}

/// Backend-neutral live settings snapshot, который можно применять без decode/session rebuild.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderLiveSettings {
    /// Общие color pipeline settings.
    pub color_pipeline: ColorPipelineSettings,

    /// HDR-to-SDR settings для поддержанного HDR presentation path.
    pub hdr_to_sdr: HdrToSdrSettings,

    /// Future shader parameters с typed values.
    pub shader_parameters: ShaderParameterSet,
}

impl RenderLiveSettings {
    /// Возвращает field-level diff относительно baseline.
    #[must_use]
    pub fn changed_fields_from(&self, baseline: &Self) -> Vec<RenderLiveSettingId> {
        let mut changed_fields = Vec::new();

        push_color_pipeline_changed_fields(
            &mut changed_fields,
            &self.color_pipeline,
            &baseline.color_pipeline,
        );
        push_hdr_to_sdr_changed_fields(&mut changed_fields, &self.hdr_to_sdr, &baseline.hdr_to_sdr);

        changed_fields.extend(
            self.shader_parameters
                .changed_parameter_ids_from(&baseline.shader_parameters)
                .into_iter()
                .map(RenderLiveSettingId::ShaderParameter),
        );

        changed_fields
    }
}

impl Default for RenderLiveSettings {
    /// Default live settings совпадают с текущим renderer default contract.
    fn default() -> Self {
        Self {
            color_pipeline: ColorPipelineSettings::default(),
            hdr_to_sdr: HdrToSdrSettings::default(),
            shader_parameters: ShaderParameterSet::default(),
        }
    }
}

/// Изменение live settings, отправляемое конкретному renderer adapter-у.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderLiveSettingsUpdate {
    /// Новый полный snapshot, который должен стать active runtime state.
    pub settings: RenderLiveSettings,

    /// Field-level ids, изменённые в этом update-е.
    pub changed_fields: Vec<RenderLiveSettingId>,
}

impl RenderLiveSettingsUpdate {
    /// Создаёт update из готового settings snapshot и explicit diff.
    #[must_use]
    pub fn new(settings: RenderLiveSettings, changed_fields: Vec<RenderLiveSettingId>) -> Self {
        Self {
            settings,
            changed_fields,
        }
    }

    /// Создаёт update, вычисляя field-level diff относительно baseline.
    #[must_use]
    pub fn from_baseline(baseline: &RenderLiveSettings, settings: RenderLiveSettings) -> Self {
        let changed_fields = settings.changed_fields_from(baseline);

        Self {
            settings,
            changed_fields,
        }
    }
}

/// Фаза применения live settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveApplyPhase {
    /// Preview update во время draft transaction.
    Preview,

    /// Commit после Apply/OK.
    Commit,

    /// Rollback к baseline после Cancel/window close.
    Rollback,
}

impl RenderLiveApplyPhase {
    /// Возвращает stable id фазы для diagnostics.
    #[must_use]
    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Commit => "commit",
            Self::Rollback => "rollback",
        }
    }
}

/// Успешный outcome применения live settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveApplyOutcome {
    /// Snapshot уже активен; adapter ничего не менял.
    NoOp,

    /// Adapter применил один или несколько field-level changes.
    Applied,
}

/// Успешный report live settings adapter-а.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderLiveApplyReport {
    /// Фаза применения, чтобы status layer не угадывал контекст.
    pub phase: RenderLiveApplyPhase,

    /// Итог успешной операции.
    pub outcome: RenderLiveApplyOutcome,

    /// Field-level ids, реально требовавшие изменения.
    pub changed_fields: Vec<RenderLiveSettingId>,
}

impl RenderLiveApplyReport {
    /// Создаёт no-op report.
    #[must_use]
    pub fn no_op(phase: RenderLiveApplyPhase) -> Self {
        Self {
            phase,
            outcome: RenderLiveApplyOutcome::NoOp,
            changed_fields: Vec::new(),
        }
    }

    /// Создаёт applied report.
    #[must_use]
    pub fn applied(phase: RenderLiveApplyPhase, changed_fields: Vec<RenderLiveSettingId>) -> Self {
        Self {
            phase,
            outcome: RenderLiveApplyOutcome::Applied,
            changed_fields,
        }
    }
}

/// Категория ошибки live settings adapter-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveSettingsErrorKind {
    /// Adapter жив, но конкретные fields/values не поддерживаются.
    Unsupported,

    /// Нужный runtime resource отсутствует прямо сейчас.
    AbsentResource,

    /// Backend сообщил ошибку, после которой normal retry небезопасен.
    Fatal,
}

/// Ошибка применения live settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderLiveSettingsError {
    /// Adapter жив, но конкретные fields/values не поддерживаются.
    Unsupported {
        /// Фаза, в которой возникла ошибка.
        phase: RenderLiveApplyPhase,

        /// Affected field-level ids.
        setting_ids: Vec<RenderLiveSettingId>,

        /// Человекочитаемое объяснение для report/status.
        reason: String,
    },

    /// Runtime resource отсутствует: например, renderer ещё не создан.
    AbsentResource {
        /// Фаза, в которой возникла ошибка.
        phase: RenderLiveApplyPhase,

        /// Человекочитаемое объяснение для report/status.
        reason: String,
    },

    /// Fatal backend error.
    Fatal {
        /// Фаза, в которой возникла ошибка.
        phase: RenderLiveApplyPhase,

        /// Человекочитаемое объяснение для report/status.
        reason: String,
    },
}

impl RenderLiveSettingsError {
    /// Создаёт unsupported error с явными affected fields.
    #[must_use]
    pub fn unsupported(
        phase: RenderLiveApplyPhase,
        setting_ids: Vec<RenderLiveSettingId>,
        reason: impl Into<String>,
    ) -> Self {
        Self::Unsupported {
            phase,
            setting_ids,
            reason: reason.into(),
        }
    }

    /// Создаёт absent-resource error.
    #[must_use]
    pub fn absent_resource(phase: RenderLiveApplyPhase, reason: impl Into<String>) -> Self {
        Self::AbsentResource {
            phase,
            reason: reason.into(),
        }
    }

    /// Создаёт fatal error.
    #[must_use]
    pub fn fatal(phase: RenderLiveApplyPhase, reason: impl Into<String>) -> Self {
        Self::Fatal {
            phase,
            reason: reason.into(),
        }
    }

    /// Возвращает фазу ошибки.
    #[must_use]
    pub const fn phase(&self) -> RenderLiveApplyPhase {
        match self {
            Self::Unsupported { phase, .. }
            | Self::AbsentResource { phase, .. }
            | Self::Fatal { phase, .. } => *phase,
        }
    }

    /// Возвращает категорию ошибки без строкового parsing.
    #[must_use]
    pub const fn kind(&self) -> RenderLiveSettingsErrorKind {
        match self {
            Self::Unsupported { .. } => RenderLiveSettingsErrorKind::Unsupported,
            Self::AbsentResource { .. } => RenderLiveSettingsErrorKind::AbsentResource,
            Self::Fatal { .. } => RenderLiveSettingsErrorKind::Fatal,
        }
    }

    /// Возвращает affected fields для unsupported error-а.
    #[must_use]
    pub fn setting_ids(&self) -> &[RenderLiveSettingId] {
        match self {
            Self::Unsupported { setting_ids, .. } => setting_ids,
            Self::AbsentResource { .. } | Self::Fatal { .. } => &[],
        }
    }
}

impl fmt::Display for RenderLiveSettingsError {
    /// Пишет короткий user-facing текст без backend-specific типов.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported {
                phase,
                setting_ids,
                reason,
            } => write!(
                formatter,
                "render live settings {} unsupported for [{}]: {}",
                phase.stable_id(),
                setting_ids
                    .iter()
                    .map(RenderLiveSettingId::stable_id)
                    .collect::<Vec<_>>()
                    .join(", "),
                reason
            ),
            Self::AbsentResource { phase, reason } => write!(
                formatter,
                "render live settings {} absent resource: {}",
                phase.stable_id(),
                reason
            ),
            Self::Fatal { phase, reason } => write!(
                formatter,
                "render live settings {} fatal error: {}",
                phase.stable_id(),
                reason
            ),
        }
    }
}

impl Error for RenderLiveSettingsError {}

/// Renderer-neutral live settings adapter boundary.
pub trait RenderLiveSettingsAdapter {
    /// Применяет preview update без TOML write и без pipeline/session rebuild.
    fn preview_live_settings(
        &mut self,
        update: &RenderLiveSettingsUpdate,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError>;

    /// Фиксирует уже валидированный settings snapshot как committed runtime state.
    fn commit_live_settings(
        &mut self,
        settings: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError>;

    /// Откатывает renderer к baseline, захваченному preview transaction-ом.
    fn rollback_live_settings(
        &mut self,
        baseline: &RenderLiveSettings,
    ) -> Result<RenderLiveApplyReport, RenderLiveSettingsError>;
}

/// Добавляет field-level diff для color pipeline части live settings.
fn push_color_pipeline_changed_fields(
    changed_fields: &mut Vec<RenderLiveSettingId>,
    settings: &ColorPipelineSettings,
    baseline: &ColorPipelineSettings,
) {
    if settings.adjustment.brightness != baseline.adjustment.brightness {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentBrightness);
    }

    if settings.adjustment.contrast != baseline.adjustment.contrast {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentContrast);
    }

    if settings.adjustment.saturation != baseline.adjustment.saturation {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentSaturation);
    }

    if settings.adjustment.exposure != baseline.adjustment.exposure {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentExposure);
    }

    if settings.adjustment.rgb_gain != baseline.adjustment.rgb_gain {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentRgbGain);
    }

    if settings.adjustment.rgb_offset != baseline.adjustment.rgb_offset {
        changed_fields.push(RenderLiveSettingId::ColorAdjustmentRgbOffset);
    }

    if settings.tone_mapping != baseline.tone_mapping {
        changed_fields.push(RenderLiveSettingId::ColorPipelineToneMapping);
    }

    if settings.swapchain_transfer != baseline.swapchain_transfer {
        changed_fields.push(RenderLiveSettingId::ColorPipelineSwapchainTransfer);
    }
}

/// Добавляет field-level diff для HDR-to-SDR части live settings.
fn push_hdr_to_sdr_changed_fields(
    changed_fields: &mut Vec<RenderLiveSettingId>,
    settings: &HdrToSdrSettings,
    baseline: &HdrToSdrSettings,
) {
    if settings.enabled != baseline.enabled {
        changed_fields.push(RenderLiveSettingId::HdrToSdrEnabled);
    }

    if settings.operator != baseline.operator {
        changed_fields.push(RenderLiveSettingId::HdrToSdrOperator);
    }

    if settings.output_mode != baseline.output_mode {
        changed_fields.push(RenderLiveSettingId::HdrToSdrOutputMode);
    }

    if settings.sdr_reference_white_nits != baseline.sdr_reference_white_nits {
        changed_fields.push(RenderLiveSettingId::HdrToSdrSdrReferenceWhiteNits);
    }

    if settings.hdr_reference_peak_nits != baseline.hdr_reference_peak_nits {
        changed_fields.push(RenderLiveSettingId::HdrToSdrHdrReferencePeakNits);
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
fn pixel_layout_supports_phase10_hdr_to_sdr(input_format: VideoFramePixelLayout) -> bool {
    matches!(
        input_format,
        VideoFramePixelLayout::P010
            | VideoFramePixelLayout::Yuv420Planar10Le
            | VideoFramePixelLayout::Yuv420Planar12Le
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
    pub format: VideoFramePixelLayout,

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

    /// Display orientation, которую renderer применяет при sampling.
    #[serde(default)]
    pub display_orientation: VideoDisplayOrientation,

    /// Typed color metadata кадра.
    pub color: RenderColorMetadata,
}

impl RenderableFrame {
    /// Возвращает `true`, если frame содержит ненулевой display size.
    #[must_use]
    pub const fn has_display_size(&self) -> bool {
        self.render_width > 0 && self.render_height > 0
    }

    /// Возвращает display width после применения quarter-turn orientation.
    #[must_use]
    pub const fn oriented_display_width(&self) -> u32 {
        if self.display_orientation.swaps_axes() {
            self.render_height
        } else {
            self.render_width
        }
    }

    /// Возвращает display height после применения quarter-turn orientation.
    #[must_use]
    pub const fn oriented_display_height(&self) -> u32 {
        if self.display_orientation.swaps_axes() {
            self.render_width
        } else {
            self.render_height
        }
    }
}

/// Техническая причина отказа при проверке одного renderer frame contract-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFrameContractRejection {
    /// Сам contract нарушает invariant neutral vocabulary.
    InvalidContract {
        /// Причина, которую вернул `video-frame-contract`.
        reason: VideoFrameContractValidationError,
    },

    /// Renderer вообще не объявлял такой transfer path.
    UnsupportedTransferPath {
        /// Transfer path/layout, который запросил caller.
        transfer_path: VideoFrameTransferPath,
    },

    /// Renderer не объявлял такой pixel layout ни для одного path-а.
    UnsupportedPixelLayout {
        /// Pixel layout, который запросил caller.
        pixel_layout: VideoFramePixelLayout,
    },

    /// Renderer поддерживает DMA-BUF для pixel layout-а, но не этот image layout.
    UnsupportedDmaBufImageLayout {
        /// Pixel layout, для которого проверялся DMA-BUF layout.
        pixel_layout: VideoFramePixelLayout,

        /// DMA-BUF image layout, который не входит в renderer contract list.
        image_layout: DmaBufImageLayout,
    },

    /// Pixel layout и transfer path по отдельности известны, но не как одна пара.
    UnsupportedContractCombination {
        /// Полный frame contract, который нельзя собирать через Cartesian product.
        frame_contract: VideoFrameContract,
    },
}

/// Размерная ось, которая превысила renderer texture limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTextureDimension {
    /// Coded width stream-а.
    Width,

    /// Coded height stream-а.
    Height,
}

/// Техническая причина отказа stream-level renderer output check-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderVideoOutputRejection {
    /// Frame contract сам по себе не входит в renderer input boundary.
    FrameContract {
        /// Детальная contract-level причина.
        reason: RenderFrameContractRejection,
    },

    /// P010 path объявлен не как production-renderable.
    P010NotRenderable {
        /// Текущий diagnostic readiness.
        readiness: P010RenderReadiness,
    },

    /// Stream требует HDR обработки, но renderer не имеет подходящего output path-а.
    HdrUnsupported {
        /// Frame contract, который проверялся для HDR stream-а.
        frame_contract: VideoFrameContract,
    },

    /// Coded размер stream-а превышает renderer texture limit.
    MaxTextureSizeExceeded {
        /// Какая ось превысила limit.
        dimension: RenderTextureDimension,

        /// Запрошенный размер по этой оси.
        requested: u32,

        /// Максимум, объявленный renderer backend-ом.
        max_texture_size: u32,
    },
}

/// Возможности одного renderer backend-а.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RenderCapabilities {
    /// Семейство backend-а.
    pub backend: RenderBackendKind,

    /// Человекочитаемое имя backend-а.
    pub display_name: String,

    /// Полные frame contracts, которые backend может принять без скрытых fallback-ов.
    pub supported_frame_contracts: Vec<VideoFrameContract>,

    /// Состояние P010: diagnostic boundary отдельно от production renderability.
    #[serde(default)]
    pub p010_render_readiness: P010RenderReadiness,

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

fn wgpu_yuv420_host_upload_frame_contracts() -> [VideoFrameContract; 3] {
    [
        VideoFrameContract::host_yuv420_planar8(),
        VideoFrameContract::host_yuv420_planar10le(),
        VideoFrameContract::host_yuv420_planar12le(),
    ]
}

impl RenderCapabilities {
    /// Создаёт capabilities для текущего WGPU MVP backend-а.
    #[must_use]
    pub fn wgpu_nv12(max_texture_size: Option<u32>) -> Self {
        let mut supported_frame_contracts = vec![VideoFrameContract::dma_buf_nv12(
            DmaBufImageLayout::ComposedLayers,
        )];
        supported_frame_contracts.extend(wgpu_yuv420_host_upload_frame_contracts());

        Self {
            backend: RenderBackendKind::Wgpu,
            display_name: "WGPU NV12 + HostPlanar YUV420 renderer".to_string(),
            supported_frame_contracts,
            p010_render_readiness: P010RenderReadiness::Unavailable,
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
        Self::wgpu_p010_bt2446c_with_dma_buf_image_layouts(
            max_texture_size,
            vec![
                DmaBufImageLayout::SeparateLayers,
                DmaBufImageLayout::ComposedLayers,
            ],
        )
    }

    /// Создаёт capabilities для WGPU P010 renderer-а с явными import layout-ами.
    #[must_use]
    pub fn wgpu_p010_bt2446c_with_dma_buf_image_layouts(
        max_texture_size: Option<u32>,
        supported_p010_dma_buf_image_layouts: Vec<DmaBufImageLayout>,
    ) -> Self {
        let mut supported_frame_contracts = vec![VideoFrameContract::dma_buf_nv12(
            DmaBufImageLayout::ComposedLayers,
        )];
        supported_frame_contracts.extend(
            supported_p010_dma_buf_image_layouts
                .into_iter()
                .map(VideoFrameContract::dma_buf_p010),
        );
        supported_frame_contracts.extend(wgpu_yuv420_host_upload_frame_contracts());

        Self {
            backend: RenderBackendKind::Wgpu,
            display_name: "WGPU P010 BT.2446-C + HostPlanar YUV420 renderer".to_string(),
            supported_frame_contracts,
            p010_render_readiness: P010RenderReadiness::Renderable,
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
    pub fn supports_frame_format(&self, format: VideoFramePixelLayout) -> bool {
        self.supported_frame_contracts
            .iter()
            .any(|contract| contract.pixel_layout == format)
    }

    /// Проверяет поддержку полного frame contract-а без stream-level HDR/size policy.
    #[must_use]
    pub fn supports_frame_contract(&self, frame_contract: VideoFrameContract) -> bool {
        self.check_frame_contract(frame_contract).is_ok()
    }

    /// Возвращает техническую причину, если frame contract не входит в renderer boundary.
    pub fn check_frame_contract(
        &self,
        frame_contract: VideoFrameContract,
    ) -> Result<(), RenderFrameContractRejection> {
        frame_contract
            .validate()
            .map_err(|reason| RenderFrameContractRejection::InvalidContract { reason })?;

        if self.supported_frame_contracts.contains(&frame_contract) {
            return Ok(());
        }

        let transfer_family_supported = self.supported_frame_contracts.iter().any(|contract| {
            same_transfer_path_family(contract.transfer_path, frame_contract.transfer_path)
        });
        if !transfer_family_supported {
            return Err(RenderFrameContractRejection::UnsupportedTransferPath {
                transfer_path: frame_contract.transfer_path,
            });
        }

        if !self.supports_frame_format(frame_contract.pixel_layout) {
            return Err(RenderFrameContractRejection::UnsupportedPixelLayout {
                pixel_layout: frame_contract.pixel_layout,
            });
        }

        if let Some(image_layout) = dma_buf_image_layout(frame_contract.transfer_path) {
            let supports_same_pixel_dma_buf =
                self.supported_frame_contracts.iter().any(|contract| {
                    contract.pixel_layout == frame_contract.pixel_layout
                        && dma_buf_image_layout(contract.transfer_path).is_some()
                });
            if supports_same_pixel_dma_buf {
                return Err(RenderFrameContractRejection::UnsupportedDmaBufImageLayout {
                    pixel_layout: frame_contract.pixel_layout,
                    image_layout,
                });
            }
        }

        Err(RenderFrameContractRejection::UnsupportedContractCombination { frame_contract })
    }

    /// Проверяет, что P010 доступен именно как production-renderable path.
    #[must_use]
    pub fn supports_p010_rendering(&self) -> bool {
        self.p010_render_readiness.is_renderable()
            && self.supported_frame_contracts.iter().any(|contract| {
                contract.pixel_layout == VideoFramePixelLayout::P010
                    && dma_buf_image_layout(contract.transfer_path).is_some()
            })
    }

    /// Проверяет, что renderer умеет импортировать конкретный P010 storage layout.
    #[must_use]
    pub fn supports_p010_storage_layout(&self, layout: DmaBufImageLayout) -> bool {
        self.supports_p010_rendering()
            && self.supports_frame_contract(VideoFrameContract::dma_buf_p010(layout))
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

    /// Проверяет stream-level renderability для уже выбранного frame contract-а.
    #[must_use]
    pub fn supports_video_output(
        &self,
        requirement: &VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) -> bool {
        self.check_video_output(requirement, frame_contract).is_ok()
    }

    /// Возвращает техническую причину stream-level renderer rejection-а.
    pub fn check_video_output(
        &self,
        requirement: &VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) -> Result<(), RenderVideoOutputRejection> {
        if let Err(reason) = self.check_frame_contract(frame_contract) {
            return Err(RenderVideoOutputRejection::FrameContract { reason });
        }

        check_frame_contract_matches_requirement(requirement, frame_contract)?;

        if frame_contract.pixel_layout == VideoFramePixelLayout::P010
            && !self.supports_p010_rendering()
        {
            return Err(RenderVideoOutputRejection::P010NotRenderable {
                readiness: self.p010_render_readiness,
            });
        }

        if requirement.hdr
            && !((self.supports_hdr_to_sdr_with(&HdrToSdrSettings::default())
                && frame_contract_supports_hdr_to_sdr(frame_contract))
                || self.supports_native_hdr_output)
        {
            return Err(RenderVideoOutputRejection::HdrUnsupported { frame_contract });
        }

        if let (Some(width), Some(max_texture_size)) = (requirement.width, self.max_texture_size)
            && width > max_texture_size
        {
            return Err(RenderVideoOutputRejection::MaxTextureSizeExceeded {
                dimension: RenderTextureDimension::Width,
                requested: width,
                max_texture_size,
            });
        }

        if let (Some(height), Some(max_texture_size)) = (requirement.height, self.max_texture_size)
            && height > max_texture_size
        {
            return Err(RenderVideoOutputRejection::MaxTextureSizeExceeded {
                dimension: RenderTextureDimension::Height,
                requested: height,
                max_texture_size,
            });
        }

        Ok(())
    }

    /// Формирует одну строку diagnostics для capability report.
    #[must_use]
    pub fn summary_text(&self) -> String {
        let frame_contracts = self
            .supported_frame_contracts
            .iter()
            .map(|contract| contract.diagnostic_label())
            .collect::<Vec<_>>()
            .join(", ");

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
            "{}: {}, {}, {}, frame contracts: {}, max texture: {}",
            self.display_name,
            hdr_support_label,
            native_hdr_label,
            self.p010_render_readiness,
            frame_contracts,
            self.max_texture_size
                .map(|size| size.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        )
    }
}

/// Сравнивает family transfer path-а без смешивания concrete layout-ов.
fn same_transfer_path_family(left: VideoFrameTransferPath, right: VideoFrameTransferPath) -> bool {
    match (left, right) {
        (
            VideoFrameTransferPath::SoftwareHostUpload,
            VideoFrameTransferPath::SoftwareHostUpload,
        ) => true,
        (
            VideoFrameTransferPath::HardwareZeroCopy { handle: left },
            VideoFrameTransferPath::HardwareZeroCopy { handle: right },
        ) => same_hardware_handle_family(left, right),
        _ => false,
    }
}

/// Сравнивает hardware handle family, не считая layout details равными.
const fn same_hardware_handle_family(
    left: HardwareFrameHandle,
    right: HardwareFrameHandle,
) -> bool {
    matches!(
        (left, right),
        (
            HardwareFrameHandle::DmaBuf { .. },
            HardwareFrameHandle::DmaBuf { .. }
        )
    )
}

/// Достаёт DMA-BUF layout только из DMA-BUF transfer path-а.
const fn dma_buf_image_layout(transfer_path: VideoFrameTransferPath) -> Option<DmaBufImageLayout> {
    match transfer_path {
        VideoFrameTransferPath::HardwareZeroCopy {
            handle: HardwareFrameHandle::DmaBuf { image_layout },
        } => Some(image_layout),
        VideoFrameTransferPath::SoftwareHostUpload => None,
    }
}

/// Проверяет, что выбранный renderer contract совпадает с codec-level metadata stream-а.
fn check_frame_contract_matches_requirement(
    requirement: &VideoDecodeRequirement,
    frame_contract: VideoFrameContract,
) -> Result<(), RenderVideoOutputRejection> {
    let expected_bit_depth = required_frame_bit_depth(requirement);
    let expected_chroma = required_frame_chroma(requirement);

    if frame_contract.pixel_layout.bit_depth() != Some(expected_bit_depth)
        || frame_contract.pixel_layout.chroma() != Some(expected_chroma)
    {
        return Err(RenderVideoOutputRejection::FrameContract {
            reason: RenderFrameContractRejection::UnsupportedContractCombination { frame_contract },
        });
    }

    Ok(())
}

/// Проверяет, что конкретный frame contract имеет GPU HDR-to-SDR shader path.
fn frame_contract_supports_hdr_to_sdr(frame_contract: VideoFrameContract) -> bool {
    pixel_layout_supports_phase10_hdr_to_sdr(frame_contract.pixel_layout)
}

/// Переводит stream bit depth в neutral frame-contract vocabulary.
fn required_frame_bit_depth(requirement: &VideoDecodeRequirement) -> FrameBitDepth {
    match requirement.bit_depth.unwrap_or(BitDepth::Eight) {
        BitDepth::Eight => FrameBitDepth::Eight,
        BitDepth::Ten => FrameBitDepth::Ten,
        BitDepth::Twelve => FrameBitDepth::Twelve,
    }
}

/// Переводит stream chroma в neutral frame-contract vocabulary.
fn required_frame_chroma(requirement: &VideoDecodeRequirement) -> FrameChromaSubsampling {
    match requirement.chroma.unwrap_or(ChromaSubsampling::Yuv420) {
        ChromaSubsampling::Yuv420 => FrameChromaSubsampling::Yuv420,
        ChromaSubsampling::Yuv422 => FrameChromaSubsampling::Yuv422,
        ChromaSubsampling::Yuv444 => FrameChromaSubsampling::Yuv444,
    }
}

#[cfg(test)]
mod tests {
    use codec_core::{
        BitDepth, ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients,
        TransferFunction, VideoCodec, VideoColorMetadata, VideoDecodeRequirement,
        video_frame_pixel_layout_from_decode_requirement,
    };

    use super::*;

    impl RenderCapabilities {
        /// Создаёт fake renderer из exact contracts без WGPU/materializer promises.
        fn fake_with_frame_contracts_for_tests(
            display_name: &str,
            supported_frame_contracts: Vec<VideoFrameContract>,
            max_texture_size: Option<u32>,
        ) -> Self {
            Self {
                backend: RenderBackendKind::Wgpu,
                display_name: display_name.to_string(),
                supported_frame_contracts,
                p010_render_readiness: P010RenderReadiness::Unavailable,
                supported_hdr_to_sdr_operators: Vec::new(),
                hdr_output_mode: HdrOutputMode::SdrBt709Only,
                supports_hdr_to_sdr: false,
                supports_native_hdr_output: false,
                max_texture_size,
                advanced_ui: false,
                ui_composition_mode: UiCompositionMode::Overlay,
                present_timing_metrics: false,
            }
        }

        /// Создаёт fake renderer, который объявляет только exact host-upload contracts.
        fn fake_host_upload_for_tests(
            supported_pixel_layouts: &[VideoFramePixelLayout],
            max_texture_size: Option<u32>,
        ) -> Self {
            let supported_frame_contracts = supported_pixel_layouts
                .iter()
                .copied()
                .map(host_upload_contract_for_tests)
                .collect();

            Self::fake_with_frame_contracts_for_tests(
                "Fake host-upload renderer",
                supported_frame_contracts,
                max_texture_size,
            )
        }
    }

    /// Создаёт host-upload contract для explicit planar layout-а в тестах.
    fn host_upload_contract_for_tests(pixel_layout: VideoFramePixelLayout) -> VideoFrameContract {
        VideoFrameContract {
            pixel_layout,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        }
    }

    /// Собирает stream requirement с теми metadata, которые должен покрыть contract.
    fn video_requirement_for_tests(
        bit_depth: BitDepth,
        chroma: ChromaSubsampling,
    ) -> VideoDecodeRequirement {
        VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(bit_depth)
            .with_chroma(chroma)
    }

    #[test]
    fn eight_bit_yuv420_requirement_maps_to_nv12() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420);

        assert_eq!(
            video_frame_pixel_layout_from_decode_requirement(&requirement),
            Some(VideoFramePixelLayout::Nv12)
        );
    }

    #[test]
    fn ten_bit_requirement_maps_to_p010() {
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

        assert_eq!(
            video_frame_pixel_layout_from_decode_requirement(&requirement),
            Some(VideoFramePixelLayout::P010)
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
    fn render_live_settings_default_keeps_renderer_defaults() {
        let settings = RenderLiveSettings::default();

        assert_eq!(settings.color_pipeline, ColorPipelineSettings::default());
        assert_eq!(settings.hdr_to_sdr, HdrToSdrSettings::default());
        assert!(settings.shader_parameters.is_empty());
        assert!(
            settings
                .changed_fields_from(&RenderLiveSettings::default())
                .is_empty()
        );
    }

    #[test]
    fn render_live_settings_update_tracks_changed_fields() {
        let baseline = RenderLiveSettings::default();
        let shader_parameter_id = ShaderParameterId::new("render.shader.test_gain");
        let mut settings = baseline.clone();

        settings.color_pipeline.adjustment.brightness = 0.25;
        settings.color_pipeline.adjustment.rgb_gain = [1.0, 0.9, 0.8];
        settings.hdr_to_sdr.sdr_reference_white_nits = 120.0;
        settings
            .shader_parameters
            .parameters
            .push(ShaderParameter::new(
                shader_parameter_id.clone(),
                ShaderParameterValue::Float(0.5),
            ));

        let update = RenderLiveSettingsUpdate::from_baseline(&baseline, settings);

        assert_eq!(
            update.changed_fields,
            vec![
                RenderLiveSettingId::ColorAdjustmentBrightness,
                RenderLiveSettingId::ColorAdjustmentRgbGain,
                RenderLiveSettingId::HdrToSdrSdrReferenceWhiteNits,
                RenderLiveSettingId::ShaderParameter(shader_parameter_id),
            ]
        );
    }

    #[test]
    fn render_viewport_full_surface_covers_target() {
        let viewport = RenderViewport::full_surface(1920, 1080);

        assert_eq!(viewport, RenderViewport::new(0, 0, 1920, 1080));
        assert_eq!(viewport.size(), (1920, 1080));
        assert!(!viewport.is_empty());
    }

    #[test]
    fn render_viewport_clamps_partial_overflow_to_surface() {
        let viewport = RenderViewport::new(100, 50, 1000, 800).clamp_to_surface(640, 360);

        assert_eq!(viewport, RenderViewport::new(100, 50, 540, 310));
    }

    #[test]
    fn render_viewport_invalid_request_defaults_to_full_surface() {
        let full_surface = RenderViewport::full_surface(640, 360);

        assert_eq!(
            RenderViewport::new(10, 10, 0, 200).clamp_to_surface(640, 360),
            full_surface
        );
        assert_eq!(
            RenderViewport::new(640, 10, 100, 100).clamp_to_surface(640, 360),
            full_surface
        );
    }

    #[test]
    fn render_viewport_subtracts_left_sidebar_without_changing_content_viewport() {
        let viewport = RenderViewport::full_surface(1280, 720);
        let sidebar = RenderViewport::new(0, 64, 420, 576);

        let visible_rects = viewport.subtract(sidebar);

        assert_eq!(
            visible_rects,
            vec![
                RenderViewport::new(0, 0, 1280, 64),
                RenderViewport::new(0, 640, 1280, 80),
                RenderViewport::new(420, 64, 860, 576),
            ]
        );
        assert_eq!(viewport.size(), (1280, 720));
    }

    #[test]
    fn render_viewport_subtract_keeps_original_when_exclusion_is_outside() {
        let viewport = RenderViewport::full_surface(1280, 720);
        let outside = RenderViewport::new(1400, 0, 100, 100);

        assert_eq!(viewport.subtract(outside), vec![viewport]);
    }

    #[test]
    fn render_viewport_subtract_returns_no_rects_when_fully_excluded() {
        let viewport = RenderViewport::full_surface(1280, 720);

        assert!(viewport.subtract(viewport).is_empty());
    }

    #[test]
    fn shader_parameter_descriptor_keeps_typed_value_contract() {
        let descriptor = ShaderParameterDescriptor::new(
            ShaderParameterId::new("render.shader.preview_strength"),
            ShaderParameterValueType::Float,
            Some(ShaderNumericRange {
                min: 0.0,
                max: 1.0,
                step: Some(0.01),
            }),
            ShaderParameterValue::Float(0.5),
        );

        assert!(descriptor.default_value_is_valid());
        assert!(descriptor.accepts_value(&ShaderParameterValue::Float(1.0)));
        assert!(!descriptor.accepts_value(&ShaderParameterValue::Float(1.5)));
        assert!(!descriptor.accepts_value(&ShaderParameterValue::Float3([0.5, 0.5, 0.5])));
    }

    #[test]
    fn live_settings_errors_keep_noop_unsupported_absent_and_fatal_distinct() {
        let no_op_report = RenderLiveApplyReport::no_op(RenderLiveApplyPhase::Preview);
        let unsupported_error = RenderLiveSettingsError::unsupported(
            RenderLiveApplyPhase::Preview,
            vec![RenderLiveSettingId::ShaderParameter(
                ShaderParameterId::new("render.shader.unknown"),
            )],
            "shader parameter is not supported by this backend",
        );
        let absent_resource_error = RenderLiveSettingsError::absent_resource(
            RenderLiveApplyPhase::Rollback,
            "renderer is not initialized",
        );
        let fatal_error =
            RenderLiveSettingsError::fatal(RenderLiveApplyPhase::Commit, "device lost");

        assert_eq!(no_op_report.outcome, RenderLiveApplyOutcome::NoOp);
        assert_eq!(
            unsupported_error.kind(),
            RenderLiveSettingsErrorKind::Unsupported
        );
        assert_eq!(
            absent_resource_error.kind(),
            RenderLiveSettingsErrorKind::AbsentResource
        );
        assert_eq!(fatal_error.kind(), RenderLiveSettingsErrorKind::Fatal);
        assert_ne!(unsupported_error.kind(), fatal_error.kind());
        assert_eq!(
            unsupported_error.setting_ids(),
            &[RenderLiveSettingId::ShaderParameter(
                ShaderParameterId::new("render.shader.unknown",)
            )]
        );
    }

    #[test]
    fn neutral_render_settings_crates_do_not_depend_on_wgpu_specific_crates() {
        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("render-core crate has crates parent");
        let neutral_manifests = ["render-core/Cargo.toml", "settings-core/Cargo.toml"];

        for manifest in neutral_manifests {
            let manifest_path = crates_dir.join(manifest);
            let manifest_text =
                std::fs::read_to_string(&manifest_path).expect("neutral manifest is readable");

            for dependency_name in ["wgpu", "wgpu-types", "egui", "egui-wgpu", "render-wgpu"] {
                let has_disallowed_dependency = manifest_text.lines().any(|line| {
                    let trimmed_line = line.trim_start();

                    trimmed_line.starts_with(&format!("{dependency_name}."))
                        || trimmed_line.starts_with(&format!("{dependency_name} "))
                        || trimmed_line.starts_with(&format!("{dependency_name}="))
                });

                assert!(
                    !has_disallowed_dependency,
                    "{manifest} must stay renderer/UI neutral and not depend on {dependency_name}"
                );
            }
        }
    }

    #[test]
    fn active_color_path_describes_current_nv12_bt709_limited_sdr_path() {
        let frame = RenderableFrame {
            handle: 7,
            pts: Duration::ZERO,
            format: VideoFramePixelLayout::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            coded_width: 1920,
            coded_height: 1080,
            render_width: 1920,
            render_height: 1080,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
        };
        let settings = ColorPipelineSettings::default();

        let active_path = ActiveColorPath::from_frame(&frame, &settings);

        assert_eq!(active_path.input_format, VideoFramePixelLayout::Nv12);
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
            VideoFramePixelLayout::Nv12,
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
            VideoFramePixelLayout::P010,
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
    fn active_color_path_describes_host_yuv420_hdr_to_sdr_bt2446c_path() {
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
            VideoFramePixelLayout::Yuv420Planar10Le,
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
            "YUV420 planar 10-bit little-endian 10-bit BT.2020 PQ limited -> SDR BT.709 bt2446-c explicit-shader-oetf"
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
            VideoFramePixelLayout::P010,
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
            VideoFramePixelLayout::P010,
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
            VideoFramePixelLayout::Nv12,
            BitDepth::Eight,
            ChromaSubsampling::Yuv420,
            VideoColorMetadata::sdr_bt709_limited(),
            &settings,
        );
        let diagnostics = RenderDiagnostics {
            active_color_path: Some(active_path),
            ..RenderDiagnostics::default()
        };

        assert_eq!(
            diagnostics.active_color_path_text().as_deref(),
            Some("NV12 8-bit BT.709 limited -> SDR BT.709 preserve-current-unorm")
        );
    }

    #[test]
    fn current_wgpu_nv12_capabilities_advertise_host_yuv420_without_p010_or_hdr() {
        let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));

        assert!(capabilities.supports_frame_format(VideoFramePixelLayout::Nv12));
        assert!(
            capabilities.supports_frame_contract(VideoFrameContract::dma_buf_nv12(
                DmaBufImageLayout::ComposedLayers
            ))
        );
        assert!(capabilities.supports_frame_contract(VideoFrameContract::host_yuv420_planar8()));
        assert!(capabilities.supports_frame_contract(VideoFrameContract::host_yuv420_planar10le()));
        assert!(capabilities.supports_frame_contract(VideoFrameContract::host_yuv420_planar12le()));
        assert!(!capabilities.supports_frame_format(VideoFramePixelLayout::P010));
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
        assert!(
            capabilities
                .summary_text()
                .contains("NV12 via hardware zero-copy via DMA-BUF (composed DMA-BUF layers)")
        );
        assert!(capabilities.summary_text().contains("software host upload"));
        assert!(!capabilities.summary_text().contains("HDR supported"));
    }

    #[test]
    fn current_wgpu_capabilities_advertise_dma_buf_and_exact_yuv420_host_upload() {
        let nv12_capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
        let p010_capabilities = RenderCapabilities::wgpu_p010_bt2446c(Some(4096));

        assert_eq!(
            nv12_capabilities.supported_frame_contracts,
            vec![
                VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
                VideoFrameContract::host_yuv420_planar8(),
                VideoFrameContract::host_yuv420_planar10le(),
                VideoFrameContract::host_yuv420_planar12le(),
            ]
        );
        assert_eq!(
            p010_capabilities.supported_frame_contracts,
            vec![
                VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
                VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
                VideoFrameContract::dma_buf_p010(DmaBufImageLayout::ComposedLayers),
                VideoFrameContract::host_yuv420_planar8(),
                VideoFrameContract::host_yuv420_planar10le(),
                VideoFrameContract::host_yuv420_planar12le(),
            ]
        );

        for capabilities in [&nv12_capabilities, &p010_capabilities] {
            assert!(
                capabilities
                    .supported_frame_contracts
                    .iter()
                    .all(|contract| {
                        matches!(
                            contract.pixel_layout,
                            VideoFramePixelLayout::Nv12
                                | VideoFramePixelLayout::P010
                                | VideoFramePixelLayout::Yuv420Planar8
                                | VideoFramePixelLayout::Yuv420Planar10Le
                                | VideoFramePixelLayout::Yuv420Planar12Le
                        )
                    })
            );
            assert!(
                capabilities
                    .supported_frame_contracts
                    .iter()
                    .filter(|contract| contract.transfer_path.is_software_host_upload())
                    .all(|contract| contract.pixel_layout.chroma()
                        == Some(FrameChromaSubsampling::Yuv420))
            );
            assert!(!capabilities.supports_frame_contract(VideoFrameContract {
                pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
                transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
            }));
            assert!(!capabilities.supports_frame_contract(VideoFrameContract {
                pixel_layout: VideoFramePixelLayout::Yuv444Planar8,
                transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
            }));
        }
    }

    #[test]
    fn fake_capabilities_can_advertise_host_upload_without_cartesian_product() {
        let capabilities = RenderCapabilities::fake_with_frame_contracts_for_tests(
            "Fake mixed renderer",
            vec![
                VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
                host_upload_contract_for_tests(VideoFramePixelLayout::Yuv422Planar10Le),
            ],
            Some(4096),
        );

        let unsupported_host_layout =
            host_upload_contract_for_tests(VideoFramePixelLayout::Yuv420Planar10Le);
        let unsupported_dma_buf_layout =
            VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers);

        let invalid_cartesian_contract = VideoFrameContract {
            pixel_layout: VideoFramePixelLayout::Nv12,
            transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
        };

        assert!(
            capabilities.supports_frame_contract(host_upload_contract_for_tests(
                VideoFramePixelLayout::Yuv422Planar10Le
            ))
        );
        assert!(matches!(
            capabilities.check_frame_contract(unsupported_host_layout),
            Err(RenderFrameContractRejection::UnsupportedPixelLayout {
                pixel_layout: VideoFramePixelLayout::Yuv420Planar10Le,
            })
        ));
        assert!(matches!(
            capabilities.check_frame_contract(unsupported_dma_buf_layout),
            Err(RenderFrameContractRejection::UnsupportedDmaBufImageLayout {
                pixel_layout: VideoFramePixelLayout::Nv12,
                image_layout: DmaBufImageLayout::ComposedLayers,
            })
        ));
        assert!(matches!(
            capabilities.check_frame_contract(invalid_cartesian_contract),
            Err(RenderFrameContractRejection::InvalidContract { .. })
        ));
        assert!(capabilities.summary_text().contains("software host upload"));
    }

    #[test]
    fn fake_capabilities_support_host_upload_exact_video_outputs() {
        let supported_layouts = [
            (
                VideoFramePixelLayout::Yuv420Planar8,
                BitDepth::Eight,
                ChromaSubsampling::Yuv420,
            ),
            (
                VideoFramePixelLayout::Yuv420Planar10Le,
                BitDepth::Ten,
                ChromaSubsampling::Yuv420,
            ),
            (
                VideoFramePixelLayout::Yuv420Planar12Le,
                BitDepth::Twelve,
                ChromaSubsampling::Yuv420,
            ),
            (
                VideoFramePixelLayout::Yuv422Planar8,
                BitDepth::Eight,
                ChromaSubsampling::Yuv422,
            ),
            (
                VideoFramePixelLayout::Yuv422Planar10Le,
                BitDepth::Ten,
                ChromaSubsampling::Yuv422,
            ),
            (
                VideoFramePixelLayout::Yuv422Planar12Le,
                BitDepth::Twelve,
                ChromaSubsampling::Yuv422,
            ),
            (
                VideoFramePixelLayout::Yuv444Planar8,
                BitDepth::Eight,
                ChromaSubsampling::Yuv444,
            ),
            (
                VideoFramePixelLayout::Yuv444Planar10Le,
                BitDepth::Ten,
                ChromaSubsampling::Yuv444,
            ),
        ];
        let supported_pixel_layouts = supported_layouts
            .iter()
            .map(|(pixel_layout, _, _)| *pixel_layout)
            .collect::<Vec<_>>();
        let capabilities =
            RenderCapabilities::fake_host_upload_for_tests(&supported_pixel_layouts, Some(4096));

        for (pixel_layout, bit_depth, chroma) in supported_layouts {
            let requirement = video_requirement_for_tests(bit_depth, chroma);
            let frame_contract = host_upload_contract_for_tests(pixel_layout);

            assert!(capabilities.supports_video_output(&requirement, frame_contract));
        }

        let wrong_chroma_requirement =
            video_requirement_for_tests(BitDepth::Ten, ChromaSubsampling::Yuv420);
        let yuv422_contract =
            host_upload_contract_for_tests(VideoFramePixelLayout::Yuv422Planar10Le);

        assert!(matches!(
            capabilities.check_video_output(&wrong_chroma_requirement, yuv422_contract),
            Err(RenderVideoOutputRejection::FrameContract {
                reason: RenderFrameContractRejection::UnsupportedContractCombination {
                    frame_contract
                },
            }) if frame_contract == yuv422_contract
        ));
    }

    #[test]
    fn video_output_rejections_keep_contract_policy_and_size_distinct() {
        let yuv420_requirement =
            video_requirement_for_tests(BitDepth::Eight, ChromaSubsampling::Yuv420);
        let yuv422_requirement =
            video_requirement_for_tests(BitDepth::Eight, ChromaSubsampling::Yuv422);
        let yuv420_host_contract =
            host_upload_contract_for_tests(VideoFramePixelLayout::Yuv420Planar8);
        let yuv422_host_contract =
            host_upload_contract_for_tests(VideoFramePixelLayout::Yuv422Planar8);

        let host_upload_yuv420_capabilities = RenderCapabilities::fake_host_upload_for_tests(
            &[VideoFramePixelLayout::Yuv420Planar8],
            Some(4096),
        );
        assert!(matches!(
            host_upload_yuv420_capabilities
                .check_video_output(&yuv422_requirement, yuv422_host_contract),
            Err(RenderVideoOutputRejection::FrameContract {
                reason: RenderFrameContractRejection::UnsupportedPixelLayout {
                    pixel_layout: VideoFramePixelLayout::Yuv422Planar8,
                },
            })
        ));

        let dma_buf_only_capabilities = RenderCapabilities::fake_with_frame_contracts_for_tests(
            "Fake DMA-BUF-only renderer",
            vec![VideoFrameContract::dma_buf_nv12(
                DmaBufImageLayout::ComposedLayers,
            )],
            Some(4096),
        );
        assert!(matches!(
            dma_buf_only_capabilities.check_video_output(&yuv420_requirement, yuv420_host_contract),
            Err(RenderVideoOutputRejection::FrameContract {
                reason: RenderFrameContractRejection::UnsupportedTransferPath {
                    transfer_path: VideoFrameTransferPath::SoftwareHostUpload,
                },
            })
        ));

        let mut p010_boundary_only_capabilities =
            RenderCapabilities::fake_with_frame_contracts_for_tests(
                "Fake P010 boundary-only renderer",
                vec![VideoFrameContract::dma_buf_p010(
                    DmaBufImageLayout::SeparateLayers,
                )],
                Some(4096),
            );
        p010_boundary_only_capabilities.p010_render_readiness =
            P010RenderReadiness::ZeroCopyBoundaryVerified;
        let p010_requirement =
            video_requirement_for_tests(BitDepth::Ten, ChromaSubsampling::Yuv420);
        assert!(matches!(
            p010_boundary_only_capabilities.check_video_output(
                &p010_requirement,
                VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
            ),
            Err(RenderVideoOutputRejection::P010NotRenderable {
                readiness: P010RenderReadiness::ZeroCopyBoundaryVerified,
            })
        ));

        let mut hdr_requirement = yuv420_requirement.clone();
        hdr_requirement.hdr = true;
        assert!(matches!(
            host_upload_yuv420_capabilities
                .check_video_output(&hdr_requirement, yuv420_host_contract),
            Err(RenderVideoOutputRejection::HdrUnsupported {
                frame_contract
            }) if frame_contract == yuv420_host_contract
        ));

        let small_texture_capabilities = RenderCapabilities::fake_host_upload_for_tests(
            &[VideoFramePixelLayout::Yuv420Planar8],
            Some(32),
        );
        let oversized_requirement = yuv420_requirement.with_resolution(64, 16);
        assert!(matches!(
            small_texture_capabilities
                .check_video_output(&oversized_requirement, yuv420_host_contract),
            Err(RenderVideoOutputRejection::MaxTextureSizeExceeded {
                dimension: RenderTextureDimension::Width,
                requested: 64,
                max_texture_size: 32,
            })
        ));
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
            .supported_frame_contracts
            .push(VideoFrameContract::dma_buf_p010(
                DmaBufImageLayout::SeparateLayers,
            ));
        p010_without_operator.p010_render_readiness = P010RenderReadiness::Renderable;
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
        capabilities
            .supported_frame_contracts
            .push(VideoFrameContract::dma_buf_p010(
                DmaBufImageLayout::SeparateLayers,
            ));
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

        assert!(!capabilities.supports_p010_rendering());
        assert!(!capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
        assert!(matches!(
            capabilities.check_video_output(
                &requirement,
                VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
            ),
            Err(RenderVideoOutputRejection::P010NotRenderable {
                readiness: P010RenderReadiness::ZeroCopyBoundaryVerified,
            })
        ));
        assert!(
            capabilities
                .summary_text()
                .contains("P010 zero-copy boundary verified")
        );
    }

    #[test]
    fn current_wgpu_nv12_capabilities_reject_p010_as_unsupported_pixel_layout() {
        let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);

        assert!(matches!(
            capabilities.check_video_output(
                &requirement,
                VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
            ),
            Err(RenderVideoOutputRejection::FrameContract {
                reason: RenderFrameContractRejection::UnsupportedPixelLayout {
                    pixel_layout: VideoFramePixelLayout::P010,
                },
            })
        ));
    }

    #[test]
    fn current_wgpu_nv12_capabilities_reject_p010_before_hdr_policy() {
        let capabilities = RenderCapabilities::wgpu_nv12(Some(4096));
        let mut requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_bit_depth(BitDepth::Ten);
        requirement.hdr = true;

        assert!(matches!(
            capabilities.check_video_output(
                &requirement,
                VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
            ),
            Err(RenderVideoOutputRejection::FrameContract {
                reason: RenderFrameContractRejection::UnsupportedPixelLayout {
                    pixel_layout: VideoFramePixelLayout::P010,
                },
            })
        ));
    }

    #[test]
    fn p010_bt2446c_capabilities_accept_ten_bit_hdr_but_not_native_hdr_output() {
        let capabilities = RenderCapabilities::wgpu_p010_bt2446c(Some(4096));
        let mut requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420);
        requirement.hdr = true;

        assert!(capabilities.supports_video_output(
            &requirement,
            VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers),
        ));
        assert!(capabilities.supports_hdr_to_sdr_with(&HdrToSdrSettings::default()));
        assert!(!capabilities.supports_native_hdr_output);
        assert!(capabilities.summary_text().contains("HDR available"));
        assert!(
            capabilities
                .summary_text()
                .contains("native HDR unsupported")
        );
    }

    #[test]
    fn host_yuv420_hdr_policy_requires_high_bit_gpu_shader_contract() {
        let capabilities = RenderCapabilities::wgpu_p010_bt2446c(Some(4096));
        let mut eight_bit_hdr_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420);
        eight_bit_hdr_requirement.hdr = true;
        let mut ten_bit_hdr_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420);
        ten_bit_hdr_requirement.hdr = true;
        let mut twelve_bit_hdr_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(BitDepth::Twelve)
            .with_chroma(ChromaSubsampling::Yuv420);
        twelve_bit_hdr_requirement.hdr = true;

        assert!(matches!(
            capabilities.check_video_output(
                &eight_bit_hdr_requirement,
                VideoFrameContract::host_yuv420_planar8(),
            ),
            Err(RenderVideoOutputRejection::HdrUnsupported { .. })
        ));
        assert!(capabilities.supports_video_output(
            &ten_bit_hdr_requirement,
            VideoFrameContract::host_yuv420_planar10le(),
        ));
        assert!(capabilities.supports_video_output(
            &twelve_bit_hdr_requirement,
            VideoFrameContract::host_yuv420_planar12le(),
        ));
    }
}
