use serde::{Deserialize, Deserializer, Serialize};

/// Render-настройки верхнего уровня.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct RenderConfig {
    /// Активный render profile.
    #[setting(
        id = "render.profile",
        path = "render.profile",
        section = "render",
        group = "profile",
        surface = "main-settings-window",
        label_id = "settings.render.profile.label",
        label_ru = "Render profile",
        description_id = "settings.render.profile.description",
        description_ru = "Активный render profile для backend selection.",
        editor = "select",
        apply = "render.apply",
        options(
            option(id = "auto", label_id = "settings.render.profile.auto", label_ru = "Авто", value = RenderProfile::Auto),
            option(id = "vulkan", label_id = "settings.render.profile.vulkan", label_ru = "Vulkan", value = RenderProfile::Vulkan),
            option(id = "opengles", label_id = "settings.render.profile.opengles", label_ru = "OpenGL ES", value = RenderProfile::OpenGles),
        )
    )]
    pub profile: RenderProfile,

    /// Typed HDR-to-SDR baseline config для Phase 10.
    #[serde(default, deserialize_with = "deserialize_hdr_to_sdr_config")]
    #[setting(nested)]
    pub hdr_to_sdr: HdrToSdrConfig,

    /// Compatibility placeholder для будущего HDR tone mapping; Phase 8.5 держит `Disabled`.
    #[setting(
        id = "render.tone_mapping",
        path = "render.tone_mapping",
        section = "render",
        group = "profile",
        surface = "main-settings-window",
        label_id = "settings.render.tone_mapping.label",
        label_ru = "Tone mapping",
        description_id = "settings.render.tone_mapping.description",
        description_ru = "Compatibility placeholder; текущий production path держит Disabled.",
        editor = "select",
        apply = "render.apply",
        options(
            option(id = "auto", label_id = "settings.render.tone_mapping.auto", label_ru = "Авто", value = ToneMappingMode::Auto),
            option(id = "disabled", label_id = "settings.render.tone_mapping.disabled", label_ru = "Отключён", value = ToneMappingMode::Disabled),
        )
    )]
    pub tone_mapping: ToneMappingMode,

    /// Пользовательские SDR/RGB корректировки без HDR controls.
    #[setting(nested)]
    pub color_adjustment: RenderColorAdjustmentConfig,

    /// Vulkan-specific параметры.
    #[setting(nested)]
    pub vulkan: VulkanConfig,

    /// OpenGL ES fallback-параметры.
    #[setting(nested)]
    pub opengles: OpenGlesConfig,
}

impl Default for RenderConfig {
    /// Возвращает Vulkan-first defaults текущего MVP.
    fn default() -> Self {
        Self {
            profile: RenderProfile::Vulkan,
            hdr_to_sdr: HdrToSdrConfig::default(),
            tone_mapping: ToneMappingMode::Disabled,
            color_adjustment: RenderColorAdjustmentConfig::default(),
            vulkan: VulkanConfig::default(),
            opengles: OpenGlesConfig::default(),
        }
    }
}

