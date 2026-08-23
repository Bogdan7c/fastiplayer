use std::fmt;

use serde::{Deserialize, Serialize};

use super::YtDlpHdrSelection;
use crate::ConfigResult;

/// Верхняя граница persisted preference, синхронизированная app boundary с `web-media-core`.
pub const MAX_PREFERRED_VIDEO_HEIGHT: u32 = 16_384;

/// Проверенная глобальная высота video representation в пикселях.
///
/// Тип принадлежит config crate и намеренно ничего не знает о feature-level
/// `web_media_core::VideoHeight`; их связывает только composition root приложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct PreferredVideoHeight(u32);

impl PreferredVideoHeight {
    /// Создаёт preference только для поддерживаемого ненулевого диапазона.
    pub const fn new(pixels: u32) -> Result<Self, PreferredVideoHeightError> {
        if pixels == 0 {
            return Err(PreferredVideoHeightError::Zero);
        }
        if pixels > MAX_PREFERRED_VIDEO_HEIGHT {
            return Err(PreferredVideoHeightError::TooLarge {
                provided_pixels: pixels,
                maximum_pixels: MAX_PREFERRED_VIDEO_HEIGHT,
            });
        }
        Ok(Self(pixels))
    }

    /// Возвращает проверенную высоту в пикселях.
    #[must_use]
    pub const fn pixels(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for PreferredVideoHeight {
    type Error = PreferredVideoHeightError;

    /// Валидирует raw TOML/settings значение на config boundary.
    fn try_from(pixels: u32) -> Result<Self, Self::Error> {
        Self::new(pixels)
    }
}

impl From<PreferredVideoHeight> for u32 {
    /// Возвращает scalar для стабильной TOML-сериализации newtype-а.
    fn from(height: PreferredVideoHeight) -> Self {
        height.pixels()
    }
}

/// Ошибка config-owned проверки preferred video height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredVideoHeightError {
    /// Ноль не описывает video representation и не используется как скрытый `None`.
    Zero,
    /// Значение превышает именованную compatibility-границу.
    TooLarge {
        /// Полученное значение.
        provided_pixels: u32,
        /// Максимально допустимое значение.
        maximum_pixels: u32,
    },
}

impl fmt::Display for PreferredVideoHeightError {
    /// Форматирует безопасную ошибку без media locator-ов или других secrets.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => {
                formatter.write_str("предпочитаемая высота видео должна быть больше нуля")
            }
            Self::TooLarge {
                provided_pixels,
                maximum_pixels,
            } => write!(
                formatter,
                "предпочитаемая высота видео {provided_pixels} превышает максимум {maximum_pixels}"
            ),
        }
    }
}

impl std::error::Error for PreferredVideoHeightError {}

