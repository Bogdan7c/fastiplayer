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
        description_ru = "Preferred video decode path: automatic, native hardware, or software FFmpeg.",
        editor = "select",
        apply = "video.apply",
        options(
            option(id = "auto", label_id = "settings.video.preferred_backend.auto", label_ru = "Automatic", value = VideoBackendPreference::Auto),
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
        label_ru = "Maximum decode-ahead",
        description_id = "settings.video.max_decode_ahead_ms.description",
        description_ru = "Maximum video decode-ahead relative to the audio clock.",
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
        description_ru = "Maximum decoded frames in the presentation queue.",
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
        label_ru = "Decoder packet queue",
        description_id = "settings.video.decoder_packet_channel_frames.description",
        description_ru = "Bounded packet channel between the worker and decoder thread.",
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
        label_ru = "Decoded frame queue",
        description_id = "settings.video.decoder_frame_channel_frames.description",
        description_ru = "Bounded decoded frame channel between the decoder thread and worker.",
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
        description_ru = "Backend-local ready queue for bursts of FrameReady events.",
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
        description_ru = "Number of VA output surface descriptors for the hardware decoder.",
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
        description_ru = "Number of decoded software frames (in RAM) to hold at once. Applies only to software decoding (FFmpeg). Tradeoff: fewer (6) reduces memory load and gives smoother FPS for codecs that are easy to decode (AV1/VP9 4K); more (8) provides a reserve for demanding codecs (HEVC 4K) that would otherwise run out of frames and drop FPS. Applies during playback. Default: 8.",
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
        label_ru = "Software decode threads",
        description_id = "settings.video.sw_decode_threads.description",
        description_ru = "Threads assigned to the software decoder (FFmpeg). 0 = automatic: all cores minus 2, leaving capacity for rendering/upload to maintain stable render FPS. Using all cores (for example, 8 of 8) speeds up decoding but competes with rendering and causes stuttering at 4K60. Applies during playback.",
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
        description_ru = "Number of zero-copy external import slots.",
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
        label_ru = "Demux packets per tick",
        description_id = "settings.video.scheduler.demux_packets_per_tick.description",
        description_ru = "Base container packet read budget per worker tick.",
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
        label_ru = "Video packets per tick",
        description_id = "settings.video.scheduler.video_packets_per_tick.description",
        description_ru = "Base video packet submission budget to the decoder thread per tick.",
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
        label_ru = "Decoded frames per tick",
        description_id = "settings.video.scheduler.decoded_frames_per_tick.description",
        description_ru = "Base decoded frame receive budget from the decoder thread per tick.",
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
        description_ru = "Additional bounded catch-up work window after a normal tick.",
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
        label_ru = "Minimum ready frames",
        description_id = "settings.video.scheduler.present_queue_min_frames.description",
        description_ru = "Minimum ready frame reserve; the upper limit also depends on video.present_queue_frames.",
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
        label_ru = "Target ready frames",
        description_id = "settings.video.scheduler.present_queue_target_frames.description",
        description_ru = "Target ready frame reserve; AppConfig::validate() checks cross-field minimum/maximum limits.",
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
        label_ru = "Target decode-ahead",
        description_id = "settings.video.scheduler.decode_ahead_target_ms.description",
        description_ru = "Target decode-ahead; the maximum is also limited by video.max_decode_ahead_ms.",
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
        label_ru = "Minimum free slots",
        description_id = "settings.video.scheduler.surface_free_slots_min.description",
        description_ru = "Minimum free zero-copy surface/import slot reserve.",
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
        label_ru = "Target free slots",
        description_id = "settings.video.scheduler.surface_free_slots_target.description",
        description_ru = "Target free zero-copy slot reserve; AppConfig::validate() checks the cross-field minimum.",
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
                "video.preferred_backend = \"vulkan\" was removed; replace it with \"auto\" so Fastiplayer can select a supported backend, or \"hardware\" to explicitly require native hardware decoding",
            )),
            unknown_value => Err(E::unknown_variant(
                unknown_value,
                SUPPORTED_VIDEO_BACKEND_PREFERENCE_VALUES,
            )),
        }
    }
}