/// Пользовательская секция `[render.hdr_to_sdr]`.
///
/// Схема намеренно не содержит alternative tone mapping presets и native HDR
/// output mode: Phase 10 поддерживает только BT.2446-C в SDR BT.709 output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct HdrToSdrConfig {
    /// Разрешает HDR-to-SDR path, если renderer capabilities тоже подтверждают support.
    #[setting(
        id = "render.hdr_to_sdr.enabled",
        path = "render.hdr_to_sdr.enabled",
        section = "render",
        group = "hdr_to_sdr",
        surface = "main-settings-window",
        label_id = "settings.render.hdr_to_sdr.enabled.label",
        label_ru = "HDR-to-SDR",
        description_id = "settings.render.hdr_to_sdr.enabled.description",
        description_ru = "Включает HDR-to-SDR path при поддержке renderer capabilities.",
        editor = "toggle",
        apply = "render.preview"
    )]
    pub enabled: bool,

    /// Единственный production operator Phase 10.
    #[setting(
        id = "render.hdr_to_sdr.operator",
        path = "render.hdr_to_sdr.operator",
        section = "render",
        group = "hdr_to_sdr",
        surface = "main-settings-window",
        label_id = "settings.render.hdr_to_sdr.operator.label",
        label_ru = "HDR operator",
        description_id = "settings.render.hdr_to_sdr.operator.description",
        description_ru = "Tone mapping operator для HDR-to-SDR conversion.",
        editor = "select",
        apply = "render.preview",
        options(
            option(id = "bt2446_c", label_id = "settings.render.hdr_to_sdr.operator.bt2446_c", label_ru = "BT.2446-C", value = HdrToSdrOperatorConfig::Bt2446C),
        )
    )]
    pub operator: HdrToSdrOperatorConfig,

    /// SDR reference white в nits для BT.2446-C.
    #[setting(
        id = "render.hdr_to_sdr.sdr_reference_white_nits",
        path = "render.hdr_to_sdr.sdr_reference_white_nits",
        section = "render",
        group = "hdr_to_sdr",
        surface = "main-settings-window",
        label_id = "settings.render.hdr_to_sdr.sdr_reference_white_nits.label",
        label_ru = "SDR reference white",
        description_id = "settings.render.hdr_to_sdr.sdr_reference_white_nits.description",
        description_ru = "SDR reference white в nits для BT.2446-C.",
        editor = "float",
        min = crate::validation::MIN_HDR_TO_SDR_REFERENCE_NITS,
        max = crate::validation::MAX_HDR_TO_SDR_REFERENCE_NITS,
        step = 1.0,
        unit = "nits",
        apply = "render.preview"
    )]
    pub sdr_reference_white_nits: f32,

    /// HDR reference peak в nits для BT.2446-C.
    #[setting(
        id = "render.hdr_to_sdr.hdr_reference_peak_nits",
        path = "render.hdr_to_sdr.hdr_reference_peak_nits",
        section = "render",
        group = "hdr_to_sdr",
        surface = "main-settings-window",
        label_id = "settings.render.hdr_to_sdr.hdr_reference_peak_nits.label",
        label_ru = "HDR reference peak",
        description_id = "settings.render.hdr_to_sdr.hdr_reference_peak_nits.description",
        description_ru = "HDR reference peak в nits; должен быть выше SDR reference white.",
        editor = "float",
        min = crate::validation::MIN_HDR_TO_SDR_REFERENCE_NITS,
        max = crate::validation::MAX_HDR_TO_SDR_REFERENCE_NITS,
        step = 10.0,
        unit = "nits",
        apply = "render.preview"
    )]
    pub hdr_reference_peak_nits: f32,
}

impl Default for HdrToSdrConfig {
    /// Возвращает documented Phase 10 defaults.
    fn default() -> Self {
        Self {
            enabled: true,
            operator: HdrToSdrOperatorConfig::Bt2446C,
            sdr_reference_white_nits: 100.0,
            hdr_reference_peak_nits: 1_000.0,
        }
    }
}

/// HDR-to-SDR operator, разрешённый публичной TOML-схемой.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrToSdrOperatorConfig {
    /// ITU-R BT.2446 Method C.
    Bt2446C,
}

impl Default for HdrToSdrOperatorConfig {
    /// Phase 10 не предлагает альтернативные tone mapping operators.
    fn default() -> Self {
        Self::Bt2446C
    }
}

/// Читает новый table config и старый scalar placeholder `render.hdr_to_sdr`.
fn deserialize_hdr_to_sdr_config<'de, D>(deserializer: D) -> Result<HdrToSdrConfig, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum HdrToSdrConfigCompatibility {
        /// Новая Phase 10 TOML-таблица.
        Table(HdrToSdrConfig),

        /// Старый Phase 8.5 scalar был placeholder-ом и не нёс production-семантики.
        LegacyScalar(bool),
    }

    match HdrToSdrConfigCompatibility::deserialize(deserializer)? {
        HdrToSdrConfigCompatibility::Table(config) => Ok(config),
        HdrToSdrConfigCompatibility::LegacyScalar(_legacy_enabled) => Ok(HdrToSdrConfig::default()),
    }
}

/// Профиль renderer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderProfile {
    /// Автоматический выбор renderer-а.
    Auto,

    /// Vulkan/wgpu profile.
    Vulkan,

    /// OpenGL ES fallback profile.
    #[serde(rename = "opengles")]
    OpenGles,
}

/// Режим tone mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToneMappingMode {
    /// Автоматический выбор алгоритма.
    Auto,

    /// Tone mapping отключён.
    Disabled,
}