/// Настройки аудио.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    /// Начальная громкость в диапазоне `0.0..=1.0`.
    #[setting(
        id = "audio.volume",
        path = "audio.volume",
        section = "audio",
        group = "output",
        surface = "main-settings-window",
        label_id = "settings.audio.volume.label",
        label_ru = "Громкость по умолчанию",
        description_id = "settings.audio.volume.description",
        description_ru = "Стартовая громкость для нового media, не текущая runtime громкость.",
        help_id = "settings.audio.volume.help",
        help_ru = "Изменение применяется как default volume после Apply и не перезаписывает текущую громкость воспроизведения.",
        editor = "float",
        min = crate::validation::MIN_AUDIO_VOLUME,
        max = crate::validation::MAX_AUDIO_VOLUME,
        step = 0.01,
        unit = "ratio",
        apply = "audio.apply"
    )]
    pub volume: f64,

    /// Имя audio output device или `default`.
    #[setting(
        id = "audio.output_device",
        path = "audio.output_device",
        section = "audio",
        group = "output",
        surface = "main-settings-window",
        label_id = "settings.audio.output_device.label",
        label_ru = "Audio output device",
        description_id = "settings.audio.output_device.description",
        description_ru = "Stable id audio output device; `default` означает системное устройство.",
        editor = "select",
        option_provider = "audio.output_device",
        apply = "audio.apply"
    )]
    pub output_device: String,

    /// Целевой high-water mark audio buffer.
    #[setting(
        id = "audio.buffer_target_ms",
        path = "audio.buffer_target_ms",
        section = "audio",
        group = "buffer",
        surface = "main-settings-window",
        label_id = "settings.audio.buffer_target_ms.label",
        label_ru = "Audio buffer target",
        description_id = "settings.audio.buffer_target_ms.description",
        description_ru = "Целевой high-water mark audio buffer.",
        editor = "integer",
        min = crate::validation::MIN_AUDIO_BUFFER_TARGET_MS,
        max = crate::validation::MAX_AUDIO_BUFFER_TARGET_MS,
        step = 10,
        unit = "ms",
        apply = "audio.apply"
    )]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    /// Размер RAM cache; `0` явно отключает RAM cache.
    #[setting(
        id = "network.memory_cache_mb",
        path = "network.memory_cache_mb",
        section = "network",
        group = "cache",
        surface = "main-settings-window",
        label_id = "settings.network.memory_cache_mb.label",
        label_ru = "RAM cache",
        description_id = "settings.network.memory_cache_mb.description",
        description_ru = "Размер RAM cache; 0 явно отключает RAM cache.",
        editor = "integer",
        min = 0,
        max = crate::validation::MAX_NETWORK_MEMORY_CACHE_MB,
        step = 1,
        unit = "mb",
        apply = "network.apply"
    )]
    pub memory_cache_mb: u64,

    /// RAM window, которое prefetch держит впереди foreground cursor-а.
    #[setting(
        id = "network.read_ahead_mb",
        path = "network.read_ahead_mb",
        section = "network",
        group = "prefetch",
        surface = "main-settings-window",
        label_id = "settings.network.read_ahead_mb.label",
        label_ru = "Read-ahead window",
        description_id = "settings.network.read_ahead_mb.description",
        description_ru = "RAM window, которое prefetch держит впереди foreground cursor-а.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_NETWORK_READ_AHEAD_MB,
        step = 1,
        unit = "mb",
        apply = "network.apply"
    )]
    pub read_ahead_mb: u64,

    /// Размер ПЕРВОГО фонового prefetch-чтения после open/seek, в КиБ.
    #[setting(
        id = "network.prefetch_initial_chunk_kb",
        path = "network.prefetch_initial_chunk_kb",
        section = "network",
        group = "prefetch",
        surface = "main-settings-window",
        label_id = "settings.network.prefetch_initial_chunk_kb.label",
        label_ru = "Initial prefetch chunk",
        description_id = "settings.network.prefetch_initial_chunk_kb.description",
        description_ru = "Размер первого фонового prefetch-чтения после open/seek.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_NETWORK_PREFETCH_INITIAL_CHUNK_KB,
        step = 1,
        unit = "kb",
        apply = "network.apply"
    )]
    pub prefetch_initial_chunk_kb: u64,

    /// Максимальный размер одного фонового prefetch-чтения.
    #[setting(
        id = "network.prefetch_chunk_mb",
        path = "network.prefetch_chunk_mb",
        section = "network",
        group = "prefetch",
        surface = "main-settings-window",
        label_id = "settings.network.prefetch_chunk_mb.label",
        label_ru = "Prefetch chunk",
        description_id = "settings.network.prefetch_chunk_mb.description",
        description_ru = "Максимальный размер одного фонового prefetch-чтения.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_NETWORK_READ_AHEAD_MB,
        step = 1,
        unit = "mb",
        apply = "network.apply"
    )]
    pub prefetch_chunk_mb: u64,

    /// Timeout подключения к сетевому источнику.
    #[setting(
        id = "network.connect_timeout_ms",
        path = "network.connect_timeout_ms",
        section = "network",
        group = "timeout",
        surface = "main-settings-window",
        label_id = "settings.network.connect_timeout_ms.label",
        label_ru = "Connect timeout",
        description_id = "settings.network.connect_timeout_ms.description",
        description_ru = "Timeout подключения к сетевому источнику.",
        editor = "integer",
        min = crate::validation::MIN_POSITIVE_U64_SETTING_VALUE,
        max = crate::validation::MAX_POSITIVE_U64_SETTING_VALUE,
        step = 100,
        unit = "ms",
        apply = "network.apply"
    )]
    pub connect_timeout_ms: u64,

    /// Timeout чтения из сетевого источника.
    #[setting(
        id = "network.read_timeout_ms",
        path = "network.read_timeout_ms",
        section = "network",
        group = "timeout",
        surface = "main-settings-window",
        label_id = "settings.network.read_timeout_ms.label",
        label_ru = "Read timeout",
        description_id = "settings.network.read_timeout_ms.description",
        description_ru = "Timeout чтения из сетевого источника.",
        editor = "integer",
        min = crate::validation::MIN_POSITIVE_U64_SETTING_VALUE,
        max = crate::validation::MAX_POSITIVE_U64_SETTING_VALUE,
        step = 100,
        unit = "ms",
        apply = "network.apply"
    )]
    pub read_timeout_ms: u64,
}

impl Default for NetworkConfig {
    /// Возвращает conservative cache defaults.
    fn default() -> Self {
        Self {
            memory_cache_mb: 128,
            read_ahead_mb: 256,
            prefetch_initial_chunk_kb: 64,
            prefetch_chunk_mb: 8,
            connect_timeout_ms: 15_000,
            read_timeout_ms: 15_000,
        }
    }
}

/// Настройки YtDlp/service слоя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YtDlpConfig {
    /// Разрешает YtDlp adapter.
    pub enabled: bool,

    /// Политика выбора SDR/HDR stream-а до открытия media bytes.
    pub hdr_selection: YtDlpHdrSelection,

    /// Глобальная preferred height; `None` сохраняет обычный `BestPlayable`.
    pub preferred_video_height: Option<PreferredVideoHeight>,

    /// Максимальное время подготовки direct stream metadata через `yt-dlp`.
    pub resolve_timeout_ms: u64,

    /// Максимальный stdout одного single-item extraction до немедленного terminate/reap.
    pub single_item_stdout_limit_bytes: u64,

    /// Максимальный stderr одного single-item extraction без хранения payload.
    pub single_item_stderr_limit_bytes: u64,

    /// Максимальное число JSON values до построения metadata DOM.
    pub single_item_json_node_limit: u64,
}

impl YtDlpConfig {
    /// Проверяет runtime-значения YtDlp независимо от полного `AppConfig`.
    pub fn validate(&self) -> ConfigResult<()> {
        crate::validation::validate_yt_dlp_config(self)
    }
}

impl Default for YtDlpConfig {
    /// Возвращает включённый service adapter для текущего приложения.
    fn default() -> Self {
        Self {
            enabled: true,
            hdr_selection: YtDlpHdrSelection::SdrOnly,
            preferred_video_height: None,
            resolve_timeout_ms: 30_000,
            single_item_stdout_limit_bytes: 64 * 1024 * 1024,
            single_item_stderr_limit_bytes: 8 * 1024 * 1024,
            single_item_json_node_limit: 1_000_000,
        }
    }
}
