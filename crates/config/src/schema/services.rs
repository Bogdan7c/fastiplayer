use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ConfigResult;

/// Сколько аудио (мс) плеер может прочитать вперёд, пока ждёт первый видеокадр, по умолчанию.
pub const DEFAULT_AUDIO_STARTUP_READ_AHEAD_CAP_MS: u64 = 1_500;
/// Минимум стартового потолка: меньше обычного `audio.buffer_target_ms` (200 мс) смысла нет.
pub const MIN_AUDIO_STARTUP_READ_AHEAD_CAP_MS: u64 = 200;
/// Максимум стартового потолка. Кольцевой буфер audio output вмещает ~2000 мс PCM
/// (`audio::output`), поэтому потолок оставляет запас ~200 мс на последний декодированный пакет.
pub const MAX_AUDIO_STARTUP_READ_AHEAD_CAP_MS: u64 = 1_800;

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
            Self::Zero => formatter.write_str("preferred video height must be greater than zero"),
            Self::TooLarge {
                provided_pixels,
                maximum_pixels,
            } => write!(
                formatter,
                "preferred video height {provided_pixels} exceeds maximum {maximum_pixels}"
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
    /// Громкость пользователя в диапазоне `0.0..=1.0` (решение владельца, UX-11).
    ///
    /// Это последняя слышимая громкость: плеер записывает её сюда сам (с паузой
    /// после изменения и при выходе) и восстанавливает при следующем запуске.
    /// При mute здесь остаётся громкость «до mute», а сам mute хранится в `muted`.
    #[setting(
        id = "audio.volume",
        path = "audio.volume",
        section = "audio",
        group = "output",
        surface = "main-settings-window",
        label_id = "settings.audio.volume.label",
        label_ru = "Volume",
        description_id = "settings.audio.volume.description",
        description_ru = "Playback volume. The player saves it automatically and restores it on the next launch.",
        help_id = "settings.audio.volume.help",
        help_ru = "Changes from the player volume slider are saved after a few seconds and on exit. Apply here also changes the current volume immediately.",
        editor = "float",
        min = crate::validation::MIN_AUDIO_VOLUME,
        max = crate::validation::MAX_AUDIO_VOLUME,
        step = 0.01,
        unit = "ratio",
        apply = "audio.apply"
    )]
    pub volume: f64,

    /// Выключен ли звук (mute); громкость при этом хранится отдельно в `volume`.
    #[setting(
        id = "audio.muted",
        path = "audio.muted",
        section = "audio",
        group = "output",
        surface = "main-settings-window",
        label_id = "settings.audio.muted.label",
        label_ru = "The sound's off.",
        description_id = "settings.audio.muted.description",
        description_ru = "Mute. The player saves this automatically; unmuting restores the saved volume.",
        editor = "toggle",
        apply = "audio.apply"
    )]
    pub muted: bool,

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
        description_ru = "Stable audio output device ID; `default` means the system default device.",
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
        description_ru = "Target audio buffer high-water mark.",
        editor = "integer",
        min = crate::validation::MIN_AUDIO_BUFFER_TARGET_MS,
        max = crate::validation::MAX_AUDIO_BUFFER_TARGET_MS,
        step = 10,
        unit = "ms",
        apply = "audio.apply"
    )]
    pub buffer_target_ms: u64,

    /// Потолок чтения аудио вперёд, пока ещё нет первого видеокадра.
    #[setting(
        id = "audio.startup_read_ahead_cap_ms",
        path = "audio.startup_read_ahead_cap_ms",
        section = "audio",
        group = "buffer",
        surface = "main-settings-window",
        label_id = "settings.audio.startup_read_ahead_cap_ms.label",
        label_ru = "Audio read-ahead while waiting for the first frame",
        description_id = "settings.audio.startup_read_ahead_cap_ms.description",
        description_ru = "How much audio may be read ahead at startup while the first video frame is still being decoded.",
        help_id = "settings.audio.startup_read_ahead_cap_ms.help",
        help_ru = "Video with B-frames needs a dozen packets before the first picture appears; if the audio buffer limit stops reading earlier, playback never starts. After the first frame the normal audio buffer target applies again.",
        editor = "integer",
        min = crate::MIN_AUDIO_STARTUP_READ_AHEAD_CAP_MS,
        max = crate::MAX_AUDIO_STARTUP_READ_AHEAD_CAP_MS,
        step = 100,
        unit = "ms",
        apply = "audio.apply"
    )]
    pub startup_read_ahead_cap_ms: u64,
}

