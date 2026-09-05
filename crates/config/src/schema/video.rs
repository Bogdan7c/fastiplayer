use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, Visitor},
};

/// Decode-настройки video pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct VideoConfig {
    /// Предпочитаемый decode backend.
    #[setting(
        id = "video.preferred_backend",
        path = "video.preferred_backend",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.preferred_backend.label",
        label_ru = "Decode backend",
        description_id = "settings.video.preferred_backend.description",
        description_ru = "Предпочитаемый video decode path: auto, native hardware или software FFmpeg.",
        editor = "select",
        apply = "video.apply",
        options(
            option(id = "auto", label_id = "settings.video.preferred_backend.auto", label_ru = "Авто", value = VideoBackendPreference::Auto),
            option(id = "hardware", label_id = "settings.video.preferred_backend.hardware", label_ru = "Hardware", value = VideoBackendPreference::Hardware),
            option(id = "software", label_id = "settings.video.preferred_backend.software", label_ru = "Software FFmpeg", value = VideoBackendPreference::Software),
        )
    )]
    pub preferred_backend: VideoBackendPreference,

    /// Максимальный video decode-ahead относительно audio clock.
    #[setting(
        id = "video.max_decode_ahead_ms",
        path = "video.max_decode_ahead_ms",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.max_decode_ahead_ms.label",
        label_ru = "Максимальный decode-ahead",
        description_id = "settings.video.max_decode_ahead_ms.description",
        description_ru = "Максимальный video decode-ahead относительно audio clock.",
        editor = "integer",
        min = crate::validation::MIN_DECODE_AHEAD_MS,
        max = crate::validation::MAX_DECODE_AHEAD_MS,
        step = 10,
        unit = "ms",
        apply = "video.apply"
    )]
    pub max_decode_ahead_ms: u64,

    /// Максимум decoded frames в presentation queue.
    #[setting(
        id = "video.present_queue_frames",
        path = "video.present_queue_frames",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.present_queue_frames.label",
        label_ru = "Presentation queue",
        description_id = "settings.video.present_queue_frames.description",
        description_ru = "Максимум decoded frames в presentation queue.",
        editor = "integer",
        min = crate::validation::MIN_PRESENT_QUEUE_FRAMES,
        max = crate::validation::MAX_PRESENT_QUEUE_FRAMES,
        step = 1,
        unit = "frames",
        apply = "video.apply"
    )]
    pub present_queue_frames: usize,

    /// Bounded packet channel между worker и decoder thread.
    #[setting(
        id = "video.decoder_packet_channel_frames",
        path = "video.decoder_packet_channel_frames",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.decoder_packet_channel_frames.label",
        label_ru = "Очередь decoder packets",
        description_id = "settings.video.decoder_packet_channel_frames.description",
        description_ru = "Bounded packet channel между worker и decoder thread.",
        editor = "integer",
        min = crate::validation::MIN_DECODER_QUEUE_FRAMES,
        max = crate::validation::MAX_DECODER_QUEUE_FRAMES,
        step = 1,
        unit = "frames",
        apply = "video.apply"
    )]
    pub decoder_packet_channel_frames: usize,

    /// Bounded decoded frame channel между decoder thread и worker.
    #[setting(
        id = "video.decoder_frame_channel_frames",
        path = "video.decoder_frame_channel_frames",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.decoder_frame_channel_frames.label",
        label_ru = "Очередь decoded frames",
        description_id = "settings.video.decoder_frame_channel_frames.description",
        description_ru = "Bounded decoded frame channel между decoder thread и worker.",
        editor = "integer",
        min = crate::validation::MIN_DECODER_QUEUE_FRAMES,
        max = crate::validation::MAX_DECODER_QUEUE_FRAMES,
        step = 1,
        unit = "frames",
        apply = "video.apply"
    )]
    pub decoder_frame_channel_frames: usize,

    /// Backend-local ready queue для burst `FrameReady` events.
    #[setting(
        id = "video.decoder_ready_queue_frames",
        path = "video.decoder_ready_queue_frames",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.decoder_ready_queue_frames.label",
        label_ru = "Ready queue frames",
        description_id = "settings.video.decoder_ready_queue_frames.description",
        description_ru = "Backend-local ready queue для burst FrameReady events.",
        editor = "integer",
        min = crate::validation::MIN_DECODER_QUEUE_FRAMES,
        max = crate::validation::MAX_DECODER_QUEUE_FRAMES,
        step = 1,
        unit = "frames",
        apply = "video.apply"
    )]
    pub decoder_ready_queue_frames: usize,

    /// Количество VA output surface descriptors для hardware decoder-а.
    #[setting(
        id = "video.decoder_surface_pool_frames",
        path = "video.decoder_surface_pool_frames",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.decoder_surface_pool_frames.label",
        label_ru = "Decoder surface pool",
        description_id = "settings.video.decoder_surface_pool_frames.description",
        description_ru = "Количество VA output surface descriptors для hardware decoder.",
        editor = "integer",
        min = crate::validation::MIN_DECODER_QUEUE_FRAMES,
        max = crate::validation::MAX_DECODER_SURFACE_POOL_FRAMES,
        step = 1,
        unit = "frames",
        apply = "video.apply"
    )]
    pub decoder_surface_pool_frames: usize,

    /// Output frame pool size для software (FFmpeg host-frame) decode.
    #[setting(
        id = "video.sw_decoder_surface_pool_frames",
        path = "video.sw_decoder_surface_pool_frames",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.sw_decoder_surface_pool_frames.label",
        label_ru = "Software frame pool",
        description_id = "settings.video.sw_decoder_surface_pool_frames.description",
        description_ru = "Сколько декодированных software-кадров (в RAM) держать одновременно. Влияет только на software-декод (FFmpeg). Компромисс: меньше (6) = меньше нагрузка на память и плавнее FPS на лёгких для декода кодеках (AV1/VP9 4K); больше (8) = запас для тяжёлых кодеков (HEVC 4K), которым иначе не хватает кадров и FPS проседает. Применяется на лету. По умолчанию 8.",
        editor = "integer",
        min = crate::validation::MIN_DECODER_QUEUE_FRAMES,
        max = crate::validation::MAX_DECODER_SURFACE_POOL_FRAMES,
        step = 1,
        unit = "frames",
        apply = "video.apply"
    )]
    pub sw_decoder_surface_pool_frames: usize,

    /// Лимит потоков software (FFmpeg) декода; 0 = auto (ядра − 2, минимум 2).
    #[setting(
        id = "video.sw_decode_threads",
        path = "video.sw_decode_threads",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.sw_decode_threads.label",
        label_ru = "Software decode потоки",
        description_id = "settings.video.sw_decode_threads.description",
        description_ru = "Сколько потоков отдавать software-декодеру (FFmpeg). 0 = авто: все ядра минус 2 — запас, чтобы render/upload поток не голодал и FPS рендера оставался стабильным. Полный набор ядер (например, 8 из 8) ускоряет чистый декод, но вытесняет рендер и даёт рывки на 4K60. Применяется на лету.",
        editor = "integer",
        min = crate::validation::MIN_SW_DECODE_THREADS,
        max = crate::validation::MAX_SW_DECODE_THREADS,
        step = 1,
        unit = "threads",
        apply = "video.apply"
    )]
    pub sw_decode_threads: usize,

    /// Количество zero-copy external import slots.
    #[setting(
        id = "video.zero_copy_surface_pool_slots",
        path = "video.zero_copy_surface_pool_slots",
        section = "video",
        group = "decode",
        surface = "main-settings-window",
        label_id = "settings.video.zero_copy_surface_pool_slots.label",
        label_ru = "Zero-copy import slots",
        description_id = "settings.video.zero_copy_surface_pool_slots.description",
        description_ru = "Количество zero-copy external import slots.",
        editor = "integer",
        min = crate::validation::MIN_DECODER_QUEUE_FRAMES,
        max = crate::validation::MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
        step = 1,
        unit = "slots",
        apply = "video.apply"
    )]
    pub zero_copy_surface_pool_slots: usize,

    /// Настройки worker scheduler-а и bounded catch-up policy.
    #[setting(nested)]
    pub scheduler: VideoSchedulerConfig,
}

