use serde::{Deserialize, Deserializer, Serialize};

use crate::{ConfigResult, validation};

/// Текущая версия TOML-схемы.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Полная пользовательская конфигурация приложения.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Версия TOML-схемы; обязательна для будущих миграций config.
    pub schema_version: u32,

    /// Поведение playback state machine и выбора потоков.
    #[serde(default)]
    pub player: PlayerConfig,

    /// Decode-ограничения и backend preference.
    #[serde(default)]
    pub video: VideoConfig,

    /// Render-профиль и backend-specific настройки.
    #[serde(default)]
    pub render: RenderConfig,

    /// Настройки аудиовыхода.
    #[serde(default)]
    pub audio: AudioConfig,

    /// Настройки сетевого read-ahead/cache слоя.
    #[serde(default)]
    pub network: NetworkConfig,

    /// Настройки YouTube/service слоя.
    #[serde(default)]
    pub youtube: YoutubeConfig,

    /// Настройки shell UI.
    #[serde(default)]
    pub ui: UiConfig,
}

impl AppConfig {
    /// Проверяет значения, которые Serde не может выразить типами.
    pub fn validate(&self) -> ConfigResult<()> {
        validation::validate_app_config(self)
    }

    /// Сериализует config в читаемый TOML для записи default-файла.
    pub fn to_pretty_toml(&self) -> ConfigResult<String> {
        let mut toml_text = toml::to_string_pretty(self)
            .map_err(|source| crate::ConfigError::SerializeDefaultConfig { source })?;

        if !toml_text.ends_with('\n') {
            toml_text.push('\n');
        }

        Ok(toml_text)
    }
}

impl Default for AppConfig {
    /// Возвращает production defaults для первого запуска без config-файла.
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            player: PlayerConfig::default(),
            video: VideoConfig::default(),
            render: RenderConfig::default(),
            audio: AudioConfig::default(),
            network: NetworkConfig::default(),
            youtube: YoutubeConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

/// Настройки поведения player layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerConfig {
    /// Открывать media в паузе.
    pub start_paused: bool,

    /// В будущем восстанавливать позицию из storage.
    pub resume_last_position: bool,

    /// Приоритет codec candidates при выборе video stream.
    pub preferred_video_codec_order: Vec<VideoCodec>,
}

impl Default for PlayerConfig {
    /// Возвращает безопасное поведение первого запуска.
    fn default() -> Self {
        Self {
            start_paused: true,
            resume_last_position: true,
            preferred_video_codec_order: vec![
                VideoCodec::Vp9,
                VideoCodec::Av1,
                VideoCodec::H264,
                VideoCodec::H265,
                VideoCodec::Vp8,
            ],
        }
    }
}

/// Поддерживаемые имена video codec в config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    /// VP9.
    Vp9,

    /// AV1.
    Av1,

    /// H.264/AVC.
    H264,

    /// H.265/HEVC.
    H265,

    /// VP8.
    Vp8,
}

/// Decode-настройки video pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoConfig {
    /// Запрещает silent fallback на software video decode.
    pub hardware_decode_only: bool,

    /// Предпочитаемый decode backend.
    pub preferred_backend: VideoBackendPreference,

    /// Максимальный video decode-ahead относительно audio clock.
    pub max_decode_ahead_ms: u64,

    /// Максимум decoded frames в presentation queue.
    pub present_queue_frames: usize,
}

impl Default for VideoConfig {
    /// Возвращает текущие MVP-лимиты video backpressure.
    fn default() -> Self {
        Self {
            hardware_decode_only: true,
            preferred_backend: VideoBackendPreference::Auto,
            max_decode_ahead_ms: 500,
            present_queue_frames: 8,
        }
    }
}

/// Выбор decode backend из пользовательского config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoBackendPreference {
    /// Автоматически выбрать лучший доступный backend.
    Auto,

    /// VA-API hardware decode.
    Vaapi,

    /// Vulkan-oriented decode path текущего MVP.
    Vulkan,
}