impl Default for AudioConfig {
    /// Возвращает комфортные audio defaults.
    fn default() -> Self {
        Self {
            volume: 0.8,
            muted: false,
            output_device: "default".to_string(),
            buffer_target_ms: 200,
            startup_read_ahead_cap_ms: DEFAULT_AUDIO_STARTUP_READ_AHEAD_CAP_MS,
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
        description_ru = "RAM cache size; 0 explicitly disables RAM caching.",
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
        description_ru = "RAM window that prefetch keeps ahead of the foreground cursor.",
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
        description_ru = "Size of the first background prefetch read after open/seek.",
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
        description_ru = "Maximum size of one background prefetch read.",
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
        description_ru = "Network source connection timeout.",
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
        description_ru = "Network source read timeout.",
        editor = "integer",
        min = crate::validation::MIN_POSITIVE_U64_SETTING_VALUE,
        max = crate::validation::MAX_POSITIVE_U64_SETTING_VALUE,
        step = 100,
        unit = "ms",
        apply = "network.apply"
    )]
    pub read_timeout_ms: u64,

    /// Сколько ждать восстановления связи, если сеть пропала посреди просмотра.
    ///
    /// Пока идёт ожидание, плеер повторяет запрос с того же места с растущими паузами
    /// (0,5 → 8 с) и показывает буферизацию; после бюджета показывает ошибку.
    /// `0` отключает переподключение: первая сетевая ошибка сразу останавливает просмотр.
    #[setting(
        id = "network.reconnect_wait_ms",
        path = "network.reconnect_wait_ms",
        section = "network",
        group = "timeout",
        surface = "main-settings-window",
        label_id = "settings.network.reconnect_wait_ms.label",
        label_ru = "Network recovery wait",
        description_id = "settings.network.reconnect_wait_ms.description",
        description_ru = "How long to wait for the network to return after an interruption during playback; 0 means no waiting.",
        editor = "integer",
        min = 0,
        max = crate::validation::MAX_NETWORK_RECONNECT_WAIT_MS,
        step = 1000,
        unit = "ms",
        apply = "network.apply"
    )]
    pub reconnect_wait_ms: u64,
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
            reconnect_wait_ms: 60_000,
        }
    }
}

/// Пользовательская provider-neutral политика выбора dynamic range для web media.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebMediaHdrSelection {
    /// Выбирать только кандидаты с достоверно подтверждённым SDR dynamic range.
    #[default]
    SdrOnly,

    /// Сначала искать playable HDR, затем автоматически переходить к лучшему SDR.
    #[serde(rename = "prefer_hdr")]
    PreferHdrWhenAvailable,
}

/// Provider-neutral policy выбора и восстановления web media.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebMediaConfig {
    /// Политика выбора SDR/HDR stream-а до открытия media bytes.
    pub hdr_selection: WebMediaHdrSelection,

    /// Глобальная preferred height; `None` сохраняет обычный `BestPlayable`.
    pub preferred_video_height: Option<PreferredVideoHeight>,

    /// Разрешает автоматический candidate-level reopen после expiry VOD endpoint-а.
    pub vod_endpoint_recovery_enabled: bool,

    /// Максимум последовательных recovery attempts до typed terminal failure.
    pub vod_endpoint_recovery_max_consecutive_attempts: u64,

    /// Начальная задержка exponential backoff перед повторным source resolution.
    pub vod_endpoint_recovery_initial_backoff_ms: u64,

    /// Верхняя граница exponential backoff между recovery attempts.
    pub vod_endpoint_recovery_max_backoff_ms: u64,

    /// Stable playback interval, после которого consecutive budget сбрасывается.
    pub vod_endpoint_recovery_stable_reset_ms: u64,
}

impl WebMediaConfig {
    /// Проверяет provider-neutral web-media policy независимо от полного `AppConfig`.
    pub fn validate(&self) -> ConfigResult<()> {
        crate::validation::validate_web_media_config(self)
    }
}

impl Default for WebMediaConfig {
    /// Возвращает безопасную policy для всех web-media providers.
    fn default() -> Self {
        Self {
            hdr_selection: WebMediaHdrSelection::SdrOnly,
            preferred_video_height: None,
            vod_endpoint_recovery_enabled: true,
            vod_endpoint_recovery_max_consecutive_attempts: 3,
            vod_endpoint_recovery_initial_backoff_ms: 250,
            vod_endpoint_recovery_max_backoff_ms: 2_000,
            vod_endpoint_recovery_stable_reset_ms: 30_000,
        }
    }
}

/// Process controls extractor adapter-а `yt-dlp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YtDlpConfig {
    /// Разрешает YtDlp adapter.
    pub enabled: bool,

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
            resolve_timeout_ms: 30_000,
            single_item_stdout_limit_bytes: 64 * 1024 * 1024,
            single_item_stderr_limit_bytes: 8 * 1024 * 1024,
            single_item_json_node_limit: 1_000_000,
        }
    }
}