/// Пользовательские SDR/RGB корректировки с identity defaults.
///
/// RGB-массивы хранятся как `Vec<f32>`, чтобы validation-слой мог выдать
/// понятную ошибку для неверной длины, а не прятать её внутри Serde parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct RenderColorAdjustmentConfig {
    /// Аддитивное смещение яркости; `0.0` не меняет картинку.
    #[setting(
        id = "render.color_adjustment.brightness",
        path = "render.color_adjustment.brightness",
        section = "render",
        group = "color",
        surface = "main-settings-window",
        label_id = "settings.render.color_adjustment.brightness.label",
        label_ru = "Яркость",
        description_id = "settings.render.color_adjustment.brightness.description",
        description_ru = "Аддитивное смещение яркости SDR shader-а.",
        editor = "float",
        min = crate::validation::MIN_RENDER_COLOR_BRIGHTNESS,
        max = crate::validation::MAX_RENDER_COLOR_BRIGHTNESS,
        step = 0.01,
        apply = "render.preview"
    )]
    pub brightness: f32,

    /// Множитель контраста; `1.0` не меняет картинку.
    #[setting(
        id = "render.color_adjustment.contrast",
        path = "render.color_adjustment.contrast",
        section = "render",
        group = "color",
        surface = "main-settings-window",
        label_id = "settings.render.color_adjustment.contrast.label",
        label_ru = "Контраст",
        description_id = "settings.render.color_adjustment.contrast.description",
        description_ru = "Множитель контраста вокруг нейтральной середины.",
        editor = "float",
        min = crate::validation::MIN_RENDER_COLOR_CONTRAST,
        max = crate::validation::MAX_RENDER_COLOR_CONTRAST,
        step = 0.01,
        apply = "render.preview"
    )]
    pub contrast: f32,

    /// Множитель насыщенности; `1.0` не меняет картинку.
    #[setting(
        id = "render.color_adjustment.saturation",
        path = "render.color_adjustment.saturation",
        section = "render",
        group = "color",
        surface = "main-settings-window",
        label_id = "settings.render.color_adjustment.saturation.label",
        label_ru = "Насыщенность",
        description_id = "settings.render.color_adjustment.saturation.description",
        description_ru = "Множитель насыщенности SDR изображения.",
        editor = "float",
        min = crate::validation::MIN_RENDER_COLOR_SATURATION,
        max = crate::validation::MAX_RENDER_COLOR_SATURATION,
        step = 0.01,
        apply = "render.preview"
    )]
    pub saturation: f32,

    /// Exposure offset для будущего SDR/HDR pipeline; `0.0` не меняет картинку.
    #[setting(
        id = "render.color_adjustment.exposure",
        path = "render.color_adjustment.exposure",
        section = "render",
        group = "color",
        surface = "main-settings-window",
        label_id = "settings.render.color_adjustment.exposure.label",
        label_ru = "Exposure",
        description_id = "settings.render.color_adjustment.exposure.description",
        description_ru = "Exposure offset для SDR/HDR pipeline.",
        editor = "float",
        min = crate::validation::MIN_RENDER_COLOR_EXPOSURE,
        max = crate::validation::MAX_RENDER_COLOR_EXPOSURE,
        step = 0.01,
        apply = "render.preview"
    )]
    pub exposure: f32,

    /// Поканальный RGB gain в порядке R, G, B.
    #[setting(
        id = "render.color_adjustment.rgb_gain",
        path = "render.color_adjustment.rgb_gain",
        section = "render",
        group = "color",
        surface = "main-settings-window",
        label_id = "settings.render.color_adjustment.rgb_gain.label",
        label_ru = "RGB gain",
        description_id = "settings.render.color_adjustment.rgb_gain.description",
        description_ru = "Поканальный RGB gain в порядке R, G, B.",
        editor = "vector",
        min = crate::validation::MIN_RENDER_RGB_GAIN,
        max = crate::validation::MAX_RENDER_RGB_GAIN,
        step = 0.01,
        expected_len = crate::validation::RGB_CHANNEL_COUNT,
        vector_labels("Красный", "Зелёный", "Синий"),
        apply = "render.preview"
    )]
    pub rgb_gain: Vec<f32>,

    /// Поканальный RGB offset в порядке R, G, B.
    #[setting(
        id = "render.color_adjustment.rgb_offset",
        path = "render.color_adjustment.rgb_offset",
        section = "render",
        group = "color",
        surface = "main-settings-window",
        label_id = "settings.render.color_adjustment.rgb_offset.label",
        label_ru = "RGB offset",
        description_id = "settings.render.color_adjustment.rgb_offset.description",
        description_ru = "Поканальный RGB offset в порядке R, G, B.",
        editor = "vector",
        min = crate::validation::MIN_RENDER_RGB_OFFSET,
        max = crate::validation::MAX_RENDER_RGB_OFFSET,
        step = 0.01,
        expected_len = crate::validation::RGB_CHANNEL_COUNT,
        vector_labels("Красный", "Зелёный", "Синий"),
        apply = "render.preview"
    )]
    pub rgb_offset: Vec<f32>,
}