/// Render-настройки верхнего уровня.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RenderConfig {
    /// Активный render profile.
    pub profile: RenderProfile,

    /// Typed HDR-to-SDR baseline config для Phase 10.
    #[serde(default, deserialize_with = "deserialize_hdr_to_sdr_config")]
    pub hdr_to_sdr: HdrToSdrConfig,

    /// Compatibility placeholder для будущего HDR tone mapping; Phase 8.5 держит `Disabled`.
    pub tone_mapping: ToneMappingMode,

    /// Пользовательские SDR/RGB корректировки без HDR controls.
    pub color_adjustment: RenderColorAdjustmentConfig,

    /// Vulkan-specific параметры.
    pub vulkan: VulkanConfig,

    /// OpenGL ES fallback-параметры.
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HdrToSdrConfig {
    /// Разрешает HDR-to-SDR path, если renderer capabilities тоже подтверждают support.
    pub enabled: bool,

    /// Единственный production operator Phase 10.
    pub operator: HdrToSdrOperatorConfig,

    /// SDR reference white в nits для BT.2446-C.
    pub sdr_reference_white_nits: f32,

    /// HDR reference peak в nits для BT.2446-C.
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RenderColorAdjustmentConfig {
    /// Аддитивное смещение яркости; `0.0` не меняет картинку.
    pub brightness: f32,

    /// Множитель контраста; `1.0` не меняет картинку.
    pub contrast: f32,

    /// Множитель насыщенности; `1.0` не меняет картинку.
    pub saturation: f32,

    /// Exposure offset для будущего SDR/HDR pipeline; `0.0` не меняет картинку.
    pub exposure: f32,

    /// Поканальный RGB gain в порядке R, G, B.
    pub rgb_gain: Vec<f32>,

    /// Поканальный RGB offset в порядке R, G, B.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VulkanConfig {
    /// Present mode swapchain.
    pub present_mode: VulkanPresentMode,

    /// Максимальная задержка кадра в render backend.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenGlesConfig {
    /// Разрешает будущий OpenGL ES renderer.
    pub enabled: bool,

    /// Включает упрощённый UI для слабого renderer-а.
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

/// Настройки аудио.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    /// Начальная громкость в диапазоне `0.0..=1.0`.
    pub volume: f64,

    /// Имя audio output device или `default`.
    pub output_device: String,

    /// Целевой high-water mark audio buffer.
    pub buffer_target_ms: u64,
}

impl Default for AudioConfig {
    /// Возвращает комфортные audio defaults.
    fn default() -> Self {
        Self {
            volume: 0.8,
            output_device: "default".to_string(),
            buffer_target_ms: 200,
        }
    }
}

/// Настройки network/cache слоя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    /// Включает будущий cache layer.
    pub cache_enabled: bool,

    /// Максимальный read-ahead для сетевых источников.
    pub max_read_ahead_mb: u64,
}

impl Default for NetworkConfig {
    /// Возвращает conservative cache defaults.
    fn default() -> Self {
        Self {
            cache_enabled: true,
            max_read_ahead_mb: 256,
        }
    }
}

/// Настройки YouTube/service слоя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YoutubeConfig {
    /// Разрешает YouTube adapter.
    pub enabled: bool,

    /// Предпочитать account/session cookies, когда storage layer появится.
    pub prefer_account_session: bool,
}

impl Default for YoutubeConfig {
    /// Возвращает включённый service adapter для текущего приложения.
    fn default() -> Self {
        Self {
            enabled: true,
            prefer_account_session: true,
        }
    }
}

/// Настройки UI shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Показывать диагностическую панель telemetry.
    pub show_telemetry: bool,

    /// Язык UI.
    pub language: String,
}

impl Default for UiConfig {
    /// Возвращает русскоязычный UI по умолчанию.
    fn default() -> Self {
        Self {
            show_telemetry: true,
            language: "ru".to_string(),
        }
    }
}