impl Default for VideoConfig {
    /// Возвращает текущие MVP-лимиты video backpressure.
    fn default() -> Self {
        Self {
            preferred_backend: VideoBackendPreference::Auto,
            max_decode_ahead_ms: 500,
            present_queue_frames: 8,
            decoder_packet_channel_frames: 32,
            decoder_frame_channel_frames: 8,
            decoder_ready_queue_frames: 8,
            decoder_surface_pool_frames: 24,
            sw_decoder_surface_pool_frames: 8,
            sw_decode_threads: 0,
            zero_copy_surface_pool_slots: 24,
            scheduler: VideoSchedulerConfig::default(),
        }
    }
}

/// Scheduler-настройки video pipeline без codec/backend-specific имён.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct VideoSchedulerConfig {
    /// Базовый budget чтения container packets за один worker tick.
    #[setting(
        id = "video.scheduler.demux_packets_per_tick",
        path = "video.scheduler.demux_packets_per_tick",
        section = "video",
        group = "scheduler",
        surface = "main-settings-window",
        label_id = "settings.video.scheduler.demux_packets_per_tick.label",
        label_ru = "Demux packets за tick",
        description_id = "settings.video.scheduler.demux_packets_per_tick.description",
        description_ru = "Базовый budget чтения container packets за один worker tick.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_SCHEDULER_DEMUX_PACKETS_PER_TICK,
        step = 1,
        unit = "packets",
        apply = "video.apply"
    )]
    pub demux_packets_per_tick: usize,

    /// Базовый budget отправки video packets в decoder thread за tick.
    #[setting(
        id = "video.scheduler.video_packets_per_tick",
        path = "video.scheduler.video_packets_per_tick",
        section = "video",
        group = "scheduler",
        surface = "main-settings-window",
        label_id = "settings.video.scheduler.video_packets_per_tick.label",
        label_ru = "Video packets за tick",
        description_id = "settings.video.scheduler.video_packets_per_tick.description",
        description_ru = "Базовый budget отправки video packets в decoder thread за tick.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_SCHEDULER_VIDEO_PACKETS_PER_TICK,
        step = 1,
        unit = "packets",
        apply = "video.apply"
    )]
    pub video_packets_per_tick: usize,

    /// Базовый budget приёма decoded frames из decoder thread за tick.
    #[setting(
        id = "video.scheduler.decoded_frames_per_tick",
        path = "video.scheduler.decoded_frames_per_tick",
        section = "video",
        group = "scheduler",
        surface = "main-settings-window",
        label_id = "settings.video.scheduler.decoded_frames_per_tick.label",
        label_ru = "Decoded frames за tick",
        description_id = "settings.video.scheduler.decoded_frames_per_tick.description",
        description_ru = "Базовый budget приёма decoded frames из decoder thread за tick.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_SCHEDULER_DECODED_FRAMES_PER_TICK,
        step = 1,
        unit = "frames",
        apply = "video.apply"
    )]
    pub decoded_frames_per_tick: usize,

    /// Дополнительное bounded окно catch-up work после обычного tick.
    #[setting(
        id = "video.scheduler.catch_up_budget_ms",
        path = "video.scheduler.catch_up_budget_ms",
        section = "video",
        group = "scheduler",
        surface = "main-settings-window",
        label_id = "settings.video.scheduler.catch_up_budget_ms.label",
        label_ru = "Catch-up budget",
        description_id = "settings.video.scheduler.catch_up_budget_ms.description",
        description_ru = "Дополнительное bounded окно catch-up work после обычного tick.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_SCHEDULER_CATCH_UP_BUDGET_MS,
        step = 1,
        unit = "ms",
        apply = "video.apply"
    )]
    pub catch_up_budget_ms: u64,

    /// Минимальный запас ready frames, ниже которого pipeline считается starvation-prone.
    #[setting(
        id = "video.scheduler.present_queue_min_frames",
        path = "video.scheduler.present_queue_min_frames",
        section = "video",
        group = "scheduler",
        surface = "main-settings-window",
        label_id = "settings.video.scheduler.present_queue_min_frames.label",
        label_ru = "Минимум ready frames",
        description_id = "settings.video.scheduler.present_queue_min_frames.description",
        description_ru = "Минимальный запас ready frames; верхняя граница дополнительно зависит от video.present_queue_frames.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_PRESENT_QUEUE_FRAMES,
        step = 1,
        unit = "frames",
        apply = "video.apply"
    )]
    pub present_queue_min_frames: usize,

    /// Целевой запас ready frames; max задаётся `VideoConfig::present_queue_frames`.
    #[setting(
        id = "video.scheduler.present_queue_target_frames",
        path = "video.scheduler.present_queue_target_frames",
        section = "video",
        group = "scheduler",
        surface = "main-settings-window",
        label_id = "settings.video.scheduler.present_queue_target_frames.label",
        label_ru = "Цель ready frames",
        description_id = "settings.video.scheduler.present_queue_target_frames.description",
        description_ru = "Целевой запас ready frames; cross-field min/max проверяет AppConfig::validate().",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_PRESENT_QUEUE_FRAMES,
        step = 1,
        unit = "frames",
        apply = "video.apply"
    )]
    pub present_queue_target_frames: usize,

    /// Целевой decode-ahead; max задаётся `VideoConfig::max_decode_ahead_ms`.
    #[setting(
        id = "video.scheduler.decode_ahead_target_ms",
        path = "video.scheduler.decode_ahead_target_ms",
        section = "video",
        group = "scheduler",
        surface = "main-settings-window",
        label_id = "settings.video.scheduler.decode_ahead_target_ms.label",
        label_ru = "Целевой decode-ahead",
        description_id = "settings.video.scheduler.decode_ahead_target_ms.description",
        description_ru = "Целевой decode-ahead; max дополнительно ограничен video.max_decode_ahead_ms.",
        editor = "integer",
        min = crate::validation::MIN_DECODE_AHEAD_MS,
        max = crate::validation::MAX_DECODE_AHEAD_MS,
        step = 10,
        unit = "ms",
        apply = "video.apply"
    )]
    pub decode_ahead_target_ms: u64,

    /// Минимальный резерв свободных zero-copy surface/import slots перед decode.
    #[setting(
        id = "video.scheduler.surface_free_slots_min",
        path = "video.scheduler.surface_free_slots_min",
        section = "video",
        group = "scheduler",
        surface = "main-settings-window",
        label_id = "settings.video.scheduler.surface_free_slots_min.label",
        label_ru = "Минимум свободных slots",
        description_id = "settings.video.scheduler.surface_free_slots_min.description",
        description_ru = "Минимальный резерв свободных zero-copy surface/import slots.",
        editor = "integer",
        min = 0,
        max = crate::validation::MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
        step = 1,
        unit = "slots",
        apply = "video.apply"
    )]
    pub surface_free_slots_min: usize,

    /// Целевой резерв свободных zero-copy surface/import slots для catch-up.
    #[setting(
        id = "video.scheduler.surface_free_slots_target",
        path = "video.scheduler.surface_free_slots_target",
        section = "video",
        group = "scheduler",
        surface = "main-settings-window",
        label_id = "settings.video.scheduler.surface_free_slots_target.label",
        label_ru = "Цель свободных slots",
        description_id = "settings.video.scheduler.surface_free_slots_target.description",
        description_ru = "Целевой резерв свободных zero-copy slots; cross-field min проверяет AppConfig::validate().",
        editor = "integer",
        min = 0,
        max = crate::validation::MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
        step = 1,
        unit = "slots",
        apply = "video.apply"
    )]
    pub surface_free_slots_target: usize,
}

