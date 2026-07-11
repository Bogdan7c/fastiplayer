use serde::{Deserialize, Serialize};

use super::YoutubeHdrSelection;

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

/// Настройки YouTube/service слоя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct YoutubeConfig {
    /// Разрешает YouTube adapter.
    #[setting(
        id = "youtube.enabled",
        path = "youtube.enabled",
        section = "youtube",
        group = "service",
        surface = "main-settings-window",
        label_id = "settings.youtube.enabled.label",
        label_ru = "YouTube adapter",
        description_id = "settings.youtube.enabled.description",
        description_ru = "Разрешает YouTube service adapter.",
        editor = "toggle",
        apply = "youtube.apply"
    )]
    pub enabled: bool,

    /// Предпочитать account/session cookies, если service adapter их поддерживает.
    #[setting(
        id = "youtube.prefer_account_session",
        path = "youtube.prefer_account_session",
        section = "youtube",
        group = "service",
        surface = "main-settings-window",
        label_id = "settings.youtube.prefer_account_session.label",
        label_ru = "Использовать account session",
        description_id = "settings.youtube.prefer_account_session.description",
        description_ru = "Предпочитать account/session cookies, если adapter их поддерживает.",
        editor = "toggle",
        apply = "youtube.apply"
    )]
    pub prefer_account_session: bool,

    /// Политика выбора SDR/HDR stream-а до открытия media bytes.
    #[setting(
        id = "youtube.hdr_selection",
        path = "youtube.hdr_selection",
        section = "youtube",
        group = "service",
        surface = "main-settings-window",
        label_id = "settings.youtube.hdr_selection.label",
        label_ru = "Динамический диапазон YouTube",
        description_id = "settings.youtube.hdr_selection.description",
        description_ru = "Выбирать только SDR или предпочитать HDR при полной поддержке decoder и renderer с автоматическим SDR fallback.",
        editor = "select",
        apply = "youtube.apply",
        options(
            option(id = "sdr_only", label_id = "settings.youtube.hdr_selection.sdr_only", label_ru = "Только SDR", value = YoutubeHdrSelection::SdrOnly),
            option(id = "prefer_hdr", label_id = "settings.youtube.hdr_selection.prefer_hdr", label_ru = "Предпочитать HDR", value = YoutubeHdrSelection::PreferHdrWhenAvailable),
        )
    )]
    pub hdr_selection: YoutubeHdrSelection,

    /// Максимальное время подготовки direct stream metadata через `yt-dlp`.
    #[setting(
        id = "youtube.resolve_timeout_ms",
        path = "youtube.resolve_timeout_ms",
        section = "youtube",
        group = "service",
        surface = "main-settings-window",
        label_id = "settings.youtube.resolve_timeout_ms.label",
        label_ru = "YouTube resolve timeout",
        description_id = "settings.youtube.resolve_timeout_ms.description",
        description_ru = "Максимальное время подготовки direct stream metadata через yt-dlp.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_YOUTUBE_RESOLVE_TIMEOUT_MS,
        step = 100,
        unit = "ms",
        apply = "youtube.apply"
    )]
    pub resolve_timeout_ms: u64,
}

impl Default for YoutubeConfig {
    /// Возвращает включённый service adapter для текущего приложения.
    fn default() -> Self {
        Self {
            enabled: true,
            prefer_account_session: true,
            hdr_selection: YoutubeHdrSelection::SdrOnly,
            resolve_timeout_ms: 30_000,
        }
    }
}