impl RenderColorAdjustmentConfig {
    /// Возвращает `true`, если корректировки не должны менять SDR output.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0
            && self.contrast == 1.0
            && self.saturation == 1.0
            && self.exposure == 0.0
            && self.rgb_gain == [1.0, 1.0, 1.0]
            && self.rgb_offset == [0.0, 0.0, 0.0]
    }
}

impl Default for RenderColorAdjustmentConfig {
    /// Возвращает defaults, которые сохраняют текущую SDR картинку.
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            exposure: 0.0,
            rgb_gain: vec![1.0, 1.0, 1.0],
            rgb_offset: vec![0.0, 0.0, 0.0],
        }
    }
}

/// Vulkan-specific настройки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct VulkanConfig {
    /// Present mode swapchain.
    #[setting(
        id = "render.vulkan.present_mode",
        path = "render.vulkan.present_mode",
        section = "render",
        group = "vulkan",
        surface = "main-settings-window",
        label_id = "settings.render.vulkan.present_mode.label",
        label_ru = "Vulkan present mode",
        description_id = "settings.render.vulkan.present_mode.description",
        description_ru = "Present mode swapchain для Vulkan/wgpu renderer.",
        editor = "select",
        apply = "render.apply",
        options(
            option(id = "auto", label_id = "settings.render.vulkan.present_mode.auto", label_ru = "Авто", value = VulkanPresentMode::Auto),
            option(id = "fifo", label_id = "settings.render.vulkan.present_mode.fifo", label_ru = "FIFO/VSync", value = VulkanPresentMode::Fifo),
            option(id = "mailbox", label_id = "settings.render.vulkan.present_mode.mailbox", label_ru = "Mailbox", value = VulkanPresentMode::Mailbox),
            option(id = "immediate", label_id = "settings.render.vulkan.present_mode.immediate", label_ru = "Immediate", value = VulkanPresentMode::Immediate),
        )
    )]
    pub present_mode: VulkanPresentMode,

    /// Максимальная задержка кадра в render backend.
    #[setting(
        id = "render.vulkan.max_frame_latency",
        path = "render.vulkan.max_frame_latency",
        section = "render",
        group = "vulkan",
        surface = "main-settings-window",
        label_id = "settings.render.vulkan.max_frame_latency.label",
        label_ru = "Max frame latency",
        description_id = "settings.render.vulkan.max_frame_latency.description",
        description_ru = "Максимальная задержка кадра в render backend.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_VULKAN_FRAME_LATENCY,
        step = 1,
        unit = "frames",
        apply = "render.apply"
    )]
    pub max_frame_latency: u32,
}

impl Default for VulkanConfig {
    /// Возвращает VSync-friendly настройки.
    fn default() -> Self {
        Self {
            present_mode: VulkanPresentMode::Fifo,
            max_frame_latency: 2,
        }
    }
}

/// Пользовательское имя present mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VulkanPresentMode {
    /// Автоматический выбор доступного present mode.
    Auto,

    /// VSync/FIFO.
    Fifo,

    /// Low-latency mailbox, если backend поддерживает.
    Mailbox,

    /// Immediate без VSync.
    Immediate,
}

/// OpenGL ES fallback-настройки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct OpenGlesConfig {
    /// Разрешает будущий OpenGL ES renderer.
    #[setting(
        id = "render.opengles.enabled",
        path = "render.opengles.enabled",
        section = "render",
        group = "opengles",
        surface = "main-settings-window",
        label_id = "settings.render.opengles.enabled.label",
        label_ru = "OpenGL ES renderer",
        description_id = "settings.render.opengles.enabled.description",
        description_ru = "Разрешает будущий OpenGL ES fallback renderer.",
        editor = "toggle",
        apply = "render.apply"
    )]
    pub enabled: bool,

    /// Включает упрощённый UI для слабого renderer-а.
    #[setting(
        id = "render.opengles.simple_ui",
        path = "render.opengles.simple_ui",
        section = "render",
        group = "opengles",
        surface = "main-settings-window",
        label_id = "settings.render.opengles.simple_ui.label",
        label_ru = "Упрощённый UI",
        description_id = "settings.render.opengles.simple_ui.description",
        description_ru = "Включает упрощённый UI для слабого renderer-а.",
        editor = "toggle",
        apply = "render.apply"
    )]
    pub simple_ui: bool,
}

impl Default for OpenGlesConfig {
    /// Возвращает disabled fallback для Vulkan-first MVP.
    fn default() -> Self {
        Self {
            enabled: false,
            simple_ui: true,
        }
    }
}
