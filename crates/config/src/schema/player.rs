use serde::{Deserialize, Serialize};

/// Настройки поведения player layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerConfig {
    /// Открывать media в паузе.
    #[setting(
        id = "player.start_paused",
        path = "player.start_paused",
        section = "player",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.player.start_paused.label",
        label_ru = "Start paused",
        description_id = "settings.player.start_paused.description",
        description_ru = "Open new media paused.",
        editor = "toggle",
        apply = "player.apply"
    )]
    pub start_paused: bool,

    /// Включает persistent checkpoint и startup restore текущего playlist media.
    #[setting(
        id = "player.resume_last_position",
        path = "player.resume_last_position",
        section = "player",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.player.resume_last_position.label",
        label_ru = "Restore position",
        description_id = "settings.player.resume_last_position.description",
        description_ru = "Save the confirmed position of the current media and restore it on the next launch.",
        editor = "toggle",
        apply = "player.apply"
    )]
    pub resume_last_position: bool,

    /// Настройки seek/scrub поведения.
    #[setting(nested)]
    pub seek: PlayerSeekConfig,

    /// Настройки demuxer fail-safe поведения.
    #[setting(nested)]
    pub demux: PlayerDemuxConfig,

    /// Приоритет codec candidates при выборе video stream.
    #[setting(
        id = "player.preferred_video_codec_order",
        path = "player.preferred_video_codec_order",
        section = "player",
        group = "codec",
        surface = "main-settings-window",
        label_id = "settings.player.preferred_video_codec_order.label",
        label_ru = "Video codec priority",
        description_id = "settings.player.preferred_video_codec_order.description",
        description_ru = "Ordered list of codec IDs for video stream selection.",
        help_id = "settings.player.preferred_video_codec_order.help",
        help_ru = "Each codec ID must appear at most once; the first supported option has higher priority.",
        editor = "select_list",
        min_len = 1,
        max_len = 5,
        apply = "player.apply",
        options(
            option(id = "vp9", label_id = "settings.codec.vp9", label_ru = "VP9", value = VideoCodec::Vp9),
            option(id = "av1", label_id = "settings.codec.av1", label_ru = "AV1", value = VideoCodec::Av1),
            option(id = "h264", label_id = "settings.codec.h264", label_ru = "H.264", value = VideoCodec::H264),
            option(id = "h265", label_id = "settings.codec.h265", label_ru = "H.265", value = VideoCodec::H265),
            option(id = "vp8", label_id = "settings.codec.vp8", label_ru = "VP8", value = VideoCodec::Vp8),
        )
    )]
    pub preferred_video_codec_order: Vec<VideoCodec>,
}

impl Default for PlayerConfig {
    /// Возвращает поведение первого запуска.
    fn default() -> Self {
        Self {
            // Открытый файл сразу играет, как в привычных плеерах; пауза при
            // открытии — опция для тех, кто её включит. Меняется только default
            // новых config-ов: явно записанное `true` у прежних установок не
            // мигрируется (решение владельца 2026-10-11).
            start_paused: false,
            resume_last_position: true,
            seek: PlayerSeekConfig::default(),
            demux: PlayerDemuxConfig::default(),
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

/// Настройки fail-safe поведения demuxer-а.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerDemuxConfig {
    /// Сколько corrupted packets подряд demuxer может пропустить до fatal ошибки.
    #[setting(
        id = "player.demux.max_consecutive_corrupted_packets",
        path = "player.demux.max_consecutive_corrupted_packets",
        section = "player",
        group = "demux",
        surface = "main-settings-window",
        label_id = "settings.player.demux.max_consecutive_corrupted_packets.label",
        label_ru = "Corrupted packet limit",
        description_id = "settings.player.demux.max_consecutive_corrupted_packets.description",
        description_ru = "Maximum consecutive corrupted packets to skip before a fatal error.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_CONSECUTIVE_CORRUPTED_PACKETS,
        step = 1,
        unit = "packets",
        apply = "player.apply"
    )]
    pub max_consecutive_corrupted_packets: usize,
}

impl Default for PlayerDemuxConfig {
    /// Возвращает осторожный default, который переживает короткие повреждённые участки.
    fn default() -> Self {
        Self {
            max_consecutive_corrupted_packets: 64,
        }
    }
}