impl Default for VideoSchedulerConfig {
    /// Возвращает bounded budgets scheduler-а без привязки к display/video FPS.
    fn default() -> Self {
        Self {
            demux_packets_per_tick: 12,
            video_packets_per_tick: 8,
            decoded_frames_per_tick: 8,
            catch_up_budget_ms: 4,
            present_queue_min_frames: 2,
            present_queue_target_frames: 4,
            decode_ahead_target_ms: 250,
            surface_free_slots_min: 2,
            surface_free_slots_target: 4,
        }
    }
}

/// Выбор decode backend из пользовательского config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoBackendPreference {
    /// Автоматически выбрать лучший доступный backend.
    Auto,

    /// Native hardware decode path; сейчас это VA-API, но TOML value не привязан к VA-API навсегда.
    Hardware,

    /// Только FFmpeg software decode path.
    Software,
}

const SUPPORTED_VIDEO_BACKEND_PREFERENCE_VALUES: &[&str] = &["auto", "hardware", "software"];
const LEGACY_VAAPI_VIDEO_BACKEND_PREFERENCE: &str = "vaapi";
const REMOVED_VULKAN_VIDEO_BACKEND_PREFERENCE: &str = "vulkan";

// Ручной Deserialize нужен, чтобы v2 `vaapi` нормализовался в `hardware`,
// удалённый `vulkan` получил точную подсказку, а остальные неизвестные id
// остались обычной schema error от Serde.
impl<'de> Deserialize<'de> for VideoBackendPreference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(VideoBackendPreferenceVisitor)
    }
}

struct VideoBackendPreferenceVisitor;

impl<'de> Visitor<'de> for VideoBackendPreferenceVisitor {
    type Value = VideoBackendPreference;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("video.preferred_backend value \"auto\", \"hardware\" or \"software\"")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match value {
            "auto" => Ok(VideoBackendPreference::Auto),
            "hardware" => Ok(VideoBackendPreference::Hardware),
            "software" => Ok(VideoBackendPreference::Software),
            LEGACY_VAAPI_VIDEO_BACKEND_PREFERENCE => Ok(VideoBackendPreference::Hardware),
            REMOVED_VULKAN_VIDEO_BACKEND_PREFERENCE => Err(E::custom(
                "video.preferred_backend = \"vulkan\" удалён; замените его на \"auto\", \
                 чтобы Fastiplayer выбрал поддерживаемый backend, или на \"hardware\", чтобы \
                 явно требовать native hardware decode",
            )),
            unknown_value => Err(E::unknown_variant(
                unknown_value,
                SUPPORTED_VIDEO_BACKEND_PREFERENCE_VALUES,
            )),
        }
    }
}