/// Настройки seek commit, resume после seek и hotkey-шагов.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerSeekConfig {
    /// Timeout финального commit-а seek/scrub.
    #[setting(
        id = "player.seek.commit_timeout_ms",
        path = "player.seek.commit_timeout_ms",
        section = "player",
        group = "seek",
        surface = "main-settings-window",
        label_id = "settings.player.seek.commit_timeout_ms.label",
        label_ru = "Timeout seek commit",
        description_id = "settings.player.seek.commit_timeout_ms.description",
        description_ru = "Maximum time for a final seek/scrub commit.",
        editor = "integer",
        min = crate::validation::MIN_POSITIVE_U64_SETTING_VALUE,
        max = crate::validation::MAX_POSITIVE_U64_SETTING_VALUE,
        step = 100,
        unit = "ms",
        apply = "player.apply"
    )]
    pub commit_timeout_ms: u64,

    /// Минимальный audio buffer перед resume после commit-а.
    #[setting(
        id = "player.seek.resume_audio_min_buffer_ms",
        path = "player.seek.resume_audio_min_buffer_ms",
        section = "player",
        group = "seek",
        surface = "main-settings-window",
        label_id = "settings.player.seek.resume_audio_min_buffer_ms.label",
        label_ru = "Minimum audio buffer",
        description_id = "settings.player.seek.resume_audio_min_buffer_ms.description",
        description_ru = "Minimum audio buffer before resuming after a seek commit.",
        editor = "integer",
        min = crate::validation::MIN_POSITIVE_U64_SETTING_VALUE,
        max = crate::validation::MAX_POSITIVE_U64_SETTING_VALUE,
        step = 10,
        unit = "ms",
        apply = "player.apply"
    )]
    pub resume_audio_min_buffer_ms: u64,

    /// Soft timeout audio gate-а после показанного target video frame.
    #[setting(
        id = "player.seek.resume_audio_gate_timeout_ms",
        path = "player.seek.resume_audio_gate_timeout_ms",
        section = "player",
        group = "seek",
        surface = "main-settings-window",
        label_id = "settings.player.seek.resume_audio_gate_timeout_ms.label",
        label_ru = "Timeout audio gate",
        description_id = "settings.player.seek.resume_audio_gate_timeout_ms.description",
        description_ru = "Soft audio gate timeout after presenting the target video frame.",
        editor = "integer",
        min = crate::validation::MIN_POSITIVE_U64_SETTING_VALUE,
        max = crate::validation::MAX_POSITIVE_U64_SETTING_VALUE,
        step = 10,
        unit = "ms",
        apply = "player.apply"
    )]
    pub resume_audio_gate_timeout_ms: u64,

    /// Минимальный запас готовых video frames перед resume после commit-а.
    #[setting(
        id = "player.seek.resume_video_min_ready_frames",
        path = "player.seek.resume_video_min_ready_frames",
        section = "player",
        group = "seek",
        surface = "main-settings-window",
        label_id = "settings.player.seek.resume_video_min_ready_frames.label",
        label_ru = "Ready video frames before resume",
        description_id = "settings.player.seek.resume_video_min_ready_frames.description",
        description_ru = "Minimum decoded frame reserve before resuming after a seek commit.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_SEEK_RESUME_VIDEO_READY_FRAMES,
        step = 1,
        unit = "frames",
        apply = "player.apply"
    )]
    pub resume_video_min_ready_frames: usize,

    /// Bounded окно worker work для accurate seek decode-preroll до target frame.
    #[setting(
        id = "player.seek.fast_preroll_budget_ms",
        path = "player.seek.fast_preroll_budget_ms",
        section = "player",
        group = "seek",
        surface = "main-settings-window",
        label_id = "settings.player.seek.fast_preroll_budget_ms.label",
        label_ru = "Budget fast preroll",
        description_id = "settings.player.seek.fast_preroll_budget_ms.description",
        description_ru = "Bounded worker time window for accurate seek decode preroll.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_SEEK_FAST_PREROLL_BUDGET_MS,
        step = 1,
        unit = "ms",
        apply = "player.apply"
    )]
    pub fast_preroll_budget_ms: u64,

    /// Burst-лимит video packets/frames для accurate seek GOP preroll.
    #[setting(
        id = "player.seek.fast_preroll_video_packet_burst",
        path = "player.seek.fast_preroll_video_packet_burst",
        section = "player",
        group = "seek",
        surface = "main-settings-window",
        label_id = "settings.player.seek.fast_preroll_video_packet_burst.label",
        label_ru = "Preroll packet burst",
        description_id = "settings.player.seek.fast_preroll_video_packet_burst.description",
        description_ru = "Video packet/frame limit for accurate seek GOP preroll.",
        editor = "integer",
        min = 1,
        max = crate::validation::MAX_SEEK_FAST_PREROLL_VIDEO_PACKET_BURST,
        step = 1,
        unit = "packets",
        apply = "player.apply"
    )]
    pub fast_preroll_video_packet_burst: usize,

    /// Поведение commit-а, если playback был на паузе.
    #[setting(
        id = "player.seek.paused_commit_behavior",
        path = "player.seek.paused_commit_behavior",
        section = "player",
        group = "seek",
        surface = "main-settings-window",
        label_id = "settings.player.seek.paused_commit_behavior.label",
        label_ru = "Seek behavior while paused",
        description_id = "settings.player.seek.paused_commit_behavior.description",
        description_ru = "What to do after a seek commit that started while paused.",
        editor = "select",
        apply = "player.apply",
        options(
            option(id = "stay_paused", label_id = "settings.player.seek.paused_commit_behavior.stay_paused", label_ru = "Stay paused", value = PausedCommitBehavior::StayPaused),
        )
    )]
    pub paused_commit_behavior: PausedCommitBehavior,

    /// Малый шаг горячих клавиш seek.
    #[setting(
        id = "player.seek.hotkey_small_step_secs",
        path = "player.seek.hotkey_small_step_secs",
        section = "player",
        group = "seek",
        surface = "main-settings-window",
        label_id = "settings.player.seek.hotkey_small_step_secs.label",
        label_ru = "Small seek step",
        description_id = "settings.player.seek.hotkey_small_step_secs.description",
        description_ru = "Small seek hotkey step in seconds.",
        editor = "integer",
        min = crate::validation::MIN_POSITIVE_U64_SETTING_VALUE,
        max = crate::validation::MAX_POSITIVE_U64_SETTING_VALUE,
        step = 1,
        unit = "seconds",
        apply = "player.apply"
    )]
    pub hotkey_small_step_secs: u64,

    /// Большой шаг горячих клавиш seek.
    #[setting(
        id = "player.seek.hotkey_large_step_secs",
        path = "player.seek.hotkey_large_step_secs",
        section = "player",
        group = "seek",
        surface = "main-settings-window",
        label_id = "settings.player.seek.hotkey_large_step_secs.label",
        label_ru = "Large seek step",
        description_id = "settings.player.seek.hotkey_large_step_secs.description",
        description_ru = "Large seek hotkey step in seconds.",
        editor = "integer",
        min = crate::validation::MIN_POSITIVE_U64_SETTING_VALUE,
        max = crate::validation::MAX_POSITIVE_U64_SETTING_VALUE,
        step = 1,
        unit = "seconds",
        apply = "player.apply"
    )]
    pub hotkey_large_step_secs: u64,
}

impl Default for PlayerSeekConfig {
    /// Возвращает documented defaults seek-поведения.
    fn default() -> Self {
        Self {
            commit_timeout_ms: 10_000,
            resume_audio_min_buffer_ms: 50,
            resume_audio_gate_timeout_ms: 250,
            resume_video_min_ready_frames: 3,
            fast_preroll_budget_ms: 48,
            fast_preroll_video_packet_burst: 512,
            paused_commit_behavior: PausedCommitBehavior::StayPaused,
            hotkey_small_step_secs: 5,
            hotkey_large_step_secs: 30,
        }
    }
}

/// Политика playback state после seek commit, начатого из paused состояния.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PausedCommitBehavior {
    /// Оставаться на паузе после commit-а.
    StayPaused,
}

impl Default for PausedCommitBehavior {
    /// По умолчанию seek из паузы не запускает playback.
    fn default() -> Self {
        Self::StayPaused
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
